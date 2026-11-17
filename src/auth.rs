//! Login through the house OIDC provider (REQ-AUTH-001) and per-person
//! scoping (REQ-AUTH-002, DD-013).
//!
//! Authorization code flow with PKCE against one issuer. The issuer is the
//! only host lichtbild contacts (SSC-003); its certificate may come from a
//! private CA given as a PEM bundle. Sessions live in memory: a restart
//! costs one more round trip to the provider.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, OAuth2TokenResponse,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use serde::Deserialize;

/// One person: which library is theirs, how the provider names them, and
/// which other libraries they may see (owner decision: own only unless
/// granted here).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub label: String,
    pub oidc_subject: Option<String>,
    pub oidc_email: Option<String>,
    #[serde(default)]
    pub sees: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret_file: PathBuf,
    pub ca_file: Option<PathBuf>,
    /// External base URL, e.g. https://host/lichtbild (no trailing slash).
    pub public_url: String,
}

/// Who is looking: the libraries they may see. Without login configured
/// (operator mode, loopback only) every library is visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub labels: Vec<String>,
    pub name: Option<String>,
}

impl Viewer {
    pub fn may_see(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }
}

#[derive(Debug, Clone)]
struct Session {
    viewer: Viewer,
    expires: Instant,
}

struct Pending {
    verifier: PkceCodeVerifier,
    nonce: Nonce,
    next: String,
    created: Instant,
}

const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
pub const COOKIE: &str = "lichtbild_session";

pub struct Auth {
    settings: OidcSettings,
    secret: String,
    http: openidconnect::reqwest::Client,
    metadata: tokio::sync::Mutex<Option<CoreProviderMetadata>>,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<String, Session>>,
    people: Vec<Person>,
    roots: Vec<String>,
}

/// A `next` target after login: a path inside the app, never another site.
pub fn safe_next(next: &str) -> String {
    let n = next.trim_start_matches('/');
    let ok = !n.contains("://")
        && !n.starts_with('/')
        && !n.contains('\\')
        && !n.contains("..")
        && n.chars().all(|c| !c.is_control());
    if ok {
        n.to_string()
    } else {
        String::new()
    }
}

