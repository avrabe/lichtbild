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
use lichtbild::auth::{
    session_key, Auth, OidcSettings, Person, SessionStore, NEXT_MAX, PENDING_MAX,
};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

const KEY_PEM: &str = include_str!("fixtures/test-oidc-rsa.pem");
const CLIENT_ID: &str = "lichtbild-test";
/// A made-up provider project id, the extra audience Zitadel adds.
const PROJECT_ID: &str = "project-1";

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
    /// A second audience next to the client id, as Zitadel adds its
    /// project id.
    extra_audience: Option<String>,
    /// The `azp` claim, when set.
    azp: Option<String>,
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
                        serde_json::json!("someone-else")
                    } else if let Some(extra) = &m.extra_audience {
                        serde_json::json!([CLIENT_ID, extra])
                    } else {
                        serde_json::json!(CLIENT_ID)
                    };
                    let mut claims = serde_json::json!({
                        "iss": m.issuer, "sub": m.sub, "aud": aud,
                        "iat": now, "exp": now + 300, "nonce": nonce,
                        "email": m.email, "email_verified": true,
                    });
                    if let Some(azp) = &m.azp {
                        claims["azp"] = serde_json::json!(azp);
                    }
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
    people: Arc<lichtbild::people::People>,
    search: Arc<lichtbild::search::Search>,
}

/// p1 belongs to subject user-1 (its email alone does not sign in); p2 to
/// the verified email two@example.test, and p2 may also see p1.
async fn env(public_url: &str) -> Env {
    env_with(public_url, false).await
}

/// `env`, with the looking at photos switched on or off.
async fn env_with(public_url: &str, vision: bool) -> Env {
    let mock = mock_provider().await;
    let fx = fixture();
    let cfg = config(&fx, &fx.data);
    let secret = fx.data.join("client-secret");
    std::fs::write(&secret, "test-secret\n").unwrap();
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    scan::run(&cfg, &idx, &Options::default()).unwrap();
    let issuer = mock.lock().unwrap().issuer.clone();
    let auth = make_auth(&issuer, &secret, public_url, true).with_store(idx.clone());
    let mut state = AppState::new(&cfg, idx.clone()).with_auth(Some(auth));
    state.vision_on = vision;
    state.search.rebuild(&idx, None).unwrap();
    let auth = state.auth.clone().unwrap();
    let people = state.people.clone();
    let search = state.search.clone();
    let app = web::router(state);
    Env {
        _fx: fx,
        app,
        mock,
        idx,
        auth,
        people,
        search,
    }
}

/// The login setup of `env`; `with_p1` leaves person p1 out when false.
fn make_auth(issuer: &str, secret: &std::path::Path, public_url: &str, with_p1: bool) -> Auth {
    let mut people = vec![
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
    ];
    if !with_p1 {
        people.remove(0);
    }
    Auth::new(
        OidcSettings {
            issuer: issuer.into(),
            client_id: CLIENT_ID.into(),
            client_secret_file: secret.to_path_buf(),
            ca_file: None,
            trusted_audiences: vec![PROJECT_ID.into()],
            public_url: public_url.into(),
        },
        people,
        vec!["p1".into(), "p2".into()],
    )
    .unwrap()
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
    let mut x = extra.to_vec();
    if htmx {
        x.push(("hx-request", "true"));
    }
    req_with_body(app, method, uri, raw_cookie, "", &x).await
}

/// A request with a form body.
async fn req_with_body(
    app: &axum::Router,
    method: &str,
    uri: &str,
    raw_cookie: Option<&str>,
    body: &str,
    extra: &[(&str, &str)],
) -> Resp {
    let mut r = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if let Some(c) = raw_cookie {
        r = r.header(header::COOKIE, c);
    }
    for (k, v) in extra {
        r = r.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(r.body(Body::from(body.to_string())).unwrap())
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
        format!("/captions/{id}"),
        "/t/nobody".into(),
        "/corrections".into(),
        "/corrections.jsonl".into(),
        "/countries".into(),
        "/days/all?day=2021-07-05".into(),
        "/hidden?sort=name".into(),
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

// Zitadel puts its project id into `aud` next to the client id. Such a
// token signs in when the extra audience is trusted and `azp` names this
// client (OIDC Core 3.1.3.7), and nowhere else.
// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn a_trusted_second_audience_signs_in_only_with_azp() {
    let e = env("https://photos.example").await;
    let cases = [
        (Some(PROJECT_ID), Some(CLIENT_ID), true),
        (Some(PROJECT_ID), None, false),
        (Some(PROJECT_ID), Some("another-client"), false),
        (Some("untrusted-project"), Some(CLIENT_ID), false),
        (None, Some("another-client"), false),
        (None, Some(CLIENT_ID), true),
    ];
    for (extra, azp, ok) in cases {
        {
            let mut m = e.mock.lock().unwrap();
            m.sub = "user-1".into();
            m.extra_audience = extra.map(String::from);
            m.azp = azp.map(String::from);
        }
        let case = format!("aud+{extra:?} azp={azp:?}");
        match login(&e, "").await {
            Ok(_) => assert!(ok, "{case}: signed in"),
            Err(r) => {
                assert!(!ok, "{case}: refused with {} {}", r.status, r.body);
                assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{case}: {}", r.body);
                assert!(!r.session_set(), "{case}: session set");
            }
        }
    }
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
        // Pages without a session do not tell which version runs.
        assert!(!r.body.contains("class=\"version\""), "{fault}: {}", r.body);
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

// The world map with a real session: p1 sees only p1's positions, and the
// map routes need a session like every other route.
// rivet: verifies REQ-MAP-001
// rivet: verifies REQ-AUTH-002
#[tokio::test]
async fn the_map_shows_a_person_only_their_own_positions() {
    let e = env("http://localhost").await;
    // Positions in p2 too, at the made-up Westdorf coordinates.
    std::fs::write(
        e._fx.root2.join("IMG_west.jpg"),
        jpeg_with_exif(
            "2023:01:02 12:00:00",
            "+01:00",
            Some(((10, 24, 'N'), (19, 54, 'E'))),
            31,
        ),
    )
    .unwrap();
    let cfg = config(&e._fx, &e._fx.data);
    scan::run(&cfg, &e.idx, &Options::default()).unwrap();
    let positioned = |label: &str| -> Vec<u64> {
        e.idx
            .all()
            .unwrap()
            .into_iter()
            .filter(|(_, r)| r.label == label && r.lat.is_some())
            .map(|(id, _)| id)
            .collect()
    };
    let (p1, p2) = (positioned("p1"), positioned("p2"));
    assert!(!p1.is_empty() && !p2.is_empty());

    for uri in [
        "/map",
        "/map/clusters?z=3",
        "/map/tiles.pmtiles",
        "/map/assets/fonts/a/0-255.pbf",
    ] {
        let r = req(&e.app, "GET", uri, None, false).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{uri} without session");
    }
    let (sid, _) = login(&e, "").await.map_err(|r| r.body).unwrap();
    let r = req(&e.app, "GET", "/map/clusters?z=18", Some(&sid), false).await;
    assert_eq!(r.status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    let mut seen = Vec::new();
    for c in json["clusters"].as_array().unwrap() {
        for it in c["items"].as_array().into_iter().flatten() {
            assert_eq!(it["who"], "p1", "{}", r.body);
            seen.push(it["id"].as_u64().unwrap());
        }
    }
    assert_eq!(json["total"].as_u64().unwrap() as usize, p1.len());
    assert_eq!(seen.len(), p1.len());
    assert!(
        seen.iter().all(|id| !p2.contains(id)),
        "p2's items on p1's map"
    );
    for uri in [
        "/map/clusters?who=p2",
        "/map/clusters?who=all",
        "/map?who=p2",
    ] {
        let r = req(&e.app, "GET", uri, Some(&sid), false).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri} for p1");
    }
    let page = req(&e.app, "GET", "/map", Some(&sid), false).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains(&format!("{} photo", p1.len())),
        "{}",
        page.body
    );
}

// A restart (an upgrade) keeps everyone signed in: the session is stored
// by its hash, and the person's access is worked out again from the
// configuration, so someone taken out of it is signed out. Sign-out ends
// the stored session too.
// rivet: verifies REQ-AUTH-001
#[tokio::test]
async fn sessions_survive_a_restart_but_not_a_removal_or_logout() {
    let e = env("https://photos.example").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
    }
    let Ok((sid, _)) = login(&e, "").await else {
        panic!("login failed");
    };
    // Only the hash is stored, never the cookie value.
    assert!(e.idx.get_session(&sid).unwrap().is_none());
    let (person, identity, _) = e.idx.get_session(&session_key(&sid)).unwrap().unwrap();
    assert_eq!((person.as_str(), identity.as_str()), ("p1", "sub:user-1"));

    let issuer = e.mock.lock().unwrap().issuer.clone();
    let secret = e._fx.data.join("client-secret");
    // "Restart": a new Auth with empty memory over the same index.
    let again =
        make_auth(&issuer, &secret, "https://photos.example", true).with_store(e.idx.clone());
    let v = again.session(&sid).expect("session lost in the restart");
    assert_eq!(v.labels, vec!["p1".to_string()]);
    assert!(again.session("made-up").is_none());
    // p1 taken out of the configuration: the stored session no longer counts.
    let without =
        make_auth(&issuer, &secret, "https://photos.example", false).with_store(e.idx.clone());
    assert!(without.session(&sid).is_none());
    // The label given to someone else: the stored session is not theirs.
    e.idx
        .put_session(&session_key("other"), "p1", "sub:someone-else", u64::MAX)
        .unwrap();
    assert!(again.session("other").is_none());
    // Sign-out removes the stored session.
    again.logout(&sid).unwrap();
    assert!(e.idx.get_session(&session_key(&sid)).unwrap().is_none());
    let after =
        make_auth(&issuer, &secret, "https://photos.example", true).with_store(e.idx.clone());
    assert!(after.session(&sid).is_none());
}

