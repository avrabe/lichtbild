#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

//! Login (REQ-AUTH-001) and per-person scoping (REQ-AUTH-002) against a mock
//! OIDC provider that runs in the test: discovery, JWKS, and a token endpoint
//! that signs RS256 ID tokens with tests/fixtures/test-oidc-rsa.pem.

mod common;

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use base64::Engine;
use http_body_util::BodyExt;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use tower::ServiceExt;

use common::*;
use lichtbild::auth::{Auth, OidcSettings, Person, NEXT_MAX, PENDING_MAX};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

const KEY_PEM: &str = include_str!("fixtures/test-oidc-rsa.pem");
const CLIENT_ID: &str = "lichtbild-test";

#[derive(Default, Clone)]
struct Mock {
    issuer: String,
    /// Identity the next token is issued for.
    sub: String,
    email: String,
    /// Nonce the browser carried to the authorize endpoint.
    nonce: Option<String>,
    /// Faults to plant.
    wrong_nonce: bool,
    wrong_audience: bool,
    foreign_key: bool,
    /// Discovery names a token endpoint on another host.
    foreign_token_endpoint: bool,
}

type Shared = Arc<Mutex<Mock>>;

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn jwk() -> serde_json::Value {
    let key = rsa::RsaPrivateKey::from_pkcs8_pem(KEY_PEM).unwrap();
    serde_json::json!({ "keys": [{
        "kty": "RSA", "use": "sig", "alg": "RS256", "kid": "k1",
        "n": b64(&key.n().to_bytes_be()), "e": b64(&key.e().to_bytes_be()),
    }]})
}

fn sign(claims: &serde_json::Value, foreign: bool) -> String {
    let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    h.kid = Some("k1".into());
    let key = if foreign {
        // A different key with the same kid: the signature must not verify.
        use rsa::pkcs8::EncodePrivateKey;
        let other = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).unwrap();
        let pem = other.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap()
    } else {
        jsonwebtoken::EncodingKey::from_rsa_pem(KEY_PEM.as_bytes()).unwrap()
    };
    jsonwebtoken::encode(&h, claims, &key).unwrap()
}