impl Auth {
    pub fn new(settings: OidcSettings, people: Vec<Person>, roots: Vec<String>) -> Result<Auth> {
        let secret = std::fs::read_to_string(&settings.client_secret_file)
            .with_context(|| "LICHTBILD_OIDC_CLIENT_SECRET_FILE".to_string())?
            .trim()
            .to_string();
        if secret.is_empty() {
            bail!("the OIDC client secret file is empty");
        }
        let mut builder = openidconnect::reqwest::ClientBuilder::new()
            // OIDC endpoints must not redirect (and a redirect could leave
            // the issuer host).
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15));
        if let Some(ca) = &settings.ca_file {
            let pem = std::fs::read(ca).with_context(|| "LICHTBILD_OIDC_CA_FILE".to_string())?;
            for cert in openidconnect::reqwest::Certificate::from_pem_bundle(&pem)
                .context("LICHTBILD_OIDC_CA_FILE: not a PEM bundle")?
            {
                builder = builder.add_root_certificate(cert);
            }
        }
        let http = builder.build().context("HTTP client for the OIDC issuer")?;
        for p in &people {
            if !roots.contains(&p.label) {
                bail!("[[people]] label {:?} is not a configured root", p.label);
            }
            if p.oidc_subject.is_none() && p.oidc_email.is_none() {
                bail!("[[people]] {:?}: set oidc_subject or oidc_email", p.label);
            }
            for s in &p.sees {
                if !roots.contains(s) {
                    bail!(
                        "[[people]] {:?}: sees {:?}, which is not a configured root",
                        p.label,
                        s
                    );
                }
            }
        }
        IssuerUrl::new(settings.issuer.clone()).context("LICHTBILD_OIDC_ISSUER")?;
        RedirectUrl::new(format!("{}/auth/callback", settings.public_url))
            .context("LICHTBILD_PUBLIC_URL")?;
        Ok(Auth {
            settings,
            secret,
            http,
            metadata: tokio::sync::Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            people,
            roots,
        })
    }

    /// Cookies are Secure when the public URL is https.
    pub fn secure_cookies(&self) -> bool {
        self.settings.public_url.starts_with("https://")
    }

    async fn metadata(&self, refresh: bool) -> Result<CoreProviderMetadata> {
        let mut m = self.metadata.lock().await;
        if refresh || m.is_none() {
            let md = CoreProviderMetadata::discover_async(
                IssuerUrl::new(self.settings.issuer.clone())?,
                &self.http,
            )
            .await
            .context("OIDC discovery")?;
            *m = Some(md);
        }
        Ok(m.clone().expect("set above"))
    }

    fn client(
        &self,
        md: CoreProviderMetadata,
    ) -> Result<
        CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
    > {
        Ok(CoreClient::from_provider_metadata(
            md,
            ClientId::new(self.settings.client_id.clone()),
            Some(ClientSecret::new(self.secret.clone())),
        )
        .set_redirect_uri(RedirectUrl::new(format!(
            "{}/auth/callback",
            self.settings.public_url
        ))?))
    }

    /// Start a login: the provider URL to redirect the browser to.
    pub async fn login_url(&self, next: &str) -> Result<String> {
        let client = self.client(self.metadata(false).await?)?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("email".into()))
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(challenge)
            .url();
        let mut p = self.pending.lock().expect("pending lock");
        p.retain(|_, v| v.created.elapsed() < PENDING_TTL);
        p.insert(
            state.secret().clone(),
            Pending {
                verifier,
                nonce,
                next: safe_next(next),
                created: Instant::now(),
            },
        );
        Ok(url.to_string())
    }

    /// Finish a login: exchange the code, verify the ID token, map the
    /// identity to a person. Returns the session id and where to go next.
    pub async fn callback(&self, code: &str, state: &str) -> Result<(String, String), LoginError> {
        let pending = self
            .pending
            .lock()
            .expect("pending lock")
            .remove(state)
            .filter(|p| p.created.elapsed() < PENDING_TTL)
            .ok_or(LoginError::UnknownState)?;
        let md = self.metadata(false).await.map_err(LoginError::Provider)?;
        let client = self.client(md).map_err(LoginError::Provider)?;
        let token = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|e| LoginError::Provider(e.into()))?
            .set_pkce_verifier(pending.verifier)
            .request_async(&self.http)
            .await
            .map_err(|e| LoginError::Provider(anyhow::anyhow!("token exchange: {e}")))?;
        let _ = token.access_token();
        let id_token = token
            .id_token()
            .ok_or_else(|| LoginError::Provider(anyhow::anyhow!("no ID token")))?;
        // Verify signature, issuer, audience, expiry and nonce. On a failure,
        // fetch the keys once more in case the provider rotated them.
        let verifier = client.id_token_verifier();
        let first = id_token.claims(&verifier, &pending.nonce).cloned();
        let claims = match first {
            Ok(c) => c,
            Err(_) => {
                let md = self.metadata(true).await.map_err(LoginError::Provider)?;
                let fresh = self.client(md).map_err(LoginError::Provider)?;
                let verifier = fresh.id_token_verifier();
                id_token
                    .claims(&verifier, &pending.nonce)
                    .map_err(|e| LoginError::InvalidToken(e.to_string()))?
                    .clone()
            }
        };
        let subject = claims.subject().as_str().to_string();
        let email = claims
            .email()
            .filter(|_| claims.email_verified().unwrap_or(false))
            .map(|e| e.as_str().to_ascii_lowercase());
        let viewer = self
            .viewer_for(&subject, email.as_deref())
            .ok_or(LoginError::NotAPerson)?;
        let sid = CsrfToken::new_random_len(32).secret().clone();
        let mut s = self.sessions.lock().expect("sessions lock");
        s.retain(|_, v| v.expires > Instant::now());
        s.insert(
            sid.clone(),
            Session {
                viewer,
                expires: Instant::now() + SESSION_TTL,
            },
        );
        Ok((sid, pending.next))
    }

    /// The person this identity is, by subject first, then verified email.
    pub fn viewer_for(&self, subject: &str, email: Option<&str>) -> Option<Viewer> {
        let p = self
            .people
            .iter()
            .find(|p| p.oidc_subject.as_deref() == Some(subject))
            .or_else(|| {
                let email = email?;
                self.people.iter().find(|p| {
                    p.oidc_email
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(email))
                })
            })?;
        let mut labels = vec![p.label.clone()];
        for s in &p.sees {
            if !labels.contains(s) && self.roots.contains(s) {
                labels.push(s.clone());
            }
        }
        Some(Viewer {
            labels,
            name: Some(p.label.clone()),
        })
    }

    pub fn session(&self, sid: &str) -> Option<Viewer> {
        let s = self.sessions.lock().expect("sessions lock");
        s.get(sid)
            .filter(|v| v.expires > Instant::now())
            .map(|v| v.viewer.clone())
    }

    pub fn logout(&self, sid: &str) {
        self.sessions.lock().expect("sessions lock").remove(sid);
    }
}

#[derive(Debug)]
pub enum LoginError {
    /// The state is unknown or expired (a replayed or stale callback).
    UnknownState,
    /// The provider could not be reached or answered wrongly.
    Provider(anyhow::Error),
    /// The ID token failed verification.
    InvalidToken(String),
    /// A valid identity that is not one of the configured people.
    NotAPerson,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_stays_inside_the_app() {
        assert_eq!(safe_next("t/p1?from=2023-05"), "t/p1?from=2023-05");
        assert_eq!(safe_next("/t/p1"), "t/p1");
        assert_eq!(safe_next("https://evil.example/"), "");
        // Leading slashes go: what is left is a path inside the app.
        assert_eq!(safe_next("//evil.example/"), "evil.example/");
        assert_eq!(safe_next("/\\evil.example"), "");
        assert_eq!(safe_next("t/../../x"), "");
        assert_eq!(safe_next("t/p1\r\nSet-Cookie: x"), "");
    }
}