// One person's hiding never changes another's view (REQ-DUP-002).
// rivet: verifies REQ-DUP-002
#[tokio::test]
async fn hiding_is_per_person() {
    let e = env("https://photos.example").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
    }
    let Ok((p1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let id = e
        .idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.label == "p1" && r.rel == b"a/IMG_exif.jpg")
        .map(|(id, _)| id)
        .unwrap();
    let c1 = format!("lichtbild_session={p1}");
    let r = req_with(
        &e.app,
        "POST",
        &format!("/hide/{id}"),
        Some(&c1),
        false,
        &[
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
    )
    .await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    let mine = req_with(&e.app, "GET", "/t/p1", Some(&c1), false, &[]).await;
    assert!(
        !mine.body.contains(&format!("id=\"i{id}\"")),
        "hidden for p1"
    );
    // p1 may not see p2's library, so cannot hide in it either.
    let other = e
        .idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.label == "p2")
        .map(|(id, _)| id)
        .unwrap();
    let r = req_with(
        &e.app,
        "POST",
        &format!("/hide/{other}"),
        Some(&c1),
        false,
        &[
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(e.idx.hidden("p1").unwrap().len(), 1);

    // p2 may see p1's library, and still sees the photo.
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "someone-else".into();
        m.email = "two@example.test".into();
    }
    let Ok((p2, _)) = login(&e, "").await else {
        panic!("p2 login failed");
    };
    let c2 = format!("lichtbild_session={p2}");
    let theirs = req_with(&e.app, "GET", "/t/p1", Some(&c2), false, &[]).await;
    assert_eq!(theirs.status, StatusCode::OK);
    assert!(
        theirs.body.contains(&format!("id=\"i{id}\"")),
        "not hidden for p2"
    );
}

// The status page shows totals over all libraries only to whoever sees
// all of them (REQ-WEB-009).
// rivet: verifies REQ-WEB-009
#[tokio::test]
async fn status_totals_only_for_whoever_sees_every_library() {
    let e = env("https://photos.example").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
    }
    let Ok((p1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let c1 = format!("lichtbild_session={p1}");
    let page = req_with(&e.app, "GET", "/status", Some(&c1), false, &[]).await;
    assert!(page.status.is_success(), "{}", page.status);
    assert!(page.body.contains("<dt>Now</dt>"), "{}", page.body);
    assert!(!page.body.contains("<dt>Duplicates</dt>"), "{}", page.body);
    assert!(!page.body.contains("<dt>Search</dt>"), "{}", page.body);
}

// Only the library's owner places its pictures by hand (REQ-PLACE-002):
// p2 may see p1's pictures but not set where they were taken.
// rivet: verifies REQ-PLACE-002
#[tokio::test]
async fn only_the_owner_places_a_picture_by_hand() {
    let e = env("http://localhost").await;
    let unplaced = |label: &str| {
        e.idx
            .all()
            .unwrap()
            .into_iter()
            .find(|(_, r)| r.label == label && r.lat.is_none() && !r.missing)
            .unwrap()
            .0
    };
    let (in_p1, in_p2) = (unplaced("p1"), unplaced("p2"));
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-2-unmapped-subject".into();
        m.email = "two@example.test".into();
    }
    let (sid, _) = login(&e, "").await.map_err(|r| r.body).unwrap();
    let c = format!("lichtbild_session={sid}");
    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    let post = |uri: String| {
        let (app, c) = (e.app.clone(), c.clone());
        async move {
            req_with_body(&app, "POST", &uri, Some(&c), "lat=10.4&lon=19.9", &form)
                .await
                .status
        }
    };
    assert_eq!(post(format!("/place/{in_p1}")).await, StatusCode::NOT_FOUND);
    assert!(e.idx.placed().unwrap().is_empty(), "nothing placed in p1");
    assert!(post(format!("/place/{in_p2}")).await.is_redirection());
    assert_eq!(e.idx.placed().unwrap()[&in_p2].by, "p2");
    let page = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{in_p1}"),
        Some(&c),
        false,
        &[],
    )
    .await;
    assert!(
        !page.body.contains("map?place="),
        "no placing in p1: {}",
        page.body
    );
}

// Several items hidden at once, and shown again: only those the person may
// see, only for them, only from the same site.
// rivet: verifies REQ-WEB-014
#[tokio::test]
async fn several_items_are_hidden_at_once_and_shown_again() {
    let e = env("https://photos.example").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
    }
    let Ok((p1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let c1 = format!("lichtbild_session={p1}");
    let all = e.idx.all().unwrap();
    let mine: Vec<u64> = all
        .iter()
        .filter(|(_, r)| r.label == "p1")
        .map(|(id, _)| *id)
        .take(2)
        .collect();
    let other = all.iter().find(|(_, r)| r.label == "p2").unwrap().0;
    let body = format!("ids={}%2C{}%2C{other}%2Cnope", mine[0], mine[1]);
    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    let r = req_with_body(&e.app, "POST", "/hide-many", Some(&c1), &body, &form).await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "2"));
    let hidden = e.idx.hidden("p1").unwrap();
    assert_eq!(hidden.len(), 2);
    assert!(mine.iter().all(|id| hidden.contains(id)));
    let page = req_with(&e.app, "GET", "/t/p1", Some(&c1), false, &[]).await;
    assert!(!page.body.contains(&format!("id=\"i{}\"", mine[0])));
    assert!(page.body.contains("class=\"tbtn select\""), "{}", page.body);
    // From another site: refused, nothing changes.
    let cross = [
        ("sec-fetch-site", "cross-site"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    let r = req_with_body(&e.app, "POST", "/unhide-many", Some(&c1), &body, &cross).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(e.idx.hidden("p1").unwrap().len(), 2);
    // The undo.
    let r = req_with_body(&e.app, "POST", "/unhide-many", Some(&c1), &body, &form).await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "2"));
    assert!(e.idx.hidden("p1").unwrap().is_empty());
}