/// Start the mock provider on a free loopback port.
async fn mock_provider() -> Shared {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let shared: Shared = Arc::new(Mutex::new(Mock {
        issuer,
        sub: "user-1".into(),
        email: "one@example.test".into(),
        ..Mock::default()
    }));
    let s1 = shared.clone();
    let s2 = shared.clone();
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let m = s1.lock().unwrap().clone();
                async move {
                    let iss = m.issuer;
                    let token = if m.foreign_token_endpoint {
                        "http://elsewhere.example:9/token".to_string()
                    } else {
                        format!("{iss}/token")
                    };
                    axum::Json(serde_json::json!({
                        "issuer": iss,
                        "authorization_endpoint": format!("{iss}/authorize"),
                        "token_endpoint": token,
                        "jwks_uri": format!("{iss}/jwks"),
                        "response_types_supported": ["code"],
                        "subject_types_supported": ["public"],
                        "id_token_signing_alg_values_supported": ["RS256"],
                    }))
                }
            }),
        )
        .route("/jwks", axum::routing::get(|| async { axum::Json(jwk()) }))
        .route(
            "/token",
            axum::routing::post(move |body: String| {
                let m = s2.lock().unwrap().clone();
                async move {
                    // Code flow with PKCE: the verifier must be present.
                    if !body.contains("code_verifier=")
                        || !body.contains("grant_type=authorization_code")
                    {
                        return (StatusCode::BAD_REQUEST, "missing pkce").into_response();
                    }
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    let nonce = if m.wrong_nonce {
                        "not-the-nonce".to_string()
                    } else {
                        m.nonce.clone().unwrap_or_default()
                    };
                    let aud = if m.wrong_audience {
                        "someone-else"
                    } else {
                        CLIENT_ID
                    };
                    let claims = serde_json::json!({
                        "iss": m.issuer, "sub": m.sub, "aud": aud,
                        "iat": now, "exp": now + 300, "nonce": nonce,
                        "email": m.email, "email_verified": true,
                    });
                    let token = serde_json::json!({
                        "access_token": "at", "token_type": "Bearer", "expires_in": 300,
                        "id_token": sign(&claims, m.foreign_key),
                    });
                    (StatusCode::OK, axum::Json(token)).into_response()
                }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    shared
}

struct Env {
    _fx: Fixture,
    app: axum::Router,
    mock: Shared,
    idx: Arc<Index>,
    auth: Arc<Auth>,
}

/// p1 belongs to subject user-1 (its email alone does not sign in); p2 to
/// the verified email two@example.test, and p2 may also see p1.
async fn env(public_url: &str) -> Env {
    let mock = mock_provider().await;
    let fx = fixture();
    let cfg = config(&fx, &fx.data);
    let secret = fx.data.join("client-secret");
    std::fs::write(&secret, "test-secret\n").unwrap();
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    scan::run(&cfg, &idx, &Options::default()).unwrap();
    let issuer = mock.lock().unwrap().issuer.clone();
    let auth = Auth::new(
        OidcSettings {
            issuer,
            client_id: CLIENT_ID.into(),
            client_secret_file: secret,
            ca_file: None,
            public_url: public_url.into(),
        },
        vec![
            Person {
                label: "p1".into(),
                oidc_subject: Some("user-1".into()),
                oidc_email: Some("one@example.test".into()),
                ..Person::default()
            },
            Person {
                label: "p2".into(),
                oidc_email: Some("Two@Example.test".into()),
                sees: vec!["p1".into()],
                ..Person::default()
            },
        ],
        vec!["p1".into(), "p2".into()],
    )
    .unwrap();
    let state = AppState::new(&cfg, idx.clone()).with_auth(Some(auth));
    let auth = state.auth.clone().unwrap();
    let app = web::router(state);
    Env {
        _fx: fx,
        app,
        mock,
        idx,
        auth,
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Resp {
    fn set_cookies(&self) -> Vec<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }
    fn session_set(&self) -> bool {
        self.set_cookies()
            .iter()
            .any(|c| c.starts_with("lichtbild_session=") && !c.contains("Max-Age=0"))
    }
}

async fn req_with(
    app: &axum::Router,
    method: &str,
    uri: &str,
    raw_cookie: Option<&str>,
    htmx: bool,
    extra: &[(&str, &str)],
) -> Resp {
    let mut r = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if let Some(c) = raw_cookie {
        r = r.header(header::COOKIE, c);
    }
    if htmx {
        r = r.header("hx-request", "true");
    }
    for (k, v) in extra {
        r = r.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(r.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Resp {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// A request with an optional session cookie.
async fn req(app: &axum::Router, method: &str, uri: &str, sid: Option<&str>, htmx: bool) -> Resp {
    let raw = sid.map(|c| format!("lichtbild_session={c}"));
    req_with(app, method, uri, raw.as_deref(), htmx, &[]).await
}

fn location(r: &Resp) -> String {
    r.headers
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    openidconnect::url::form_urlencoded::parse(q.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Start a login in a fresh browser: (state, binding cookie value).
async fn start(e: &Env, next: &str) -> (String, String) {
    let r = req(
        &e.app,
        "GET",
        &format!("/auth/login?next={next}"),
        None,
        false,
    )
    .await;
    assert!(r.status.is_redirection(), "login: {} {}", r.status, r.body);
    let authorize = location(&r);
    let iss = e.mock.lock().unwrap().issuer.clone();
    assert!(
        authorize.starts_with(&format!("{iss}/authorize?")),
        "{authorize}"
    );
    assert_eq!(
        query_param(&authorize, "client_id").as_deref(),
        Some(CLIENT_ID)
    );
    assert_eq!(
        query_param(&authorize, "response_type").as_deref(),
        Some("code")
    );
    assert_eq!(
        query_param(&authorize, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert!(query_param(&authorize, "scope").unwrap().contains("openid"));
    e.mock.lock().unwrap().nonce = query_param(&authorize, "nonce");
    let cookie = r
        .set_cookies()
        .into_iter()
        .find(|c| c.starts_with("lichtbild_login="))
        .expect("no login-binding cookie");
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
            && cookie.contains("Path=/auth"),
        "{cookie}"
    );
    let binding = cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string();
    (query_param(&authorize, "state").unwrap(), binding)
}

/// The provider sends the browser back with the code and state.
async fn callback(e: &Env, state: &str, binding: Option<&str>) -> Resp {
    let raw = binding.map(|b| format!("lichtbild_login={b}"));
    let uri = format!("/auth/callback?code=c1&state={state}");
    req_with(&e.app, "GET", &uri, raw.as_deref(), false, &[]).await
}

/// Log in as the mock's current identity: (session id, "cookie | landing").
async fn login(e: &Env, next: &str) -> Result<(String, String), Resp> {
    let (state, binding) = start(e, next).await;
    let r = callback(e, &state, Some(&binding)).await;
    if !r.status.is_redirection() {
        return Err(r);
    }
    let cookie = r
        .set_cookies()
        .into_iter()
        .find(|c| c.starts_with("lichtbild_session="))
        .unwrap();
    let sid = cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string();
    Ok((sid, cookie + " | " + &location(&r)))
}

// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn every_route_needs_a_session_except_health_static_and_login() {
    let e = env("http://localhost").await;
    let id = e.idx.all().unwrap()[0].0;
    let routes = [
        "/".to_string(),
        "/t/p1".into(),
        "/t/all".into(),
        format!("/v/p1/{id}"),
        format!("/thumb/{id}"),
        format!("/original/{id}"),
        format!("/preview/{id}"),
        "/t/nobody".into(),
    ];
    for uri in &routes {
        let r = req(&e.app, "GET", uri, None, false).await;
        assert_eq!(
            r.status,
            StatusCode::SEE_OTHER,
            "{uri} without session: {}",
            r.status
        );
        assert!(location(&r).starts_with("/auth/login?next="), "{uri}");
        assert!(r.headers.get("content-security-policy").is_some());
        // A forged cookie is no session.
        let r = req(&e.app, "GET", uri, Some("forged"), false).await;
        assert_eq!(
            r.status,
            StatusCode::SEE_OTHER,
            "{uri} with a forged cookie"
        );
    }
    // htmx requests get 401 and an HX-Redirect to the login.
    let r = req(&e.app, "GET", "/t/p1?before=x&id=1", None, true).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let to = r.headers.get("hx-redirect").unwrap().to_str().unwrap();
    assert!(to.starts_with("/auth/login?next="), "{to}");
    // Public: health and the embedded assets.
    assert_eq!(
        req(&e.app, "GET", "/healthz", None, false).await.status,
        StatusCode::OK
    );
    assert_eq!(
        req(&e.app, "GET", "/static/lichtbild.css", None, false)
            .await
            .status,
        StatusCode::OK
    );
}

// rivet: verifies REQ-AUTH-001
// rivet: verifies REQ-AUTH-002
#[tokio::test]
async fn login_gives_a_session_scoped_to_the_person() {
    let e = env("http://localhost").await;
    let (sid, cookie) = login(&e, "%2Ft%2Fp1%3Ffrom%3D2021-06")
        .await
        .map_err(|r| r.body)
        .unwrap();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"),
        "{cookie}"
    );
    assert!(!cookie.contains("Secure"), "Secure cookie on plain http");
    assert!(
        cookie.ends_with(" | /t/p1?from=2021-06"),
        "back to where the user was: {cookie}"
    );
    assert!(sid.len() >= 40, "session id too short");

    let all = e.idx.all().unwrap();
    let p1 = all
        .iter()
        .find(|(_, r)| r.label == "p1" && r.thumb == "ok")
        .unwrap()
        .0;
    let p2 = all.iter().find(|(_, r)| r.label == "p2").unwrap().0;
    for uri in [
        "/t/p1".to_string(),
        format!("/v/p1/{p1}"),
        format!("/thumb/{p1}"),
    ] {
        let r = req(&e.app, "GET", &uri, Some(&sid), false).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
    }
    // The other person's library: not visible in any way.
    for uri in [
        "/t/p2".to_string(),
        "/t/all".into(),
        format!("/v/p2/{p2}"),
        format!("/v/p1/{p2}"),
        format!("/thumb/{p2}"),
        format!("/original/{p2}"),
        format!("/preview/{p2}"),
    ] {
        let r = req(&e.app, "GET", &uri, Some(&sid), false).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri} visible to p1");
    }
    let home = req(&e.app, "GET", "/", Some(&sid), false).await;
    assert!(home.body.contains(r#"href="t/p1""#));
    assert!(!home.body.contains(r#"href="t/p2""#), "p2 listed for p1");
    assert!(!home.body.contains("merged"), "merged view without a grant");
    assert!(home.body.contains("Sign out"));

    // Logout ends the session.
    let r = req(&e.app, "POST", "/auth/logout", Some(&sid), false).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.set_cookies().iter().any(|c| c.contains("Max-Age=0")));
    assert_eq!(
        req(&e.app, "GET", "/t/p1", Some(&sid), false).await.status,
        StatusCode::SEE_OTHER
    );
}

// rivet: verifies REQ-AUTH-002
#[tokio::test]
async fn a_granted_person_sees_both_libraries_and_the_merged_view() {
    let e = env("http://localhost").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-2-unmapped-subject".into();
        m.email = "two@example.test".into(); // matched by verified email, any case
    }
    let (sid, _) = login(&e, "").await.map_err(|r| r.body).unwrap();
    let p1 = e
        .idx
        .all()
        .unwrap()
        .iter()
        .find(|(_, r)| r.label == "p1")
        .unwrap()
        .0;
    for uri in [
        "/t/p2".to_string(),
        "/t/p1".into(),
        "/t/all".into(),
        format!("/thumb/{p1}"),
    ] {
        let r = req(&e.app, "GET", &uri, Some(&sid), false).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
    }
    let home = req(&e.app, "GET", "/", Some(&sid), false).await;
    assert!(
        home.body.contains(r#"href="t/all""#),
        "merged view missing with a grant"
    );
}

// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn bad_logins_get_no_session() {
    let e = env("https://photos.example").await;
    // An identity that is not one of the people: 403, no cookie.
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "stranger".into();
        m.email = "stranger@example.test".into();
    }
    let r = login(&e, "").await.unwrap_err();
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert!(!r.session_set());

    // Forged tokens: wrong nonce, wrong audience, signed with another key.
    for fault in ["nonce", "audience", "key"] {
        {
            let mut m = e.mock.lock().unwrap();
            m.sub = "user-1".into();
            m.wrong_nonce = fault == "nonce";
            m.wrong_audience = fault == "audience";
            m.foreign_key = fault == "key";
        }
        let r = login(&e, "").await.unwrap_err();
        assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{fault}: {}", r.body);
        assert!(!r.session_set(), "{fault}: session set");
    }

    // A callback for a state lichtbild never issued, or replayed.
    {
        let mut m = e.mock.lock().unwrap();
        m.wrong_nonce = false;
        m.wrong_audience = false;
        m.foreign_key = false;
    }
    let r = callback(&e, "made-up", Some("made-up")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let (state, binding) = start(&e, "").await;
    let first = callback(&e, &state, Some(&binding)).await;
    assert!(first.status.is_redirection(), "{}", first.body);
    // Secure cookies behind https.
    assert!(
        first.set_cookies().iter().all(|c| c.contains("; Secure")),
        "{:?}",
        first.set_cookies()
    );
    let replay = callback(&e, &state, Some(&binding)).await;
    assert_eq!(
        replay.status,
        StatusCode::BAD_REQUEST,
        "replayed callback accepted"
    );
}

// Login CSRF: a callback is accepted only in the browser that started the
// login.
// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn a_callback_from_another_browser_is_refused() {
    let e = env("http://localhost").await;
    let (state, _) = start(&e, "").await;
    let (_, other_binding) = start(&e, "").await;
    for wrong in [
        None,
        Some("forged-binding"),
        Some(""),
        Some(other_binding.as_str()),
    ] {
        let r = callback(&e, &state, wrong).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{wrong:?}");
        assert!(!r.session_set(), "{wrong:?}: session set");
    }
    // The mock issues the nonce of the latest start; a fresh login in the
    // right browser completes and clears the binding cookie.
    let (state, binding) = start(&e, "").await;
    let ok = callback(&e, &state, Some(&binding)).await;
    assert!(ok.status.is_redirection(), "{}", ok.body);
    assert!(ok.session_set());
    assert!(
        ok.set_cookies()
            .iter()
            .any(|c| c.starts_with("lichtbild_login=;") && c.contains("Max-Age=0")),
        "binding not cleared: {:?}",
        ok.set_cookies()
    );
}

// A foreign callback attempt must not consume the real login.
// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn a_foreign_callback_does_not_burn_the_real_login() {
    let e = env("http://localhost").await;
    let (state, binding) = start(&e, "").await;
    assert_eq!(
        callback(&e, &state, None).await.status,
        StatusCode::BAD_REQUEST
    );
    let ok = callback(&e, &state, Some(&binding)).await;
    assert!(ok.status.is_redirection(), "{}", ok.body);
}

// Unauthenticated logins cannot grow memory without bound.
// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn pending_logins_are_capped_and_long_next_is_dropped() {
    let e = env("http://localhost").await;
    let long = "a".repeat(NEXT_MAX + 10);
    for _ in 0..PENDING_MAX + 50 {
        let r = req(
            &e.app,
            "GET",
            &format!("/auth/login?next={long}"),
            None,
            false,
        )
        .await;
        assert!(r.status.is_redirection());
    }
    assert_eq!(e.auth.pending_len(), PENDING_MAX);
    // A login still works, and the over-long next fell back to the start.
    let (_, landed) = login(&e, &long).await.map_err(|r| r.body).unwrap();
    assert!(landed.ends_with(" | /"), "{landed}");
}

// An email claim cannot stand in for a person bound to a subject.
// rivet: verifies REQ-AUTH-002
#[tokio::test]
async fn email_does_not_override_a_configured_subject() {
    let e = env("http://localhost").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "mallory".into();
        m.email = "one@example.test".into(); // p1's email, verified
    }
    let r = login(&e, "").await.unwrap_err();
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(e.auth.viewer_for("user-1", None).is_some());
    assert!(e
        .auth
        .viewer_for("mallory", Some("ONE@example.test"))
        .is_none());
    assert!(
        e.auth
            .viewer_for("anyone", Some("two@example.test"))
            .is_some(),
        "email-only person"
    );
}

// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn a_cross_site_logout_is_refused() {
    let e = env("http://localhost").await;
    let (sid, _) = login(&e, "").await.map_err(|r| r.body).unwrap();
    let raw = format!("lichtbild_session={sid}");
    let cross = [("sec-fetch-site", "cross-site")];
    let r = req_with(&e.app, "POST", "/auth/logout", Some(&raw), false, &cross).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.set_cookies().is_empty());
    assert_eq!(
        req(&e.app, "GET", "/t/p1", Some(&sid), false).await.status,
        StatusCode::OK,
        "a cross-site form ended the session"
    );
    let same = [("sec-fetch-site", "same-origin")];
    let r = req_with(&e.app, "POST", "/auth/logout", Some(&raw), false, &same).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        req(&e.app, "GET", "/t/p1", Some(&sid), false).await.status,
        StatusCode::SEE_OTHER
    );
}

// The issuer is the only host lichtbild contacts: a token endpoint
// elsewhere is refused, and a failed discovery is not retried at once.
// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn endpoints_off_the_issuer_host_are_refused() {
    let e = env("http://localhost").await;
    e.mock.lock().unwrap().foreign_token_endpoint = true;
    let r = req(&e.app, "GET", "/auth/login", None, false).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.body);
    assert!(r.body.contains("Login is unavailable"));
    // Fixed at the provider, but within the back-off: still failing fast.
    e.mock.lock().unwrap().foreign_token_endpoint = false;
    let r = req(&e.app, "GET", "/auth/login", None, false).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
}

// The login never sends the browser anywhere but the issuer, and `next`
// cannot leave the app or its base path.
// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn next_after_login_stays_in_the_app() {
    let e = env("http://localhost").await;
    for next in [
        "https%3A%2F%2Fevil.example%2F",
        "%2F%2Fevil.example",
        "%2F%5Cevil.example",
        "%252e%252e%2Fother-app%2F",
        "t%2F..%2F..%2Fother-app",
    ] {
        let (_, cookie) = login(&e, next).await.map_err(|r| r.body).unwrap();
        let target = cookie.rsplit(" | ").next().unwrap();
        assert!(
            target.starts_with('/') && !target.starts_with("//"),
            "{next} -> {target}"
        );
        assert!(
            !target.contains("://") && !target.contains('%') && !target.contains(".."),
            "{next} -> {target}"
        );
    }
}