// Favourites are each person's own: p1 marks some, alone or several at
// once, only items they may see and only from the same site; p2, who also
// sees p1's library, finds none of them among their own.
// rivet: verifies REQ-WEB-016
#[tokio::test]
async fn favourites_are_kept_per_person() {
    let e = env("https://photos.example").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
    }
    let Ok((p1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let c1 = format!("lichtbild_session={p1}");
    let all = e.idx.all().unwrap();
    let mine: Vec<u64> = all
        .iter()
        .filter(|(_, r)| r.label == "p1" && r.video.is_none())
        .map(|(id, _)| *id)
        .take(2)
        .collect();
    let other = all.iter().find(|(_, r)| r.label == "p2").unwrap().0;
    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    // One from the viewer, then several from a selection; p2's item and
    // nonsense are left alone.
    let back = format!("back=v%2Fp1%2F{}", mine[0]);
    let r = req_with_body(
        &e.app,
        "POST",
        &format!("/fav/{}", mine[0]),
        Some(&c1),
        &back,
        &form,
    )
    .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    let body = format!("ids={}%2C{other}%2Cnope", mine[1]);
    let r = req_with_body(&e.app, "POST", "/fav-many", Some(&c1), &body, &form).await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "1"));
    let favs = e.idx.favs("p1").unwrap();
    assert_eq!(favs.len(), 2);
    assert!(mine.iter().all(|id| favs.contains(id)));
    // From another site: refused.
    let cross = [
        ("sec-fetch-site", "cross-site"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    let r = req_with_body(
        &e.app,
        "POST",
        &format!("/unfav/{}", mine[0]),
        Some(&c1),
        &back,
        &cross,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(e.idx.favs("p1").unwrap().len(), 2);

    // The page, the timeline's hearts, the viewer's heart, the slideshow.
    let page = req_with(&e.app, "GET", "/favourites", Some(&c1), false, &[]).await;
    assert_eq!(page.status, StatusCode::OK);
    for id in &mine {
        assert!(
            page.body.contains(&format!("id=\"i{id}\"")),
            "{}",
            page.body
        );
    }
    assert!(!page.body.contains(&format!("id=\"i{other}\"")));
    assert_eq!(
        page.body.matches("class=\"heart\"").count(),
        2,
        "{}",
        page.body
    );
    let tl = req_with(&e.app, "GET", "/t/p1", Some(&c1), false, &[]).await;
    assert_eq!(tl.body.matches("class=\"heart\"").count(), 2, "{}", tl.body);
    let v = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", mine[0]),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert!(
        v.body.contains(&format!("action=\"unfav/{}\"", mine[0])),
        "{}",
        v.body
    );
    assert!(v.body.contains("aria-pressed=\"true\""));
    let show = req_with(&e.app, "GET", "/show/p1?fav=1", Some(&c1), false, &[]).await;
    assert_eq!(
        show.body.matches("<li data-src=").count(),
        2,
        "{}",
        show.body
    );
    assert!(show.body.contains("class=\"close\" href=\"favourites\""));
    // A hidden favourite is not shown.
    e.idx.set_hidden("p1", mine[1], true).unwrap();
    let page = req_with(&e.app, "GET", "/favourites", Some(&c1), false, &[]).await;
    assert!(!page.body.contains(&format!("id=\"i{}\"", mine[1])));
    e.idx.set_hidden("p1", mine[1], false).unwrap();

    // In search, "♥" or "Favoriten" keeps only favourites; alone it is the
    // Favourites page, and "Fav" is completed.
    let year = &all
        .iter()
        .find(|(id, _)| *id == mine[0])
        .unwrap()
        .1
        .taken_local[..4];
    let plain = req_with(
        &e.app,
        "GET",
        &format!("/search?q={year}"),
        Some(&c1),
        false,
        &[],
    )
    .await;
    let only = req_with(
        &e.app,
        "GET",
        &format!("/search?q=%E2%99%A5+{year}"),
        Some(&c1),
        false,
        &[],
    )
    .await;
    let found = |page: &str| -> Vec<u64> {
        page.split("class=\"item")
            .skip(1)
            .filter_map(|p| p.split("href=\"v/p1/").nth(1))
            .filter_map(|p| p.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
            .collect()
    };
    let (plain, only) = (found(&plain.body), found(&only.body));
    assert!(plain.contains(&mine[0]), "{plain:?}");
    assert!(
        plain.iter().any(|id| !mine.contains(id)),
        "the year finds others too: {plain:?}"
    );
    assert!(only.contains(&mine[0]), "{only:?}");
    assert!(only.iter().all(|id| mine.contains(id)), "{only:?}");
    let r = req_with(&e.app, "GET", "/search?q=Favoriten", Some(&c1), false, &[]).await;
    assert!(location(&r).ends_with("/favourites"), "{}", location(&r));
    let sug = req_with(&e.app, "GET", "/suggest?q=Fav", Some(&c1), false, &[]).await;
    assert!(
        sug.body
            .contains("<b>Favoriten</b></span><span class=\"dim\">2<"),
        "{}",
        sug.body
    );
    // Counted as the page shows them: a hidden favourite is not.
    e.idx.set_hidden("p1", mine[1], true).unwrap();
    let sug = req_with(&e.app, "GET", "/suggest?q=Fav", Some(&c1), false, &[]).await;
    assert!(
        sug.body
            .contains("<b>Favoriten</b></span><span class=\"dim\">1<"),
        "{}",
        sug.body
    );
    e.idx.set_hidden("p1", mine[1], false).unwrap();

    // p2 sees p1's photos but not p1's favourites.
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-2-unmapped-subject".into();
        m.email = "two@example.test".into();
    }
    let (p2, _) = login(&e, "").await.map_err(|r| r.body).unwrap();
    let c2 = format!("lichtbild_session={p2}");
    let page = req_with(&e.app, "GET", "/favourites", Some(&c2), false, &[]).await;
    assert!(page.body.contains("None yet"), "{}", page.body);
    let only = req_with(
        &e.app,
        "GET",
        &format!("/search?q=%E2%99%A5+{year}"),
        Some(&c2),
        false,
        &[],
    )
    .await;
    assert!(found(&only.body).is_empty(), "{}", only.body);
    let sug = req_with(&e.app, "GET", "/suggest?q=Fav", Some(&c2), false, &[]).await;
    assert!(!sug.body.contains("Favoriten"), "{}", sug.body);
    let v = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", mine[0]),
        Some(&c2),
        false,
        &[],
    )
    .await;
    assert!(
        v.body.contains(&format!("action=\"fav/{}\"", mine[0])),
        "{}",
        v.body
    );
    assert!(e.idx.favs("p2").unwrap().is_empty());

    // p1 takes them out again.
    let body = format!("ids={}%2C{}", mine[0], mine[1]);
    let r = req_with_body(&e.app, "POST", "/unfav-many", Some(&c1), &body, &form).await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "2"));
    assert!(e.idx.favs("p1").unwrap().is_empty());
}

// Albums are each person's own: p1 makes them, fills them only with
// items they may see, renames, empties and deletes them; p2, who sees
// p1's library, can neither see nor change them.
// rivet: verifies REQ-WEB-017
#[tokio::test]
async fn albums_are_each_persons_own() {
    let e = env("https://photos.example").await;
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
    }
    let Ok((p1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let c1 = format!("lichtbild_session={p1}");
    let all = e.idx.all().unwrap();
    let mine: Vec<u64> = all
        .iter()
        .filter(|(_, r)| r.label == "p1" && r.video.is_none())
        .map(|(id, _)| *id)
        .take(2)
        .collect();
    let other = all.iter().find(|(_, r)| r.label == "p2").unwrap().0;
    let post = |uri: String,
                cookie: String,
                body: String,
                headers: &'static [(&'static str, &'static str)]| {
        let app = e.app.clone();
        async move { req_with_body(&app, "POST", &uri, Some(&cookie), &body, headers).await }
    };
    const FORM: &[(&str, &str)] = &[
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    const CROSS: &[(&str, &str)] = &[
        ("sec-fetch-site", "cross-site"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];

    // Made from the albums page, then filled from a selection.
    let r = post("/albums".into(), c1.clone(), "name=Sommer".into(), FORM).await;
    assert!(location(&r).ends_with("/album/1"), "{}", location(&r));
    let r = post("/albums".into(), c1.clone(), "name=+++".into(), FORM).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let body = format!("album=1&ids={}%2C{}%2C{other}%2Cnope", mine[0], mine[1]);
    let r = post("/album-add".into(), c1.clone(), body.clone(), FORM).await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "1"));
    let held = e.idx.album_items(1).unwrap();
    assert_eq!(held.len(), 2);
    assert!(mine.iter().all(|id| held.contains(id)));
    let r = post("/album-add".into(), c1.clone(), body, CROSS).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // A new one named in place, from the viewer's form.
    let body = format!(
        "album=new&name=++Neues+++Album+&ids={}&back=v%2Fp1%2F{}",
        mine[0], mine[0]
    );
    let r = post("/album-add".into(), c1.clone(), body, FORM).await;
    assert!(r.status.is_redirection());
    assert_eq!(e.idx.album(2).unwrap().unwrap().name, "Neues Album");
    assert_eq!(e.idx.album_items(2).unwrap().len(), 1);

    // The pages.
    let page = req_with(&e.app, "GET", "/albums", Some(&c1), false, &[]).await;
    assert!(page.body.contains("<b>Sommer</b><i>2</i>"), "{}", page.body);
    assert!(
        page.body.contains("<b>Neues Album</b><i>1</i>"),
        "{}",
        page.body
    );
    let page = req_with(&e.app, "GET", "/album/1", Some(&c1), false, &[]).await;
    for id in &mine {
        assert!(
            page.body.contains(&format!("id=\"i{id}\"")),
            "{}",
            page.body
        );
    }
    let show = req_with(&e.app, "GET", "/show/p1?album=1", Some(&c1), false, &[]).await;
    assert_eq!(
        show.body.matches("<li data-src=").count(),
        2,
        "{}",
        show.body
    );
    assert!(show.body.contains("class=\"close\" href=\"album/1\""));
    let v = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", mine[0]),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert!(
        v.body.contains("<option value=\"1\">Sommer</option>"),
        "{}",
        v.body
    );
    let tl = req_with(&e.app, "GET", "/t/p1", Some(&c1), false, &[]).await;
    assert!(tl.body.contains("<template id=\"albumopts\"><option value=\"2\">Neues Album</option><option value=\"1\">Sommer</option></template>"), "{}", tl.body);

    // p2 sees p1's photos, not p1's albums.
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-2-unmapped-subject".into();
        m.email = "two@example.test".into();
    }
    let (p2, _) = login(&e, "").await.map_err(|r| r.body).unwrap();
    let c2 = format!("lichtbild_session={p2}");
    let r = req_with(&e.app, "GET", "/album/1", Some(&c2), false, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let page = req_with(&e.app, "GET", "/albums", Some(&c2), false, &[]).await;
    assert!(!page.body.contains("Sommer"), "{}", page.body);
    let show = req_with(&e.app, "GET", "/show/p1?album=1", Some(&c2), false, &[]).await;
    assert_eq!(show.body.matches("<li data-src=").count(), 0);
    let ids = format!("ids={}", mine[0]);
    for (uri, body) in [
        ("/album-add".to_string(), format!("album=1&{ids}")),
        ("/album/1/remove".into(), ids.clone()),
        ("/album/1/rename".into(), "name=Meins".into()),
        ("/album/1/delete".into(), String::new()),
    ] {
        let r = post(uri.clone(), c2.clone(), body, FORM).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(e.idx.album(1).unwrap().unwrap().name, "Sommer");
    assert_eq!(e.idx.album_items(1).unwrap().len(), 2);

    // p1 takes one out, renames, deletes; the pictures stay.
    let r = post(
        "/album/1/remove".into(),
        c1.clone(),
        format!("ids={}%2C{other}", mine[1]),
        FORM,
    )
    .await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "1"));
    let r = post(
        "/album/1/rename".into(),
        c1.clone(),
        "name=Sommer+2021".into(),
        FORM,
    )
    .await;
    assert!(location(&r).ends_with("/album/1"));
    assert_eq!(e.idx.album(1).unwrap().unwrap().name, "Sommer 2021");
    let r = post("/album/1/delete".into(), c1.clone(), String::new(), FORM).await;
    assert!(location(&r).ends_with("/albums"));
    assert!(e.idx.album(1).unwrap().is_none());
    assert!(e.idx.album_items(1).unwrap().is_empty());
    assert!(e.idx.locate(mine[0]).unwrap().is_some());
    // Ids are never given out twice: after the newest album goes, a new
    // one gets a new id, so an old link to it finds nothing.
    let r = post("/album/2/delete".into(), c1.clone(), String::new(), FORM).await;
    assert!(location(&r).ends_with("/albums"));
    let r = post("/albums".into(), c1.clone(), "name=Herbst".into(), FORM).await;
    assert!(location(&r).ends_with("/album/3"), "{}", location(&r));
    let r = req_with(&e.app, "GET", "/album/2", Some(&c1), false, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

// Sharing an album is the owner's choice, off by default: shared, p2 sees
// it among "Shared with you" and opens it (only what p2 may see, without
// what p2 hid), but cannot change it; unshared, it is gone for p2 again.
// rivet: verifies REQ-WEB-017
#[tokio::test]
async fn a_shared_album_is_seen_by_the_house_and_changed_only_by_its_owner() {
    let e = env("https://photos.example").await;
    const FORM: &[(&str, &str)] = &[
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
    }
    let Ok((p1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let c1 = format!("lichtbild_session={p1}");
    let mine: Vec<u64> = e
        .idx
        .all()
        .unwrap()
        .iter()
        .filter(|(_, r)| r.label == "p1" && r.video.is_none())
        .map(|(id, _)| *id)
        .take(2)
        .collect();
    let body = format!("album=new&name=Ferien&ids={}%2C{}", mine[0], mine[1]);
    let r = req_with_body(&e.app, "POST", "/album-add", Some(&c1), &body, FORM).await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "1"));
    assert!(!e.idx.album(1).unwrap().unwrap().shared);

    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-2-unmapped-subject".into();
        m.email = "two@example.test".into();
    }
    let (p2, _) = login(&e, "").await.map_err(|r| r.body).unwrap();
    let c2 = format!("lichtbild_session={p2}");
    // Not shared: nothing for p2.
    let r = req_with(&e.app, "GET", "/album/1", Some(&c2), false, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // p2 cannot share it either.
    let r = req_with_body(&e.app, "POST", "/album/1/share", Some(&c2), "name=on", FORM).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(!e.idx.album(1).unwrap().unwrap().shared);

    // p1 shares it.
    let r = req_with_body(&e.app, "POST", "/album/1/share", Some(&c1), "name=on", FORM).await;
    assert!(location(&r).ends_with("/album/1"), "{}", location(&r));
    let own = req_with(&e.app, "GET", "/albums", Some(&c1), false, &[]).await;
    assert!(
        own.body
            .contains("<b>Ferien</b><i>2</i><small>shared</small>"),
        "{}",
        own.body
    );
    assert!(!own.body.contains("Shared with you"));
    assert_eq!(own.body.matches("<b>Ferien</b>").count(), 1, "{}", own.body);
    let list = req_with(&e.app, "GET", "/albums", Some(&c2), false, &[]).await;
    assert!(
        list.body.contains("<h2>Shared with you</h2>"),
        "{}",
        list.body
    );
    assert!(
        list.body
            .contains("<b>Ferien</b><i>2</i><small>from p1</small>"),
        "{}",
        list.body
    );
    // p2 hides one: the album shows p2 the other only.
    e.idx.set_hidden("p2", mine[1], true).unwrap();
    let page = req_with(&e.app, "GET", "/album/1", Some(&c2), false, &[]).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains(&format!("id=\"i{}\"", mine[0])),
        "{}",
        page.body
    );
    assert!(!page.body.contains(&format!("id=\"i{}\"", mine[1])));
    assert!(page.body.contains("Shared by p1."));
    assert!(!page.body.contains("album/1/rename"), "no tools for p2");
    assert!(!page.body.contains("data-album="), "no removing for p2");
    let show = req_with(&e.app, "GET", "/show/p1?album=1", Some(&c2), false, &[]).await;
    assert_eq!(
        show.body.matches("<li data-src=").count(),
        1,
        "{}",
        show.body
    );
    // Seen, never changed by p2; and not offered to add to.
    let tl = req_with(&e.app, "GET", "/t/p1", Some(&c2), false, &[]).await;
    assert!(!tl.body.contains("Ferien"), "{}", tl.body);
    let ids = format!("ids={}", mine[0]);
    for (uri, body) in [
        ("/album-add".to_string(), format!("album=1&{ids}")),
        ("/album/1/remove".into(), ids.clone()),
        ("/album/1/rename".into(), "name=Meins".into()),
        ("/album/1/share".into(), "name=off".into()),
        ("/album/1/delete".into(), String::new()),
    ] {
        let r = req_with_body(&e.app, "POST", &uri, Some(&c2), &body, FORM).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let a = e.idx.album(1).unwrap().unwrap();
    assert!(a.shared && a.name == "Ferien");
    assert_eq!(e.idx.album_items(1).unwrap().len(), 2);

    // p1 stops sharing: gone for p2.
    let r = req_with_body(
        &e.app,
        "POST",
        "/album/1/share",
        Some(&c1),
        "name=off",
        FORM,
    )
    .await;
    assert!(r.status.is_redirection());
    let r = req_with(&e.app, "GET", "/album/1", Some(&c2), false, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let list = req_with(&e.app, "GET", "/albums", Some(&c2), false, &[]).await;
    assert!(!list.body.contains("Ferien"));
}

/// A face vector pointing mostly along `d`, a little along `e`.
fn face_vec(d: usize, e: usize) -> Vec<i8> {
    let mut v = vec![0i8; lichtbild::faces::DIM];
    v[d] = 100;
    v[e] = 25;
    v
}

fn one_face(v: Vec<i8>) -> Vec<lichtbild::faces::Face> {
    vec![lichtbild::faces::Face {
        b: [0.3, 0.2, 0.2, 0.25],
        score: 0.95,
        v,
    }]
}

// Names are the house's, but a viewer sees a person, their photos, their
// face, their name in suggestions and search only through photos they may
// see; naming and "not them" need a same-site request on a face the viewer
// may see.
// rivet: verifies REQ-FACE-003
#[tokio::test]
async fn names_are_shared_but_each_person_sees_only_their_photos_of_them() {
    let e = env("https://photos.example").await;
    let all = e.idx.all().unwrap();
    let photos = |l: &str| -> Vec<(u64, String)> {
        all.iter()
            .filter(|(_, r)| r.label == l && r.video.is_none() && !r.missing)
            .map(|(id, r)| (*id, r.hash.clone()))
            .collect()
    };
    let (p1, p2) = (photos("p1"), photos("p2"));
    // X and Y (p1) show the same face; Z (p2, which p1 may not see)
    // someone else.
    let (x, y, z) = (&p1[0], &p1[1], &p2[0]);
    e.idx
        .set_faces(&x.1, lichtbild::faces::VERSION, &one_face(face_vec(0, 1)))
        .unwrap();
    e.idx
        .set_faces(&y.1, lichtbild::faces::VERSION, &one_face(face_vec(0, 2)))
        .unwrap();
    e.idx
        .set_faces(&z.1, lichtbild::faces::VERSION, &one_face(face_vec(5, 6)))
        .unwrap();
    let data = e._fx.data.clone();
    for h in [&x.1, &y.1, &z.1] {
        let p = lichtbild::faces::crop_path(&data, h, 0);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, common::jpeg(16, 16, 3)).unwrap();
    }
    e.people.rebuild(&e.idx).unwrap();

    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "someone-else".into();
        m.email = "two@example.test".into();
    }
    let Ok((s2, _)) = login(&e, "").await else {
        panic!("p2 login failed");
    };
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
        m.email = "one@example.test".into();
    }
    let Ok((s1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let (c1, c2) = (
        format!("lichtbild_session={s1}"),
        format!("lichtbild_session={s2}"),
    );

    // p2 sees both: the look-alikes form one group, offered for a name.
    let page = req_with(&e.app, "GET", "/people", Some(&c2), false, &[]).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Who is this?"), "{}", page.body);
    // p2 names the group from Y: X and Y become Oma.
    let body = format!("item={}&face=0&name=%20Oma%20", y.0);
    let r = req_with_body(&e.app, "POST", "/people/name", Some(&c2), &body, &form).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    let oma = e.idx.people().unwrap()[0].0;
    assert_eq!(e.idx.people().unwrap()[0].1.name, "Oma");
    assert_eq!(e.idx.named_faces().unwrap().len(), 2);
    // And Z, alone, as Opa.
    let body = format!("item={}&face=0&one=1&name=Opa", z.0);
    let r = req_with_body(&e.app, "POST", "/people/name", Some(&c2), &body, &form).await;
    assert!(r.status.is_redirection());
    let opa = e
        .idx
        .people()
        .unwrap()
        .into_iter()
        .find(|(_, p)| p.name == "Opa")
        .unwrap()
        .0;
    // Typing the same name again finds the same person, in any case.
    let body = format!("item={}&face=0&one=1&name=oma", x.0);
    req_with_body(&e.app, "POST", "/people/name", Some(&c2), &body, &form).await;
    assert_eq!(e.idx.people().unwrap().len(), 2);

    // p1 sees Oma, and nothing of Opa.
    let page = req_with(&e.app, "GET", "/people", Some(&c1), false, &[]).await;
    assert!(
        page.body.contains("Oma") && !page.body.contains("Opa"),
        "{}",
        page.body
    );
    let r = req_with(
        &e.app,
        "GET",
        &format!("/person/{oma}"),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains(&format!("thumb/{}?", x.0)), "{}", r.body);
    for uri in [
        format!("/person/{opa}"),
        format!("/face/{}/0", z.0),
        format!("/people/{}/0", z.0),
    ] {
        let r = req_with(&e.app, "GET", &uri, Some(&c1), false, &[]).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let r = req_with(&e.app, "GET", "/suggest?q=Op", Some(&c1), true, &[]).await;
    assert!(!r.body.contains("Opa"), "{}", r.body);
    let r = req_with(&e.app, "GET", "/suggest?q=Om", Some(&c1), true, &[]).await;
    assert!(r.body.contains("Oma"), "{}", r.body);
    let r = req_with(&e.app, "GET", "/search?q=Opa", Some(&c1), false, &[]).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "no redirect to someone p1 has no photo of"
    );
    let r = req_with(&e.app, "GET", "/search?q=oma", Some(&c1), false, &[]).await;
    assert!(r.status.is_redirection());
    assert!(location(&r).ends_with(&format!("/person/{oma}")));
    // p1 can neither name, rename, take off nor refuse what it cannot see.
    for (uri, body) in [
        (
            "/people/name".to_string(),
            format!("item={}&face=0&name=Zorro", z.0),
        ),
        (
            "/people/not".to_string(),
            format!("item={}&face=0&person={opa}", z.0),
        ),
        (format!("/person/{opa}/rename"), "name=Zorro".to_string()),
        (format!("/person/{opa}/delete"), String::new()),
    ] {
        let r = req_with_body(&e.app, "POST", &uri, Some(&c1), &body, &form).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(e.idx.person(opa).unwrap().unwrap().name, "Opa");
    // Not from another site.
    let cross = [
        ("sec-fetch-site", "cross-site"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    let body = format!("item={}&face=0&person={oma}", x.0);
    let r = req_with_body(&e.app, "POST", "/people/not", Some(&c1), &body, &cross).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // p1 says X is not Oma: the name comes off X and is not passed back.
    let r = req_with_body(&e.app, "POST", "/people/not", Some(&c1), &body, &form).await;
    assert!(r.status.is_redirection());
    let x_now = e.people.in_photo(&x.1);
    assert_ne!(x_now[0].who, lichtbild::people::Who::Person(oma));
    let r = req_with(
        &e.app,
        "GET",
        &format!("/person/{oma}"),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.body.contains(&format!("thumb/{}?", y.0)) && !r.body.contains(&format!("thumb/{}?", x.0)),
        "{}",
        r.body
    ); // A photo p1 hid is no way to Oma for p1 (p2 still has Y).
    e.idx.set_hidden("p1", y.0, true).unwrap();
    let uri = format!("/person/{oma}");
    let r = req_with(&e.app, "GET", &uri, Some(&c1), false, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = req_with(&e.app, "GET", "/people", Some(&c1), false, &[]).await;
    assert!(!r.body.contains("Oma"), "{}", r.body);
    let r = req_with(&e.app, "GET", "/suggest?q=Om", Some(&c1), true, &[]).await;
    assert!(!r.body.contains("Oma"), "{}", r.body);
    let r = req_with(&e.app, "GET", &uri, Some(&c2), false, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    // The names a name field offers are the viewer's only; a name only
    // someone else has photos of is not joined by naming, it is a new one.
    let r = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", x.0),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert!(r.body.contains("datalist id=\"names\""), "{}", r.body);
    assert!(
        !r.body.contains("value=\"Opa\"") && !r.body.contains("value=\"Oma\""),
        "{}",
        r.body
    );
    let body = format!("item={}&face=0&one=1&name=Opa", x.0);
    let r = req_with_body(&e.app, "POST", "/people/name", Some(&c1), &body, &form).await;
    assert!(r.status.is_redirection());
    assert!(
        !location(&r).ends_with(&format!("/person/{opa}")),
        "{}",
        location(&r)
    );
    let named: Vec<u64> = e
        .idx
        .people()
        .unwrap()
        .into_iter()
        .filter(|(_, p)| p.name == "Opa")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(named.len(), 2, "p1's Opa is not p2's");
    let mine = *named.iter().find(|id| **id != opa).unwrap();
    // Renaming p1's to "Oma", whom p1 no longer sees, does not join them.
    let r = req_with_body(
        &e.app,
        "POST",
        &format!("/person/{mine}/rename"),
        Some(&c1),
        "name=Oma",
        &form,
    )
    .await;
    assert!(
        location(&r).ends_with(&format!("/person/{mine}")),
        "{}",
        location(&r)
    );
    assert!(e.idx.person(oma).unwrap().is_some());
}

/// Logged-in cookies for p1 (sees only p1) and p2 (sees p1 and p2).
async fn two_sessions(e: &Env) -> (String, String) {
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "someone-else".into();
        m.email = "two@example.test".into();
    }
    let Ok((s2, _)) = login(e, "").await else {
        panic!("p2 login failed");
    };
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
        m.email = "one@example.test".into();
    }
    let Ok((s1, _)) = login(e, "").await else {
        panic!("p1 login failed");
    };
    (
        format!("lichtbild_session={s1}"),
        format!("lichtbild_session={s2}"),
    )
}

/// Search as `c` until `done` holds for the page, for up to ten seconds
/// (search learns a correction in the background).
async fn search_until(e: &Env, c: &str, q: &str, done: impl Fn(&str) -> bool) -> String {
    let mut page = String::new();
    for _ in 0..100 {
        page = req_with(
            &e.app,
            "GET",
            &format!("/search?q={q}"),
            Some(c),
            false,
            &[],
        )
        .await
        .body;
        if done(&page) {
            return page;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("search for {q} never came right: {page}");
}

// rivet: verifies REQ-FIX-001, REQ-FIX-002
#[tokio::test]
async fn corrections_are_the_houses_and_seen_only_with_ones_photos() {
    use lichtbild::objects::Object;
    let e = env_with("https://photos.example", true).await;
    let all = e.idx.all().unwrap();
    let first = |l: &str| -> (u64, String) {
        all.iter()
            .find(|(_, r)| r.label == l && r.video.is_none() && !r.missing)
            .map(|(id, r)| (*id, r.hash.clone()))
            .unwrap()
    };
    // X (p1) has a snowboard that is not there, a dog and a misread line;
    // Z (p2, which p1 may not see) a snowboard too.
    let (x, z) = (first("p1"), first("p2"));
    let board = |score: f32, bbox: [f32; 4]| Object {
        kind: 31,
        score,
        bbox,
    };
    let look = |objects: Vec<Object>, line: Option<&str>| lichtbild::vision::Look {
        text: lichtbild::ocr::Text {
            state: if line.is_some() { "ok" } else { "none" }.into(),
            model: "test".into(),
            lines: line
                .map(|t| lichtbild::ocr::Line {
                    quad: [0.1, 0.1, 0.5, 0.1, 0.5, 0.2, 0.1, 0.2],
                    block: 0,
                    text: t.into(),
                    conf: 0.9,
                })
                .into_iter()
                .collect(),
            docs: vec![0],
        },
        objects,
        vector: vec![],
    };
    let dog = Object {
        kind: 16,
        score: 0.9,
        bbox: [0.6, 0.5, 0.9, 0.9],
    };
    e.idx
        .set_look(
            &x.1,
            "test",
            &look(
                vec![board(0.8, [0.1, 0.1, 0.4, 0.4]), dog],
                Some("Sommerfcst"),
            ),
        )
        .unwrap();
    e.idx
        .set_look(
            &z.1,
            "test",
            &look(vec![board(0.84, [0.2, 0.2, 0.5, 0.5])], None),
        )
        .unwrap();
    e.search.rebuild(&e.idx, None).unwrap();
    let (c1, c2) = two_sessions(&e).await;
    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    // A hit links to the photo, a text hit with the words to mark.
    let has = |page: &str, id: u64| {
        page.contains(&format!("/{id}\"")) || page.contains(&format!("/{id}?q="))
    };

    // The viewer offers to correct each find and each line.
    let v = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", x.0),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert_eq!(v.status, StatusCode::OK);
    assert!(v.body.contains("Something wrong?") && v.body.contains("action=\"fix/thing\""));
    assert!(v.body.contains("action=\"fix/text\"") && v.body.contains("Sommerfcst"));
    let p = search_until(&e, &c2, "snowboard", |p| has(p, x.0) && has(p, z.0)).await;
    assert!(has(&p, z.0));

    // p1 says X has no snowboard: gone for the whole house, Z kept.
    let fix = |item: u64, rest: &str| format!("item={item}&n=0&back=v%2Fp1%2F{item}&{rest}");
    let r = req_with_body(
        &e.app,
        "POST",
        "/fix/thing",
        Some(&c1),
        &fix(x.0, "now=wrong"),
        &form,
    )
    .await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    // Not from another site, and not on a photo p1 may not see.
    let r = req_with_body(
        &e.app,
        "POST",
        "/fix/thing",
        Some(&c1),
        &fix(x.0, "now=right"),
        &[("sec-fetch-site", "cross-site"), form[1]],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = req_with_body(
        &e.app,
        "POST",
        "/fix/thing",
        Some(&c1),
        &fix(z.0, "now=wrong"),
        &form,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = req_with_body(
        &e.app,
        "POST",
        "/fix/text",
        Some(&c1),
        &fix(z.0, "now=wrong"),
        &form,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    search_until(&e, &c2, "snowboard", |p| !has(p, x.0) && has(p, z.0)).await;
    let things = req_with(&e.app, "GET", "/things", Some(&c1), false, &[])
        .await
        .body;
    assert!(
        !things.contains("Snowboard") && things.contains("Hund"),
        "{things}"
    );
    let v = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", x.0),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert!(v.body.contains("marked not there") && v.body.contains("No Snowboards in our house"));

    // p2 says Z's snowboard is skis: two wrong snowboards teach a bar
    // above both, and Z is found as skis.
    let r = req_with_body(
        &e.app,
        "POST",
        "/fix/thing",
        Some(&c2),
        &fix(z.0, "now=kind&kind=skis"),
        &form,
    )
    .await;
    assert!(r.status.is_redirection());
    search_until(&e, &c2, "ski", |p| has(p, z.0)).await;
    let fixes = e.idx.fixes().unwrap();
    assert!((fixes.bar(31) - 0.85).abs() < 1e-6, "{}", fixes.bar(31));
    // The list and the download show each person only their photos' part.
    let page1 = req_with(&e.app, "GET", "/corrections", Some(&c1), false, &[])
        .await
        .body;
    let page2 = req_with(&e.app, "GET", "/corrections", Some(&c2), false, &[])
        .await
        .body;
    assert!(
        page1.contains(&format!("/{}\"", x.0)) && !page1.contains(&format!("/{}\"", z.0)),
        "{page1}"
    );
    assert!(
        page2.contains(&format!("/{}\"", z.0)) && page2.contains("Learned"),
        "{page2}"
    );
    let dl1 = req_with(&e.app, "GET", "/corrections.jsonl", Some(&c1), false, &[]).await;
    assert!(dl1.headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .contains("attachment"));
    assert!(
        dl1.body.contains(&x.1) && !dl1.body.contains(&z.1),
        "{}",
        dl1.body
    );
    assert!(dl1.body.contains("\"model\":\"snowboard\""), "{}", dl1.body);
    assert!(
        !dl1.body.contains("user-1") && !dl1.body.contains("example.test"),
        "{}",
        dl1.body
    );

    // A kind never in the house, and back.
    let off = "kind=Hund&off=1&back=things";
    let r = req_with_body(&e.app, "POST", "/fix/off", Some(&c1), off, &form).await;
    assert!(r.status.is_redirection());
    let things = req_with(&e.app, "GET", "/things", Some(&c2), false, &[])
        .await
        .body;
    assert!(!things.contains("Hund"), "{things}");
    req_with_body(
        &e.app,
        "POST",
        "/fix/off",
        Some(&c1),
        "kind=16&back=things",
        &form,
    )
    .await;
    let things = req_with(&e.app, "GET", "/things", Some(&c2), false, &[])
        .await
        .body;
    assert!(things.contains("Hund"), "{things}");

    // A line read wrong is corrected, and search finds the right word.
    let r = req_with_body(
        &e.app,
        "POST",
        "/fix/text",
        Some(&c1),
        &fix(x.0, "now=save&text=Sommerfest"),
        &form,
    )
    .await;
    assert!(r.status.is_redirection());
    search_until(&e, &c1, "sommerfest", |p| has(p, x.0)).await;
    let v = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", x.0),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert!(v.body.contains("read “Sommerfcst”"), "{}", v.body);

    // Taken back, the snowboard is there again.
    req_with_body(
        &e.app,
        "POST",
        "/fix/thing",
        Some(&c1),
        &fix(x.0, "now=undo"),
        &form,
    )
    .await;
    assert!(!e.idx.fixes().unwrap().things.contains_key(&x.1));
    assert_eq!(e.idx.objects(&x.1).unwrap().len(), 2);
}

// A thing the detector does not know (a capybara it calls a bear) gets
// the house's own word: shown, searched and offered again; the same word
// in other letters is the same kind; a word without letters is refused
// with a reason.
// rivet: verifies REQ-FIX-003
#[tokio::test]
async fn a_thing_can_be_named_with_the_houses_own_word() {
    use lichtbild::objects::Object;
    let e = env_with("https://photos.example", true).await;
    let all = e.idx.all().unwrap();
    let mut p1 = all
        .iter()
        .filter(|(_, r)| r.label == "p1" && r.video.is_none() && !r.missing)
        .map(|(id, r)| (*id, r.hash.clone()));
    let (x, y) = (p1.next().unwrap(), p1.next().unwrap());
    let bear = |bbox: [f32; 4]| Object {
        kind: 21,
        score: 0.95,
        bbox,
    };
    let look = |objects: Vec<Object>| lichtbild::vision::Look {
        text: lichtbild::ocr::Text {
            state: "none".into(),
            model: "test".into(),
            lines: vec![],
            docs: vec![],
        },
        objects,
        vector: vec![],
    };
    for h in [&x.1, &y.1] {
        e.idx
            .set_look(
                h,
                "test",
                &look(vec![bear([0.1, 0.1, 0.5, 0.5]), bear([0.5, 0.5, 0.9, 0.9])]),
            )
            .unwrap();
    }
    e.search.rebuild(&e.idx, None).unwrap();
    let (c1, _) = two_sessions(&e).await;
    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    let fix =
        |item: u64, n: usize, rest: &str| format!("item={item}&n={n}&back=v%2Fp1%2F{item}&{rest}");
    let post = |body: String| {
        let (app, c1) = (&e.app, &c1);
        async move { req_with_body(app, "POST", "/fix/thing", Some(c1), &body, &form).await }
    };

    let r = post(fix(x.0, 0, "now=kind&kind=+Capybara+")).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    // Other letters, the same word: one own kind, not two.
    let r = post(fix(y.0, 1, "now=kind&kind=capybara")).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    let fixes = e.idx.fixes().unwrap();
    assert_eq!(fixes.own(), vec![(lichtbild::fixes::OWN_FIRST, "Capybara")]);
    assert_eq!(fixes.kind_of("CAPYBARA"), Some(lichtbild::fixes::OWN_FIRST));
    assert_eq!(
        e.idx.own_kind(" CAPYBARA ").unwrap(),
        Some(lichtbild::fixes::OWN_FIRST),
        "the index too knows the word in other letters"
    );
    // A known kind stays the detector's, not an own word.
    let r = post(fix(y.0, 0, "now=kind&kind=Hund")).await;
    assert!(r.status.is_redirection());
    assert_eq!(e.idx.fixes().unwrap().own().len(), 1);

    let v = req_with(
        &e.app,
        "GET",
        &format!("/v/p1/{}", x.0),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert!(v.body.contains("is a Capybara"), "{}", v.body);
    assert!(
        v.body.contains("title=\"Capybara\""),
        "the box carries the word"
    );
    assert!(
        v.body.contains("<option value=\"Capybara\">"),
        "offered again"
    );
    let things = req_with(&e.app, "GET", "/things", Some(&c1), false, &[])
        .await
        .body;
    assert!(things.contains("<b>Capybara</b>"), "{things}");
    let has = |page: &str, id: u64| {
        page.contains(&format!("/{id}\"")) || page.contains(&format!("/{id}?q="))
    };
    search_until(&e, &c1, "capybara", |p| has(p, x.0) && has(p, y.0)).await;

    // No letters, or too long: refused, and the answer says why.
    for bad in [
        "now=kind&kind=+42+",
        &format!("now=kind&kind={}", "a".repeat(41)),
    ] {
        let r = post(fix(x.0, 1, bad)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(r.body.contains("at most 40 characters"), "{}", r.body);
    }
    assert_eq!(e.idx.fixes().unwrap().own().len(), 1);
}

// The hidden list names files of one's own library only: p2, who sees p1's
// library, gets the date of a hidden p1 photo but not its file name, and a
// word from that name does not find it.
// rivet: verifies REQ-WEB-020
#[tokio::test]
async fn the_hidden_list_names_only_ones_own_files() {
    let e = env("https://photos.example").await;
    let (_, c2) = two_sessions(&e).await;
    let all = e.idx.all().unwrap();
    let first = |l: &str| {
        all.iter()
            .find(|(_, r)| r.label == l && !r.missing)
            .map(|(id, r)| (*id, String::from_utf8_lossy(&r.rel).into_owned()))
            .unwrap()
    };
    let ((theirs, their_path), (mine, my_path)) = (first("p1"), first("p2"));
    e.idx.set_hidden("p2", theirs, true).unwrap();
    e.idx.set_hidden("p2", mine, true).unwrap();
    let page = req_with(&e.app, "GET", "/hidden", Some(&c2), false, &[]).await;
    assert!(
        page.body.contains(&format!("action=\"unhide/{theirs}\"")),
        "{}",
        page.body
    );
    assert!(
        page.body.contains(&format!("title=\"{my_path}\"")),
        "{}",
        page.body
    );
    let their_name = their_path.rsplit('/').next().unwrap();
    assert!(!page.body.contains(their_name), "{}", page.body);
    let word = &their_name[..their_name.rfind('.').unwrap_or(their_name.len())];
    let page = req_with(
        &e.app,
        "GET",
        &format!("/hidden?q={}", word.replace(' ', "+")),
        Some(&c2),
        false,
        &[],
    )
    .await;
    assert!(
        !page.body.contains(&format!("action=\"unhide/{theirs}\"")),
        "{}",
        page.body
    );
}

// Relations between named people: set from a person page, read from both
// sides, close ones first, seen only between people the viewer knows,
// dropped with the name.
// rivet: verifies REQ-FACE-004
#[tokio::test]
async fn relations_between_people_are_seen_only_between_known_people() {
    let e = env("https://photos.example").await;
    let all = e.idx.all().unwrap();
    let photos = |l: &str| -> Vec<(u64, String)> {
        all.iter()
            .filter(|(_, r)| r.label == l && r.video.is_none() && !r.missing)
            .map(|(id, r)| (*id, r.hash.clone()))
            .collect()
    };
    let (p1, p2) = (photos("p1"), photos("p2"));
    let (x, z) = (&p1[0], &p2[0]);
    e.idx
        .set_faces(&x.1, lichtbild::faces::VERSION, &one_face(face_vec(0, 1)))
        .unwrap();
    e.idx
        .set_faces(&z.1, lichtbild::faces::VERSION, &one_face(face_vec(5, 6)))
        .unwrap();
    e.people.rebuild(&e.idx).unwrap();
    let oma = e.idx.create_person("Oma", "t", 0).unwrap();
    let opa = e.idx.create_person("Opa", "t", 0).unwrap();
    e.idx.name_faces(&[(x.1.clone(), 0)], oma).unwrap();
    e.idx.name_faces(&[(z.1.clone(), 0)], opa).unwrap();
    e.people.relabel(&e.idx).unwrap();

    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "someone-else".into();
        m.email = "two@example.test".into();
    }
    let Ok((s2, _)) = login(&e, "").await else {
        panic!("p2 login failed");
    };
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
        m.email = "one@example.test".into();
    }
    let Ok((s1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let (c1, c2) = (
        format!("lichtbild_session={s1}"),
        format!("lichtbild_session={s2}"),
    );
    let post = |uri: String, body: &str, c: String| {
        let body = body.to_string();
        let app = e.app.clone();
        async move { req_with_body(&app, "POST", &uri, Some(&c), &body, &form).await }
    };

    // p2 knows both: Opa is Oma's partner, and Oma's child too (loose).
    let r = post(
        format!("/person/{oma}/relate"),
        "kind=partner&name=opa",
        c2.clone(),
    )
    .await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    let r = post(
        format!("/person/{opa}/relate"),
        "kind=child&name=Oma&weight=1",
        c2.clone(),
    )
    .await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    let mut rels = e.idx.relations().unwrap();
    rels.sort();
    let mut want = vec![((oma, opa, 1), 1), ((oma.min(opa), oma.max(opa), 0), 3)];
    want.sort();
    assert_eq!(rels, want);
    let page = req_with(
        &e.app,
        "GET",
        &format!("/person/{oma}"),
        Some(&c2),
        false,
        &[],
    )
    .await;
    let (partner, parent) = (
        page.body.find("partner of</span>").expect("partner shown"),
        page.body
            .find("parent of</span>")
            .expect("parent shown from Oma's side"),
    );
    assert!(partner < parent, "the close relation comes first");
    assert!(page.body.contains(&format!("person/{oma}?with={opa}")));
    let page = req_with(
        &e.app,
        "GET",
        &format!("/person/{opa}"),
        Some(&c2),
        false,
        &[],
    )
    .await;
    assert!(page.body.contains("child of</span>"), "{}", page.body);
    // Together: no photo has both.
    let page = req_with(
        &e.app,
        "GET",
        &format!("/person/{oma}?with={opa}"),
        Some(&c2),
        false,
        &[],
    )
    .await;
    assert!(
        page.body.contains("Together with <b>Opa</b>"),
        "{}",
        page.body
    );
    assert!(!page.body.contains(&format!("thumb/{}?", x.0)));
    // Oneself, an unknown name or an unknown kind is refused.
    for body in [
        "kind=friend&name=Oma",
        "kind=friend&name=Zorro",
        "kind=boss&name=Opa",
    ] {
        let r = post(format!("/person/{oma}/relate"), body, c2.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}");
    }

    // p1 knows only Oma: no relation to Opa shows, none can be made or dropped.
    let page = req_with(
        &e.app,
        "GET",
        &format!("/person/{oma}"),
        Some(&c1),
        false,
        &[],
    )
    .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("Opa") && !page.body.contains("partner of</span>"));
    let r = post(
        format!("/person/{oma}/relate"),
        "kind=friend&name=Opa",
        c1.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = post(
        format!("/person/{opa}/relate"),
        "kind=friend&name=Oma",
        c1.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let body = format!("a={}&b={}&kind=0", oma.min(opa), oma.max(opa));
    let r = post(format!("/person/{oma}/unrelate"), &body, c1.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(e.idx.relations().unwrap().len(), 2);
    // A cross-site request changes nothing.
    let r = req_with_body(
        &e.app,
        "POST",
        &format!("/person/{oma}/relate"),
        Some(&c2),
        "kind=friend&name=Opa",
        &[
            ("sec-fetch-site", "cross-site"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // p2 drops the partner relation, then the name Opa: nothing is left.
    let r = post(format!("/person/{oma}/unrelate"), &body, c2.clone()).await;
    assert!(r.status.is_redirection());
    assert_eq!(e.idx.relations().unwrap(), vec![((oma, opa, 1), 1)]);
    let r = post(format!("/person/{opa}/delete"), "", c2.clone()).await;
    assert!(r.status.is_redirection());
    assert!(e.idx.relations().unwrap().is_empty());
}

// Hiding: strangers' faces become one nameless hidden person and leave
// "Who is this?" and the photo's faces; a named person can be hidden and
// shown again; hidden people are out of suggestions and relations; only
// someone who sees the person may hide them.
// rivet: verifies REQ-FACE-005
#[tokio::test]
async fn strangers_and_people_can_be_hidden_and_shown_again() {
    let e = env("https://photos.example").await;
    let all = e.idx.all().unwrap();
    let photos = |l: &str| -> Vec<(u64, String)> {
        all.iter()
            .filter(|(_, r)| r.label == l && r.video.is_none() && !r.missing)
            .map(|(id, r)| (*id, r.hash.clone()))
            .collect()
    };
    let (p1, p2) = (photos("p1"), photos("p2"));
    // X and Y (p1): the same stranger; Z (p2): Opa.
    let (x, y, z, w) = (&p1[0], &p1[1], &p2[0], &p1[2]);
    for (h, v) in [
        (&x.1, face_vec(0, 1)),
        (&y.1, face_vec(0, 2)),
        (&z.1, face_vec(5, 6)),
        (&w.1, face_vec(9, 10)),
    ] {
        e.idx
            .set_faces(h, lichtbild::faces::VERSION, &one_face(v))
            .unwrap();
    }
    let data = e._fx.data.clone();
    for h in [&x.1, &y.1, &z.1, &w.1] {
        let p = lichtbild::faces::crop_path(&data, h, 0);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, common::jpeg(16, 16, 3)).unwrap();
    }
    e.people.rebuild(&e.idx).unwrap();
    let opa = e.idx.create_person("Opa", "t", 0).unwrap();
    let oma = e.idx.create_person("Oma", "t", 0).unwrap();
    e.idx.name_faces(&[(z.1.clone(), 0)], opa).unwrap();
    e.idx.name_faces(&[(w.1.clone(), 0)], oma).unwrap();
    e.people.relabel(&e.idx).unwrap();

    let form = [
        ("sec-fetch-site", "same-origin"),
        ("content-type", "application/x-www-form-urlencoded"),
    ];
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "someone-else".into();
        m.email = "two@example.test".into();
    }
    let Ok((s2, _)) = login(&e, "").await else {
        panic!("p2 login failed");
    };
    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
        m.email = "one@example.test".into();
    }
    let Ok((s1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let (c1, c2) = (
        format!("lichtbild_session={s1}"),
        format!("lichtbild_session={s2}"),
    );
    let get = |uri: String, c: String| {
        let app = e.app.clone();
        async move { req_with(&app, "GET", &uri, Some(&c), false, &[]).await }
    };
    let post = |uri: &str, body: String, c: String| {
        let (app, uri) = (e.app.clone(), uri.to_string());
        async move { req_with_body(&app, "POST", &uri, Some(&c), &body, &form).await }
    };

    // The stranger is offered for a name, with a way to hide them.
    let page = get("/people".into(), c2.clone()).await;
    assert!(page.body.contains("Who is this?"), "{}", page.body);
    let group = get(format!("/people/{}/0", x.0), c2.clone()).await;
    assert!(
        group.body.contains("Hide: no one we know"),
        "{}",
        group.body
    );
    let viewer = get(format!("/v/p1/{}", x.0), c2.clone()).await;
    assert!(viewer.body.contains(&format!("face/{}/0", x.0)));

    // Hidden: both faces go to one nameless hidden person.
    let r = post("/people/hide", format!("item={}&face=0", x.0), c2.clone()).await;
    assert!(r.status.is_redirection(), "{} {}", r.status, r.body);
    let gone = e
        .idx
        .people()
        .unwrap()
        .into_iter()
        .find(|(_, p)| p.hidden)
        .expect("a hidden person");
    assert!(gone.1.name.is_empty());
    let named = e.idx.named_faces().unwrap();
    assert_eq!(named.iter().filter(|(_, p)| *p == gone.0).count(), 2);
    let page = get("/people".into(), c2.clone()).await;
    assert!(!page.body.contains("Who is this?"), "{}", page.body);
    assert!(page.body.contains("Hidden (1)") && page.body.contains("Strangers"));
    let viewer = get(format!("/v/p1/{}", x.0), c2.clone()).await;
    assert!(
        !viewer.body.contains(&format!("face/{}/0", x.0)),
        "a hidden face is not listed with the photo"
    );
    assert_eq!(viewer.status, StatusCode::OK, "the photo stays");

    // A named person: hidden, out of suggestions and relations, then back.
    let r = post(
        &format!("/person/{opa}/hide"),
        "hidden=1".into(),
        c2.clone(),
    )
    .await;
    assert!(r.status.is_redirection());
    assert!(e.idx.person(opa).unwrap().unwrap().hidden);
    let r = req_with(&e.app, "GET", "/suggest?q=Op", Some(&c2), true, &[]).await;
    assert!(!r.body.contains("Opa"), "{}", r.body);
    let page = get("/people".into(), c2.clone()).await;
    assert!(page.body.contains("Hidden (2)"), "{}", page.body);
    let page = get(format!("/person/{opa}"), c2.clone()).await;
    assert!(page.body.contains("Show again"), "{}", page.body);
    let r = post(
        &format!("/person/{oma}/relate"),
        "kind=friend&name=Opa".into(),
        c2.clone(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "no relation to a hidden person"
    );
    assert!(e.idx.relations().unwrap().is_empty());
    let r = post(
        &format!("/person/{opa}/hide"),
        "hidden=0".into(),
        c2.clone(),
    )
    .await;
    assert!(r.status.is_redirection());
    let r = req_with(&e.app, "GET", "/suggest?q=Op", Some(&c2), true, &[]).await;
    assert!(r.body.contains("Opa"), "{}", r.body);

    // p1 does not see Opa: cannot hide them; a bad value or a cross-site
    // request changes nothing.
    let r = post(
        &format!("/person/{opa}/hide"),
        "hidden=1".into(),
        c1.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = post(
        &format!("/person/{opa}/hide"),
        "hidden=yes".into(),
        c2.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = req_with_body(
        &e.app,
        "POST",
        &format!("/person/{opa}/hide"),
        Some(&c2),
        "hidden=1",
        &[
            ("sec-fetch-site", "cross-site"),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(!e.idx.person(opa).unwrap().unwrap().hidden);
}

// A story of some days: from the pictures the viewer sees there, with the
// names, things and words lichtbild knows; the same seed tells the same
// story, German or English; another library's days stay closed; a hidden
// person is not named.
// rivet: verifies REQ-STORY-001
// rivet: verifies REQ-STORY-002
#[tokio::test]
async fn a_story_of_some_days_tells_what_was_seen_and_who_was_there() {
    let e = env("https://photos.example").await;
    let all = e.idx.all().unwrap();
    let (xid, xrec) = all
        .iter()
        .find(|(_, r)| r.label == "p1" && r.video.is_none() && !r.missing)
        .map(|(id, r)| (*id, r.clone()))
        .unwrap();
    let day = xrec.taken_local.get(..10).unwrap().to_string();
    let look = lichtbild::vision::Look {
        text: lichtbild::ocr::Text {
            state: "ok".into(),
            model: "t".into(),
            lines: vec![lichtbild::ocr::Line {
                quad: [0.0; 8],
                block: 0,
                text: "Eiscafé Sonne".into(),
                conf: 0.95,
            }],
            docs: vec![0],
        },
        objects: vec![lichtbild::objects::Object {
            kind: 15,
            score: 0.97,
            bbox: [0.1, 0.1, 0.5, 0.5],
        }],
        vector: vec![],
    };
    e.idx.set_look(&xrec.hash, "t", &look).unwrap();
    e.idx
        .set_faces(
            &xrec.hash,
            lichtbild::faces::VERSION,
            &one_face(face_vec(0, 1)),
        )
        .unwrap();
    e.people.rebuild(&e.idx).unwrap();
    let oma = e.idx.create_person("Oma", "t", 0).unwrap();
    e.idx.name_faces(&[(xrec.hash.clone(), 0)], oma).unwrap();
    e.people.relabel(&e.idx).unwrap();

    {
        let mut m = e.mock.lock().unwrap();
        m.sub = "user-1".into();
        m.email = "one@example.test".into();
    }
    let Ok((s1, _)) = login(&e, "").await else {
        panic!("p1 login failed");
    };
    let c1 = format!("lichtbild_session={s1}");
    let get = |uri: String| {
        let (app, c) = (e.app.clone(), c1.clone());
        async move { req_with(&app, "GET", &uri, Some(&c), false, &[]).await }
    };

    let days = get(format!("/days/p1?from={day}&to={day}")).await;
    assert!(
        days.body
            .contains(&format!("story/p1?from={day}&amp;to={day}")),
        "{}",
        days.body
    );
    let uri = format!("/story/p1?from={day}&to={day}");
    let de = get(uri.clone()).await;
    assert_eq!(de.status, StatusCode::OK, "{}", de.body);
    for want in [
        "Oma",
        "Eiscafé Sonne",
        "Katzen",
        "Ausgedacht",
        &format!("<img src=\"original/{xid}\""),
    ] {
        assert!(de.body.contains(want), "{want}: {}", de.body);
    }
    assert_eq!(
        get(uri.clone()).await.body,
        de.body,
        "the same seed, the same story"
    );
    let en = get(format!("{uri}&lang=en")).await;
    assert!(
        en.body.contains("a cat") && en.body.contains("Made up"),
        "{}",
        en.body
    );
    // Not someone else's days, not one day across the years, not nothing.
    assert_eq!(
        get(format!("/story/p2?from={day}&to={day}")).await.status,
        StatusCode::NOT_FOUND
    );
    for bad in ["/story/p1?md=07-05", "/story/p1"] {
        assert_eq!(
            get(bad.to_string()).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    // The same days as a poem: a haiku, then rhymed couplets with the facts.
    let poem = get(format!("{uri}&form=poem")).await;
    assert_eq!(poem.status, StatusCode::OK);
    for want in [
        "<article class=\"story poem\">",
        "<p class=\"haiku\">",
        "Eiscafé Sonne",
        "<br>",
    ] {
        assert!(poem.body.contains(want), "{want}: {}", poem.body);
    }
    assert!(de.body.contains("📜 Gedicht") && poem.body.contains("📖 Geschichte"));
    // A hidden person is not named.
    e.idx.set_person_hidden(oma, true).unwrap();
    assert!(!get(uri).await.body.contains("Oma"));
}
