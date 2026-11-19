//! Server-rendered timeline with htmx (REQ-WEB-001..004, DD-003).
//! Every response carries a same-origin Content-Security-Policy (SSC-003);
//! files are served by item id only, with path containment (SSC-004).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Extension;
use axum::Router;
use serde::Deserialize;

use crate::auth::Viewer;
use crate::config::Config;
use crate::index::{self, Index};
use crate::media::DateSource;

const HTMX: &str = include_str!("../assets/htmx-2.0.4.min.js");
const CSS: &str = include_str!("../assets/lichtbild.css");
const JS: &str = include_str!("../assets/lichtbild.js");
const PAGE_SIZE: u32 = 240;

pub const CSP: &str = "default-src 'self'; img-src 'self'; media-src 'self'; script-src 'self'; \
style-src 'self'; connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'self'; \
form-action 'self'; frame-ancestors 'none'";

#[derive(Clone)]
pub struct AppState {
    pub index: Arc<Index>,
    pub data_dir: PathBuf,
    pub base_path: String,
    pub roots: Arc<HashMap<String, PathBuf>>,
    pub labels: Arc<Vec<String>>,
    pub allowed_hosts: Arc<Vec<String>>,
    /// Offline place names, when the deployment supplies GeoNames files.
    pub places: Option<Arc<crate::places::Places>>,
    pub max_file_bytes: u64,
    /// Login through the house OIDC provider; None in operator mode
    /// (loopback only, every library visible).
    pub auth: Option<Arc<crate::auth::Auth>>,
    /// The nav v1 switcher's HTML, rendered in every page's header.
    pub nav_html: Arc<String>,
    /// The search index (REQ-SEARCH-001), rebuilt by the scan thread.
    pub search: Arc<crate::search::Search>,
    /// Duplicate groups (REQ-DUP-001), rebuilt by the scan thread.
    pub dups: Arc<crate::dups::Dups>,
    /// What the scan thread is doing (REQ-WEB-009).
    pub status: Arc<crate::status::Status>,
    /// Positions, recorded, set by hand or estimated (REQ-PLACE-002).
    pub positions: Arc<crate::place::Cache>,
    /// The speech model's name, when transcripts are on.
    pub transcribe_model: Option<String>,
    /// Text in pictures is read (REQ-TEXT-001).
    pub ocr_on: bool,
    /// The world map's files and positions cache (REQ-MAP-001).
    pub map: Arc<crate::map::MapState>,
}

impl AppState {
    pub fn new(cfg: &Config, index: Arc<Index>) -> Self {
        AppState {
            index,
            data_dir: cfg.data_dir.clone(),
            base_path: cfg.base_path.clone(),
            roots: Arc::new(
                cfg.roots
                    .iter()
                    .map(|r| (r.label.clone(), r.path.clone()))
                    .collect(),
            ),
            labels: Arc::new(cfg.roots.iter().map(|r| r.label.clone()).collect()),
            allowed_hosts: Arc::new(cfg.allowed_hosts.clone()),
            places: None,
            max_file_bytes: cfg.limits.max_file_bytes,
            auth: None,
            nav_html: Arc::new(cfg.nav_html.clone()),
            search: Arc::new(crate::search::Search::default()),
            dups: Arc::new(crate::dups::Dups::default()),
            status: Arc::new(crate::status::Status::default()),
            transcribe_model: cfg.transcribe.as_ref().map(|t| t.model.to_string()),
            positions: Arc::new(crate::place::Cache::default()),
            ocr_on: cfg.ocr_dir.is_some(),
            map: Arc::new(crate::map::MapState::new(&cfg.map)),
        }
    }

    pub fn with_auth(mut self, auth: Option<crate::auth::Auth>) -> Self {
        self.auth = auth.map(Arc::new);
        self
    }

    pub fn with_places(mut self, places: Option<crate::places::Places>) -> Self {
        self.places = places.map(Arc::new);
        self
    }
}

/// The full router, nested under the base path when one is set.
pub fn router(state: AppState) -> Router {
    let base = state.base_path.clone();
    let app: Router<AppState> = Router::new()
        .route("/", get(home))
        .route("/t/{who}", get(timeline))
        .route("/v/{who}/{id}", get(viewer))
        .route("/search", get(search))
        .route("/hide/{id}", axum::routing::post(hide))
        .route("/unhide/{id}", axum::routing::post(unhide))
        .route("/keep/{id}", axum::routing::post(keep))
        .route("/hidden", get(hidden_page))
        .route("/status", get(status_page))
        .route("/place/{id}", axum::routing::post(place))
        .route("/unplace/{id}", axum::routing::post(unplace))
        .route("/words", get(words_page))
        .route("/duplicates", get(duplicates_page))
        .route("/duplicates.csv", get(duplicates_csv))
        .route("/thumb/{id}", get(thumb))
        .route("/original/{id}", get(original))
        .route("/preview/{id}", get(preview))
        .route("/static/{file}", get(static_file))
        .route("/map", get(crate::map::page))
        .route("/map/clusters", get(crate::map::clusters_json))
        .route("/map/tiles.pmtiles", get(crate::map::tiles))
        .route("/map/assets/{*path}", get(crate::map::assets))
        .route("/auth/login", get(auth_login))
        .route("/auth/callback", get(auth_callback))
        .route("/auth/logout", axum::routing::post(auth_logout))
        .route("/healthz", get(|| async { "ok" }));
    // With a base path, "/base", "/base/" and "/base/..." reach the app;
    // nothing else does.
    let app = if base.is_empty() {
        app
    } else {
        Router::new()
            .route(&format!("{base}/"), get(home))
            .nest(&base, app)
    };
    let hosts = state.allowed_hosts.clone();
    let login_state = state.clone();
    app.with_state(state)
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let st = login_state.clone();
            async move { require_login(&st, req, next).await }
        }))
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let hosts = hosts.clone();
            async move { check_host(&hosts, req, next).await }
        }))
        .layer(middleware::from_fn(security_headers))
}

/// The host part of a Host header or authority: "[::1]:8080" -> "::1",
/// "Photos.Example:443" -> "photos.example".
pub fn host_name(authority: &str) -> String {
    let a = authority.trim();
    let h = if let Some(rest) = a.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        a.rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(a, |(h, _)| h)
    };
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// REQ-WEB-005: answer only requests addressed to localhost or a configured
/// host name, so a page on another site cannot reach lichtbild through DNS
/// rebinding while there is no login.
async fn check_host(allowed: &[String], req: Request, next: Next) -> Response {
    let authority = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    let ok = authority.is_some_and(|a| {
        let h = host_name(&a);
        matches!(h.as_str(), "localhost" | "127.0.0.1" | "::1") || allowed.contains(&h)
    });
    if ok {
        next.run(req).await
    } else {
        (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response()
    }
}

/// REQ-AUTH-001: deny by default. Every route needs a session except the
/// health check, the embedded static assets and the login routes. The
/// request then carries the Viewer: the libraries this person may see.
async fn require_login(st: &AppState, mut req: Request, next: Next) -> Response {
    let Some(auth) = &st.auth else {
        // Operator mode: no login configured, loopback only, all visible.
        req.extensions_mut().insert(Viewer {
            labels: st.labels.to_vec(),
            name: None,
        });
        return next.run(req).await;
    };
    let path = req.uri().path();
    let rel = path.strip_prefix(st.base_path.as_str()).unwrap_or(path);
    let public = rel == "/healthz" || rel.starts_with("/static/") || rel.starts_with("/auth/");
    if public {
        return next.run(req).await;
    }
    if let Some(v) = session_cookie(req.headers()).and_then(|sid| auth.session(&sid)) {
        req.extensions_mut().insert(v);
        return next.run(req).await;
    }
    // Not signed in: browsers go to the provider, htmx swaps reload there.
    let target = req
        .uri()
        .path_and_query()
        .map(|pq| {
            pq.as_str()
                .strip_prefix(st.base_path.as_str())
                .unwrap_or(pq.as_str())
                .to_string()
        })
        .unwrap_or_default();
    let next_q: String =
        openidconnect::url::form_urlencoded::byte_serialize(target.as_bytes()).collect();
    let login = format!("{}/auth/login?next={next_q}", st.base_path);
    if req.headers().contains_key("hx-request") {
        let mut r = StatusCode::UNAUTHORIZED.into_response();
        if let Ok(v) = HeaderValue::from_str(&login) {
            r.headers_mut().insert("hx-redirect", v);
        }
        return r;
    }
    if req.method() == axum::http::Method::GET {
        return Redirect::to(&login).into_response();
    }
    StatusCode::UNAUTHORIZED.into_response()
}

fn session_cookie(h: &axum::http::HeaderMap) -> Option<String> {
    cookie_value(h, crate::auth::COOKIE)
}

fn cookie_value(h: &axum::http::HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// The login-binding cookie, scoped to the login routes, 10 minutes.
fn login_cookie_header(st: &AppState, value: &str, max_age: u64) -> String {
    let secure = st.auth.as_ref().is_some_and(|a| a.secure_cookies());
    format!(
        "{}={value}; Path={}/auth; Max-Age={max_age}; HttpOnly; SameSite=Lax{}",
        crate::auth::LOGIN_COOKIE,
        st.base_path,
        if secure { "; Secure" } else { "" }
    )
}

fn cookie_header(st: &AppState, value: &str, max_age: u64) -> String {
    let path = if st.base_path.is_empty() {
        "/"
    } else {
        st.base_path.as_str()
    };
    let secure = st.auth.as_ref().is_some_and(|a| a.secure_cookies());
    format!(
        "{}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{}",
        crate::auth::COOKIE,
        if secure { "; Secure" } else { "" }
    )
}

#[derive(Deserialize)]
struct LoginQuery {
    next: Option<String>,
}

async fn auth_login(State(st): State<AppState>, Query(q): Query<LoginQuery>) -> Response {
    let Some(auth) = &st.auth else {
        return Redirect::to(&format!("{}/", st.base_path)).into_response();
    };
    match auth.login_url(q.next.as_deref().unwrap_or("")).await {
        Ok((url, binding)) => {
            let mut r = Redirect::to(&url).into_response();
            if let Ok(v) = HeaderValue::from_str(&login_cookie_header(&st, &binding, 600)) {
                r.headers_mut().insert(header::SET_COOKIE, v);
            }
            r
        }
        Err(e) => {
            tracing::error!("login: {e:#}");
            notice(
                &st,
                StatusCode::BAD_GATEWAY,
                "Login is unavailable",
                "The house login service could not be reached. Try again in a moment.",
            )
        }
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

async fn auth_callback(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    use crate::auth::LoginError;
    let Some(auth) = &st.auth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return notice(
            &st,
            StatusCode::BAD_REQUEST,
            "Login was not completed",
            "<a href=\"auth/login\">Sign in again</a>",
        );
    };
    let binding = cookie_value(&headers, crate::auth::LOGIN_COOKIE);
    match auth.callback(&code, &state, binding.as_deref()).await {
        Ok((sid, next)) => {
            let mut r = Redirect::to(&format!("{}/{next}", st.base_path)).into_response();
            for c in [
                cookie_header(&st, &sid, 30 * 24 * 3600),
                login_cookie_header(&st, "", 0),
            ] {
                if let Ok(v) = HeaderValue::from_str(&c) {
                    r.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            r
        }
        Err(LoginError::UnknownState) => notice(
            &st,
            StatusCode::BAD_REQUEST,
            "This login has expired",
            "<a href=\"auth/login\">Sign in again</a>",
        ),
        Err(LoginError::NotAPerson) => notice(
            &st,
            StatusCode::FORBIDDEN,
            "No library for this account",
            "This account is signed in, but lichtbild has no photo library for it.",
        ),
        Err(LoginError::InvalidToken(e)) => {
            tracing::error!("login: invalid ID token: {e}");
            notice(
                &st,
                StatusCode::BAD_GATEWAY,
                "Login failed",
                "<a href=\"auth/login\">Try again</a>",
            )
        }
        Err(LoginError::Provider(e)) => {
            tracing::error!("login: provider: {e:#}");
            notice(
                &st,
                StatusCode::BAD_GATEWAY,
                "Login is unavailable",
                "The house login service could not be reached. Try again in a moment.",
            )
        }
    }
}

async fn auth_logout(State(st): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    // A sign-out form on another site must not sign anyone out: browsers
    // label such requests cross-site.
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    if site.is_some_and(|s| s != "same-origin" && s != "none") {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let (Some(auth), Some(sid)) = (&st.auth, session_cookie(&headers)) {
        if let Err(e) = auth.logout(&sid) {
            tracing::error!("logout: stored session not removed: {e:#}");
            return notice(
                &st,
                StatusCode::INTERNAL_SERVER_ERROR,
                "Sign-out failed",
                "The session could not be ended. <a href=\"./\">Back</a>",
            );
        }
    }
    let mut r = notice(
        &st,
        StatusCode::OK,
        "Signed out",
        "<a href=\"./\">Sign in again</a>",
    );
    if let Ok(v) = HeaderValue::from_str(&cookie_header(&st, "", 0)) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

/// A small page for login outcomes; `html` is trusted markup from this file.
/// It is shown before or without a session, so it carries no version.
fn notice(st: &AppState, status: StatusCode, title: &str, html: &str) -> Response {
    let body = format!("<h1>{}</h1>\n<p>{html}</p>", esc(title));
    (
        status,
        page(&st.base_path, title, &body, &st.nav_html, "", "", ""),
    )
        .into_response()
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // Pages are asked for again on every visit: a page kept from before an
    // upgrade (sessions do not survive a restart) showed no thumbnails,
    // because each one was redirected to the login. Static files carry
    // their version in the URL and keep their own caching.
    let html = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if html && !h.contains_key(header::CACHE_CONTROL) {
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, private"),
        );
    }
    res
}

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// A short hash of the embedded CSS and JS. It goes into their URLs, so a
/// browser that cached them for a day still fetches the new ones after an
/// upgrade.
pub fn asset_version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let mut h = blake3::Hasher::new();
        h.update(CSS.as_bytes());
        h.update(JS.as_bytes());
        h.update(crate::map::MAP_JS.as_bytes());
        h.finalize().to_hex()[..10].to_string()
    })
}

/// The footer of pages behind login: which lichtbild build serves them, so
/// the household can tell what is running. Pages without a session do not
/// show it.
fn version_footer() -> String {
    format!(
        "<footer class=\"version\">lichtbild {}</footer>",
        esc(crate::VERSION)
    )
}

/// The app's menu (one button, every page behind login): where to go in
/// lichtbild. The house's other apps stay in the nav switcher beside it.
fn app_menu(st: &AppState) -> String {
    let signout = if st.auth.is_some() {
        "<li class=\"sep\"><form method=\"post\" action=\"auth/logout\"><button type=\"submit\">Sign out</button></form></li>"
    } else {
        ""
    };
    format!(
        "<details class=\"appmenu\"><summary aria-label=\"Menu\" title=\"Menu\"><span class=\"burger\" aria-hidden=\"true\"></span></summary>\
         <ul>\
         <li><a href=\"./\"><b>Home</b><span>Timelines, on this day</span></a></li>\
         <li><a href=\"map\"><b>Map</b><span>Where pictures were taken</span></a></li>\
         <li><a href=\"words\"><b>Words</b><span>What was said in videos</span></a></li>\
         <li class=\"sep\"><a href=\"hidden\"><b>Hidden</b><span>What you chose not to see</span></a></li>\
         <li><a href=\"duplicates\"><b>Duplicate files</b><span>Identical files to clean up</span></a></li>\
         <li><a href=\"status\"><b>Status</b><span>Scan, transcripts, version</span></a></li>\
         {signout}</ul></details>"
    )
}

/// A page with this app's header: the name, the menu and the nav switcher.
fn page_for(st: &AppState, title: &str, body: &str) -> Html<String> {
    page(
        &st.base_path,
        title,
        body,
        &st.nav_html,
        "",
        &version_footer(),
        &app_menu(st),
    )
}

/// A page with extra same-origin stylesheets or scripts in its head (the map).
pub(crate) fn page_with_head(st: &AppState, title: &str, head: &str, body: &str) -> Html<String> {
    page(
        &st.base_path,
        title,
        body,
        &st.nav_html,
        head,
        &version_footer(),
        &app_menu(st),
    )
}

fn page(
    base: &str,
    title: &str,
    body: &str,
    nav: &str,
    head: &str,
    footer: &str,
    menu: &str,
) -> Html<String> {
    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<base href="{base}/">
<title>{title}</title>
<meta name="htmx-config" content='{{"includeIndicatorStyles":false}}'>
{head}<link rel="stylesheet" href="static/lichtbild.css?v={v}">
<script src="static/htmx-2.0.4.min.js" defer></script>
<script src="static/lichtbild.js?v={v}" defer></script>
</head>
<body>
<header><a href="./" class="brand">lichtbild</a>{menu}<form class="search" action="search" method="get" role="search"><input type="search" name="q" placeholder="Search" aria-label="Search" maxlength="200"></form>{nav}</header>
<main>
{body}
</main>
{footer}
</body>
</html>
"#,
        base = esc(base),
        title = esc(title),
        v = asset_version(),
    ))
}

pub(crate) fn internal<E: std::fmt::Display>(e: E) -> Response {
    tracing::error!("request failed: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
}

/// The libraries a timeline shows for this viewer: "all" is every library
/// they may see (the merged view, only when that is more than one), a
/// label only when they may see it.
/// The photo/video filter (REQ-WEB-010): Some(true) for videos only,
/// Some(false) for photos only, None for both; and its query suffix.
pub fn kind_filter(kind: Option<&str>) -> (Option<bool>, &'static str) {
    match kind {
        Some("video") => (Some(true), "&amp;kind=video"),
        Some("photo") => (Some(false), "&amp;kind=photo"),
        _ => (None, ""),
    }
}

/// Percent-encode a query value (RFC 3986 unreserved characters stay).
pub fn urlencode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o += &format!("%{b:02X}"),
        }
    }
    o
}

/// The filter's suffix ("&amp;kind=video") as the query of a viewer link
/// ("?kind=video"), so the viewer walks and returns to the same items.
pub fn viewer_query(kq: &str) -> String {
    match kq.strip_prefix("&amp;") {
        Some(q) => format!("?{q}"),
        None => String::new(),
    }
}

/// The All / Photos / Videos switch for `base` ("t/p1", "search?q=x").
pub fn kind_switch(base: &str, kind: Option<bool>) -> String {
    let sep = if base.contains('?') { "&amp;" } else { "?" };
    let item = |label: &str, href: String, on: bool| {
        format!(
            "<a href=\"{href}\"{}>{label}</a>",
            if on {
                " class=\"on\" aria-current=\"true\""
            } else {
                ""
            }
        )
    };
    format!(
        "<nav class=\"kinds\" aria-label=\"Show\">{}{}{}</nav>",
        item("All", base.to_string(), kind.is_none()),
        item(
            "Photos",
            format!("{base}{sep}kind=photo"),
            kind == Some(false)
        ),
        item(
            "Videos",
            format!("{base}{sep}kind=video"),
            kind == Some(true)
        ),
    )
}

/// Whose choices (hidden items) apply: the signed-in person, or the
/// operator when there is no login.
pub fn person(viewer: &Viewer) -> String {
    viewer
        .name
        .clone()
        .unwrap_or_else(|| "operator".to_string())
}

pub fn scope(viewer: &Viewer, who: &str) -> Option<Vec<String>> {
    if who == "all" {
        (viewer.labels.len() > 1).then(|| viewer.labels.clone())
    } else {
        viewer.may_see(who).then(|| vec![who.to_string()])
    }
}

/// The home page's cards and its "on this day" pictures.
type Home = (Vec<Card>, Vec<(u64, index::Record)>);

/// One card on the home page: a library (or the merged view), its newest
/// pictures, how many it holds and which years.
struct Card {
    who: String,
    title: String,
    count: u64,
    years: Option<(String, String)>,
    latest: Vec<index::Item>,
}

async fn home(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let idx = st.index.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let labels = viewer.labels.clone();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Home> {
        let counts = idx.counts()?;
        let hidden = idx.hidden(&me)?;
        let mut scopes: Vec<(String, String, Vec<String>)> = labels
            .iter()
            .map(|l| (l.clone(), l.clone(), vec![l.clone()]))
            .collect();
        if labels.len() > 1 {
            scopes.push(("all".into(), "Everyone".into(), labels.clone()));
        }
        let mut cards = Vec::new();
        for (who, title, scope) in scopes {
            let months = idx.months(&scope)?;
            let latest = idx.timeline_where(&scope, None, 4, |id, rec| {
                rec.thumb == "ok" && dups.shown(id, &scope, &hidden).shown
            })?;
            cards.push(Card {
                who,
                title,
                count: scope
                    .iter()
                    .map(|l| counts.get(l).copied().unwrap_or(0))
                    .sum(),
                years: match (months.last(), months.first()) {
                    (Some(old), Some(new)) => Some((
                        old.0.get(..4).unwrap_or("").to_string(),
                        new.0.get(..4).unwrap_or("").to_string(),
                    )),
                    _ => None,
                },
                latest,
            });
        }
        // On this day, in earlier years: one copy, nothing hidden.
        let otd = idx
            .on_this_day(
                &labels,
                today.get(5..10).unwrap_or(""),
                today.get(..4).unwrap_or(""),
                60,
            )?
            .into_iter()
            .filter(|(id, r)| r.thumb == "ok" && dups.shown(*id, &labels, &hidden).shown)
            .take(14)
            .collect();
        Ok((cards, otd))
    })
    .await;
    let (cards, otd) = match res {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let thumb = |id: u64, hash: &str| {
        format!(
            "<img src=\"thumb/{id}?h={}\" loading=\"lazy\" alt=\"\">",
            esc(hash.get(..12).unwrap_or(""))
        )
    };
    let mut body = String::from("<section class=\"cards\">");
    for c in &cards {
        let mut mosaic = String::new();
        for it in &c.latest {
            mosaic += &thumb(it.id, &it.hash);
        }
        for _ in c.latest.len()..4 {
            mosaic += "<span></span>";
        }
        let years = match &c.years {
            Some((a, b)) if a == b => a.clone(),
            Some((a, b)) => format!("{a}–{b}"),
            None => String::new(),
        };
        body += &format!(
            "<a class=\"card\" href=\"t/{who}\"><div class=\"mosaic\">{mosaic}</div>\
             <div class=\"cmeta\"><b>{title}</b><span class=\"n\">{n}</span><span class=\"years\">{years}</span></div></a>",
            who = esc(&c.who),
            title = esc(&c.title),
            n = c.count,
            years = esc(&years),
        );
    }
    body += "</section>";
    if !otd.is_empty() {
        let who = if viewer.labels.len() > 1 {
            "all".to_string()
        } else {
            viewer.labels[0].clone()
        };
        body += "<section class=\"otd\"><h2>On this day</h2><div class=\"strip\">";
        for (id, r) in &otd {
            body += &format!(
                "<a class=\"item\" href=\"v/{who}/{id}\" title=\"{t}\">{img}<span class=\"year\">{y}</span></a>",
                who = esc(&who),
                t = esc(&r.taken_local.replace('T', " ")),
                img = thumb(*id, &r.hash),
                y = esc(r.taken_local.get(..4).unwrap_or("")),
            );
        }
        body += "</div></section>";
    }
    body += "<nav class=\"tiles\">\
             <a href=\"map\"><b>Map</b><span>Where pictures were taken</span></a>\
             <a href=\"words\"><b>Words</b><span>What was said in videos</span></a>\
             <a href=\"status\"><b>Status</b><span>Scan and transcripts</span></a></nav>";
    page_for(&st, "lichtbild", &body).into_response()
}

#[derive(Deserialize)]
struct Cursor {
    before: Option<String>,
    id: Option<u64>,
    day: Option<String>,
    /// "photo" or "video": only those (REQ-WEB-010).
    kind: Option<String>,
    /// Start at this year ("2023") or month ("2023-05") and go back in time.
    from: Option<String>,
}

/// `from=YYYY` or `from=YYYY-MM` -> the exclusive upper bound of the page:
/// the first instant after that year or month.
pub fn parse_from(v: &str) -> Option<String> {
    let b = v.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    match b.len() {
        4 if digits(0..4) => {
            let y: u32 = v.parse().ok()?;
            Some(format!("{:04}-01-01T00:00:00", y + 1))
        }
        7 if digits(0..4) && b[4] == b'-' && digits(5..7) => {
            let y: u32 = v[..4].parse().ok()?;
            let m: u32 = v[5..].parse().ok()?;
            if !(1..=12).contains(&m) {
                return None;
            }
            let (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            Some(format!("{y:04}-{m:02}-01T00:00:00"))
        }
        _ => None,
    }
}

/// The date scrubber: a rail along the right edge on wide screens (ticks
/// sized by item count, see assets/lichtbild.js) and a "Jump to date" menu
/// on phones. Both are plain links, so they work without JavaScript.
pub fn render_jump(who: &str, months: &[(String, u64)], kq: &str) -> String {
    if months.is_empty() {
        return String::new();
    }
    let who = esc(who);
    let name = |ym: &str| {
        let m: usize = ym.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(0);
        MONTHS.get(m.wrapping_sub(1)).copied().unwrap_or("")
    };
    // The rail: month ticks, for a mouse on a wide screen (CSS hides it on
    // touch screens, where its ticks are too fine to hit).
    let mut rail = String::from("<nav class=\"rail\" aria-label=\"Jump to date\">");
    let mut year = "";
    for (ym, n) in months {
        let y = ym.get(..4).unwrap_or("");
        if y != year {
            year = y;
            rail += &format!(
                "<a class=\"y\" href=\"t/{who}?from={y}{kq}\">{y}</a>",
                y = esc(y)
            );
        }
        let short = name(ym).get(..3).unwrap_or("");
        rail += &format!(
            "<a class=\"m\" data-n=\"{n}\" data-ym=\"{ym}\" href=\"t/{who}?from={ym}{kq}\" title=\"{full} {y} · {n}\"><span>{short}</span></a>",
            ym = esc(ym),
            full = name(ym),
            y = esc(y),
        );
    }
    rail += "</nav>";

    // The chooser, on every screen: a floating button that names the month
    // in view, opening the years, newest first; a year opens its twelve
    // months (empty ones greyed). It stays one row per year however many
    // years the library spans.
    let mut years: Vec<(&str, [u64; 12])> = Vec::new();
    for (ym, n) in months {
        let y = ym.get(..4).unwrap_or("");
        let m: usize = ym.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(0);
        if years.last().is_none_or(|(ly, _)| *ly != y) {
            years.push((y, [0; 12]));
        }
        if let (Some(last), 1..=12) = (years.last_mut(), m) {
            last.1[m - 1] += n;
        }
    }
    let mut menu = String::from(
        "<details class=\"jumpmenu\"><summary><span class=\"jnow\">Jump to date</span></summary><div class=\"jm\">",
    );
    for (i, (y, counts)) in years.iter().enumerate() {
        let total: u64 = counts.iter().sum();
        menu += &format!(
            "<details class=\"jyear\" data-y=\"{y}\"{open}><summary><span class=\"y\">{y}</span><span class=\"n\">{total}</span></summary><div class=\"months\">",
            y = esc(y),
            open = if i == 0 { " open" } else { "" },
        );
        for (mi, n) in counts.iter().enumerate() {
            let short = MONTHS[mi].get(..3).unwrap_or("");
            if *n > 0 {
                menu += &format!(
                    "<a href=\"t/{who}?from={y}-{m:02}{kq}\" data-ym=\"{y}-{m:02}\">{short}<span class=\"n\">{n}</span></a>",
                    y = esc(y),
                    m = mi + 1,
                );
            } else {
                menu += &format!("<span class=\"none\">{short}</span>");
            }
        }
        menu += &format!(
            "</div><a class=\"whole\" href=\"t/{who}?from={y}{kq}\">All of {y}</a></details>",
            y = esc(y)
        );
    }
    menu += "</div></details>";
    rail + &menu
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Render items grouped by year, month and day; `prev_day` is the last day
/// already on the page, so a continuation does not repeat headings.
pub fn render_items(
    who: &str,
    items: &[index::Item],
    prev_day: Option<&str>,
    more: bool,
    kq: &str,
) -> String {
    let mut s = String::new();
    let mut prev = prev_day.map(str::to_string);
    let mut open = prev.is_some();
    if open {
        s += "<div class=\"grid\">";
    }
    for it in items {
        let day = it.taken_local.get(..10).unwrap_or("").to_string();
        if prev.as_deref() != Some(day.as_str()) {
            if open {
                s += "</div>\n";
            }
            let p = prev.as_deref().unwrap_or("");
            if p.get(..4) != day.get(..4) {
                s += &format!(
                    "<h2 class=\"year\">{}</h2>\n",
                    esc(day.get(..4).unwrap_or(""))
                );
            }
            if p.get(..7) != day.get(..7) {
                let m: usize = day.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(0);
                let name = MONTHS.get(m.wrapping_sub(1)).copied().unwrap_or("");
                s += &format!("<h3 class=\"month\">{name}</h3>\n");
            }
            s += &format!("<h4 class=\"day\">{}</h4>\n<div class=\"grid\">", esc(&day));
            open = true;
            prev = Some(day);
        }
        let weak = DateSource::is_weak(&it.date_source);
        let title = if weak {
            format!("{} (date from {})", it.taken_local, it.date_source)
        } else {
            it.taken_local.clone()
        };
        let img = if it.thumb_ok {
            format!(
                "<img src=\"thumb/{id}?h={hv}\" loading=\"lazy\" alt=\"\" width=\"{w}\" height=\"{h}\">",
                id = it.id,
                hv = esc(it.hash.get(..12).unwrap_or("")),
                w = it.width.min(360),
                h = (u64::from(it.height) * u64::from(it.width.min(360))
                    / u64::from(it.width.max(1))),
            )
        } else if it.video_ms.is_some() {
            "<span class=\"nothumb\">video</span>".to_string()
        } else {
            "<span class=\"nothumb\">no preview</span>".to_string()
        };
        let dur = it
            .video_ms
            .map(|ms| format!("<span class=\"dur\">{}</span>", duration(ms)))
            .unwrap_or_default();
        let dur = if it.copies > 1 {
            format!(
                "{dur}<span class=\"copies\" title=\"{n} copies\">×{n}</span>",
                n = it.copies
            )
        } else {
            dur
        };
        s += &format!(
            "<a class=\"item{cls}{vid}\" id=\"i{id}\" href=\"v/{who}/{id}{vq}\" title=\"{t}\">{img}{dur}</a>",
            cls = if weak { " weak" } else { "" },
            vid = if it.video_ms.is_some() { " video" } else { "" },
            id = it.id,
            who = esc(who),
            t = esc(&title),
            vq = viewer_query(kq),
        );
    }
    if open {
        s += "</div>\n";
    }
    if more {
        if let (Some(last), Some(day)) = (items.last(), prev.as_deref()) {
            s += &format!(
                "<div class=\"more\" hx-get=\"t/{who}?before={b}&amp;id={id}&amp;day={d}{kq}\" hx-trigger=\"revealed\" hx-swap=\"outerHTML\">loading…</div>\n",
                who = esc(who),
                b = esc(&last.taken_local),
                id = last.id,
                d = esc(day),
            );
        }
    }
    s
}

async fn timeline(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(who): UrlPath<String>,
    Query(cur): Query<Cursor>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(labels) = scope(&viewer, &who) else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    let from_bound = match cur.from.as_deref() {
        None => None,
        Some(f) => match parse_from(f) {
            Some(b) => Some(b),
            None => {
                return (StatusCode::BAD_REQUEST, "from: expected YYYY or YYYY-MM").into_response()
            }
        },
    };
    let full_page = !headers.contains_key("hx-request");
    let idx = st.index.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let (kind, kq) = kind_filter(cur.kind.as_deref());
    // A continuation carries before/id; a jump carries from (id 0 sorts
    // before every real id, so the bound month is excluded entirely).
    let before = cur
        .before
        .clone()
        .zip(cur.id)
        .or_else(|| from_bound.clone().map(|b| (b, 0)));
    let res = tokio::task::spawn_blocking(move || {
        // One copy of each duplicate group, the best the viewer may see,
        // and nothing this person hid (REQ-DUP-001, REQ-DUP-002).
        let hidden = idx.hidden(&me)?;
        let mut items = idx.timeline_where(
            &labels,
            before.as_ref().map(|(t, i)| (t.as_str(), *i)),
            PAGE_SIZE as usize + 1,
            |id, rec| {
                kind.is_none_or(|v| rec.video.is_some() == v)
                    && dups.shown(id, &labels, &hidden).shown
            },
        )?;
        for it in &mut items {
            it.copies = dups.shown(it.id, &labels, &hidden).copies;
        }
        // The chooser counts what this timeline shows (REQ-WEB-010).
        let months = if full_page {
            idx.months_where(&labels, |id| dups.shown(id, &labels, &hidden).shown, kind)?
        } else {
            Vec::new()
        };
        anyhow::Ok((items, months))
    })
    .await;
    let (mut items, months) = match res {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let more = items.len() > PAGE_SIZE as usize;
    items.truncate(PAGE_SIZE as usize);
    let fragment = render_items(&who, &items, cur.day.as_deref(), more, kq);
    if !full_page {
        return Html(fragment).into_response();
    }
    let title = if who == "all" {
        "Everyone".to_string()
    } else {
        who.clone()
    };
    // After a jump, a link back up to the next newer month that has items.
    let newer = match cur.from.as_deref() {
        // A full page that starts at a cursor (the viewer's back link):
        // offer the whole month it is in.
        None => match cur.before.as_deref().and_then(|b| b.get(..7)) {
            Some(ym) => format!(
                "<p class=\"newer\"><a href=\"t/{}?from={}\">↑ Newer photos</a></p>\n",
                esc(&who),
                esc(ym)
            ),
            None => String::new(),
        },
        Some(f) => {
            let up = months
                .iter()
                .rev()
                .find(|(ym, _)| ym.as_str() > f && !ym.starts_with(f))
                .map(|(ym, _)| ym.clone());
            let href = match up {
                Some(ym) => format!("t/{}?from={}{kq}", esc(&who), esc(&ym)),
                None if kq.is_empty() => format!("t/{}", esc(&who)),
                None => format!("t/{}?{}", esc(&who), kq.trim_start_matches("&amp;")),
            };
            format!("<p class=\"newer\"><a href=\"{href}\">↑ Newer photos</a></p>\n")
        }
    };
    let body = format!(
        "<div class=\"thead\"><h1>{}</h1>{}</div>\n{}{}<div class=\"timeline\">\n{}</div>\n{}",
        esc(&title),
        kind_switch(&format!("t/{}", esc(&who)), kind),
        render_jump(&who, &months, kq),
        newer,
        fragment,
        if items.is_empty() {
            "<p>No items yet. Run <code>lichtbild scan</code>.</p>"
        } else {
            ""
        }
    );
    page_for(&st, &title, &body).into_response()
}

/// A photo, its (newer, older) neighbours, the URLs to prefetch for them,
/// and the EXIF details for the info panel.
type ViewerItem = (
    index::Record,
    Option<u64>,
    Option<u64>,
    Vec<String>,
    Vec<(&'static str, String)>,
    Option<crate::stt::Transcript>,
    Vec<(u64, index::Record)>,
    bool,
);

#[derive(Deserialize)]
pub struct ViewerQuery {
    /// Start a video at this second (from a transcript search hit).
    t: Option<f32>,
    /// The timeline's photo/video filter (REQ-WEB-010).
    kind: Option<String>,
    /// A search: lines of read text holding its words are marked.
    q: Option<String>,
}

/// A video's transcript as a panel of timed lines; a click on a time seeks
/// the video (assets/lichtbild.js), the link works without JavaScript.
pub fn render_transcript(id: u64, who: &str, tr: &crate::stt::Transcript) -> String {
    if tr.pieces.is_empty() {
        return String::new();
    }
    let mut langs: Vec<&str> = tr.pieces.iter().map(|p| p.lang.as_str()).collect();
    langs.sort_unstable();
    langs.dedup();
    let mut s = format!(
        "<details class=\"transcript\"><summary title=\"Transcript (t)\" aria-label=\"Transcript\">≡</summary>\
         <div class=\"tpanel\" data-title=\"What is said\" data-below=\"Show below the video\" data-over=\"Show over the video\"><div class=\"ptools\" hidden><button type=\"button\" class=\"copy\">Copy</button><button type=\"button\" class=\"place\">Show below the video</button></div><p class=\"dim\">Transcript ({}{}), made in the house by {}</p><ol>",
        esc(&langs.join(", ")),
        if tr.partial { ", first 30 minutes" } else { "" },
        esc(&tr.model)
    );
    for p in &tr.pieces {
        s += &format!(
            "<li><a class=\"ts\" data-t=\"{t:.1}\" href=\"v/{who}/{id}?t={t:.1}\">{at}</a> {text}</li>",
            t = p.start,
            who = esc(who),
            at = duration((p.start * 1000.0) as u64),
            text = esc(&p.text)
        );
    }
    s += "</ol></div></details>";
    s
}

/// The text read in a picture (REQ-TEXT-001): a panel with the text by
/// document and block, to read and copy, and a layer of line boxes over
/// the picture (placed by assets/lichtbild.js from the corners in
/// `data-quad`; a tap searches for the line). Lines holding a word of `q`
/// are marked, and then the layer shows from the start.
pub fn render_text(text: &crate::ocr::Text, q: Option<&str>) -> (String, String) {
    if text.state != "ok" || text.lines.is_empty() {
        return (String::new(), String::new());
    }
    let words: Vec<String> = q
        .unwrap_or("")
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.chars().count() >= 2)
        .collect();
    let hit = |t: &str| {
        let t = t.to_lowercase();
        !words.is_empty() && words.iter().any(|w| t.contains(w.as_str()))
    };
    let any_hit = text.lines.iter().any(|l| hit(&l.text));
    let docs = text.documents();
    let mut panel = format!(
        "<details class=\"textp\"{open}><summary title=\"Text in the picture\" aria-label=\"Text in the picture\">Aa</summary>\
         <div class=\"xpanel\" data-title=\"Text in the picture\" data-below=\"Show below the picture\" data-over=\"Show over the picture\"><div class=\"ptools\" hidden><button type=\"button\" class=\"copy\">Copy</button><button type=\"button\" class=\"place\">Show below the picture</button></div><p class=\"dim\">Text read in this picture, in the house{many}. A line on the picture searches for it.</p>",
        open = if any_hit { " open" } else { "" },
        many = if docs > 1 { format!("; {docs} separate documents") } else { String::new() },
    );
    let mut layer = String::from("<div class=\"textlayer\" hidden aria-hidden=\"true\">");
    let mut cur_doc = None;
    let mut cur_block = None;
    for l in &text.lines {
        let doc = text.docs.get(l.block as usize).copied().unwrap_or(0);
        if cur_block != Some(l.block) {
            if cur_block.is_some() {
                panel += "</p>";
            }
            if docs > 1 && cur_doc != Some(doc) {
                if cur_doc.is_some() {
                    panel += "</section>";
                }
                panel += &format!("<section class=\"doc\"><h3>Document {}</h3>", doc + 1);
                cur_doc = Some(doc);
            }
            panel += "<p>";
            cur_block = Some(l.block);
        } else {
            panel += "<br>";
        }
        let cls = if hit(&l.text) { " class=\"hit\"" } else { "" };
        panel += &format!("<span{cls}>{}</span>", esc(&l.text));
        let quad: Vec<String> = l.quad.iter().map(|v| format!("{v:.4}")).collect();
        layer += &format!(
            "<a{cls} href=\"search?q={q}\" data-quad=\"{quad}\" title=\"{t}\"></a>",
            q = esc(&urlencode(&l.text)),
            quad = quad.join(","),
            t = esc(&l.text),
        );
    }
    panel += "</p>";
    if docs > 1 {
        panel += "</section>";
    }
    panel += "</div></details>";
    layer += "</div>";
    (panel, layer)
}

/// The copies of a picture the viewer may see (REQ-DUP-001): which is
/// shown, and where each one is; a click opens that copy.
pub fn render_copies(id: u64, who: &str, copies: &[(u64, index::Record)]) -> String {
    if copies.len() < 2 {
        return String::new();
    }
    let mut s = format!(
        "<details class=\"copies\"><summary title=\"Copies\" aria-label=\"Copies\">×{n}</summary>\
         <div class=\"cpanel\"><p class=\"dim\">The same picture {n} times; the timeline shows the first. “Keep this one” hides the others, for you only.</p><ol>",
        n = copies.len()
    );
    for (c, r) in copies {
        let rel = String::from_utf8_lossy(&r.rel);
        let bytes = if r.size >= 1_048_576 {
            format!("{:.1} MB", r.size as f64 / 1_048_576.0)
        } else {
            format!("{} kB", r.size.div_ceil(1024))
        };
        s += &format!(
            "<li{cur}><a href=\"v/{who}/{c}\">{lib} · {path}</a> <span class=\"dim\">{w} × {h} · {bytes} · date from {src}</span>\
             <form method=\"post\" action=\"keep/{c}\"><input type=\"hidden\" name=\"back\" value=\"v/{who}/{c}\"><button type=\"submit\">Keep this one</button></form></li>",
            cur = if *c == id { " class=\"current\"" } else { "" },
            who = esc(who),
            lib = esc(&r.label),
            path = esc(&rel),
            w = r.width,
            h = r.height,
            src = esc(&r.date_source),
        );
    }
    s += "</ol></div></details>";
    s
}

/// The viewer's info panel (REQ-WEB-008): date and its source, place,
/// camera details, size and file. The place name comes from the offline
/// GeoNames index (REQ-PLACE-001); nothing is looked up outside.
/// "m:ss", or "h:mm:ss" from an hour.
pub fn duration(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// What the viewer may do about an item's position: (item id, may set it
/// on the map, may forget a hand-set one).
pub type PlaceActions = Option<(u64, bool, bool)>;

pub fn render_info(
    rec: &index::Record,
    exif: &[(&'static str, String)],
    places: Option<&crate::places::Places>,
    pos: Option<&crate::place::Position>,
    act: PlaceActions,
) -> String {
    let mut rows: Vec<(&str, String)> = Vec::new();
    let mut taken = esc(&rec.taken_local.replace('T', " "));
    if let Some(o) = &rec.taken_offset {
        taken += &format!(" <span class=\"dim\">UTC{}</span>", esc(o));
    }
    let source = match rec.date_source.as_str() {
        "exif" => "camera (EXIF)",
        "exif-other" => "camera (EXIF, not the capture time)",
        "file-name" => "file name",
        "file-time" => "file time",
        "video" => "camera (video)",
        "video-utc" => "camera (video, in UTC)",
        other => other,
    };
    taken += &format!("<br><span class=\"dim\">from {}</span>", esc(source));
    rows.push(("Taken", taken));
    use crate::place::How;
    let mut v = match pos {
        Some(p) => {
            let coords = format!("{:.5}, {:.5}", p.lat, p.lon);
            let near = match places.and_then(|pl| pl.nearest(p.lat, p.lon)) {
                Some(n) => format!(
                    "near {} <span class=\"dim\">({:.0} km)</span><br>",
                    esc(&n.label),
                    n.km.max(1.0)
                ),
                None => String::new(),
            };
            let within = if p.radius_m >= 1000 {
                format!("{:.1} km", f64::from(p.radius_m) / 1000.0)
            } else {
                format!("{} m", p.radius_m)
            };
            let mins = |s: u32| match s / 60 {
                0 => "less than a minute".to_string(),
                1 => "1 minute".to_string(),
                m => format!("{m} minutes"),
            };
            let how = match p.how {
                How::Recorded => String::new(),
                How::ByHand => "<br><span class=\"dim\">set by hand</span>".into(),
                How::Between { gap_s } => format!(
                    "<br><span class=\"dim\">estimated, within {within}: between two pictures {} apart</span>",
                    mins(gap_s)
                ),
                How::Near { gap_s } => format!(
                    "<br><span class=\"dim\">estimated, within {within}: from a picture {} away</span>",
                    mins(gap_s)
                ),
            };
            let about = if p.how.estimated() { "about " } else { "" };
            format!("{about}{near}<span class=\"dim\">{coords}</span>{how}")
        }
        None => "<span class=\"dim\">no position recorded</span>".into(),
    };
    if let Some((id, can_set, can_forget)) = act {
        let mut a = Vec::new();
        if can_set {
            let label = match pos.map(|p| p.how) {
                None => "Place it on the map",
                Some(How::ByHand) => "Move it",
                Some(_) => "Place it yourself",
            };
            a.push(format!("<a href=\"map?place={id}\">{label}</a>"));
        }
        if can_forget && pos.is_some_and(|p| p.how == How::ByHand) {
            a.push(format!(
                "<form method=\"post\" action=\"unplace/{id}\"><button type=\"submit\">Forget it</button></form>"
            ));
        }
        if !a.is_empty() {
            v += &format!("<br><span class=\"placeact\">{}</span>", a.join(" · "));
        }
    }
    rows.push(("Place", v));
    if let Some(v) = &rec.video {
        let codec = match v.codec.as_str() {
            "h264" => "H.264",
            "hevc" => "HEVC",
            other => other,
        };
        rows.push((
            "Length",
            format!(
                "{} <span class=\"dim\">· {}</span>",
                duration(v.duration_ms),
                esc(codec)
            ),
        ));
    }
    for (k, v) in exif {
        rows.push((k, esc(v)));
    }
    let bytes = if rec.size >= 1_048_576 {
        format!("{:.1} MB", rec.size as f64 / 1_048_576.0)
    } else {
        format!("{} kB", rec.size.div_ceil(1024))
    };
    rows.push((
        "Size",
        format!(
            "{} × {} <span class=\"dim\">· {bytes}</span>",
            rec.width, rec.height
        ),
    ));
    let rel = String::from_utf8_lossy(&rec.rel);
    let file = rel.rsplit('/').next().unwrap_or(&rel);
    rows.push(("File", esc(file)));
    rows.push(("Library", esc(&rec.label)));
    let mut s = String::from(
        "<details class=\"info\"><summary title=\"Info (i)\" aria-label=\"Info\">ⓘ</summary><dl class=\"meta\">",
    );
    for (k, v) in rows {
        s += &format!("<dt>{k}</dt><dd>{v}</dd>");
    }
    s += "</dl></details>";
    s
}

/// The viewer's photo, or a note when the original is a format browsers
/// cannot display and no preview could be made from it.
pub fn photo_html(id: u64, rec: &index::Record) -> String {
    if rec.video.is_some() {
        // The original plays in the browser, seeking by range requests;
        // the poster is the decoded first keyframe when there is one.
        let poster = if rec.preview {
            format!(
                " poster=\"preview/{id}?h={}\"",
                esc(rec.hash.get(..12).unwrap_or(""))
            )
        } else {
            String::new()
        };
        return format!(
            "<video class=\"photo\" controls playsinline preload=\"metadata\"{poster} src=\"original/{id}\"></video>"
        );
    }
    let rel = String::from_utf8_lossy(&rec.rel).to_ascii_lowercase();
    let browser_cannot_show = rel.ends_with(".heic") || rel.ends_with(".heif");
    if browser_cannot_show && !rec.preview {
        return format!(
            "<div class=\"nophoto\"><p>This HEIC photo could not be decoded, so there is no preview.</p>\
             <p><a href=\"original/{id}\" download>Download the original</a></p></div>"
        );
    }
    format!(
        "<img class=\"photo\" src=\"{}\" alt=\"\">",
        esc(&photo_src(id, rec))
    )
}

/// What the viewer shows for an item: the JPEG preview when the original is
/// a format browsers cannot display (HEIC), the original otherwise.
pub fn photo_src(id: u64, rec: &index::Record) -> String {
    if rec.preview {
        format!("preview/{id}?h={}", rec.hash.get(..12).unwrap_or(""))
    } else {
        format!("original/{id}")
    }
}

/// GET /v/{who}/{id}: one photo, large, with the previous (newer) and next
/// (older) photo of the same timeline. Arrow keys and swipes in
/// assets/lichtbild.js follow the same links; without JavaScript they are
/// plain links.
async fn viewer(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath((who, id)): UrlPath<(String, u64)>,
    Query(vq): Query<ViewerQuery>,
) -> Response {
    let Some(labels) = scope(&viewer, &who) else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    let idx = st.index.clone();
    let roots = st.roots.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let max_file_bytes = st.max_file_bytes;
    let (kind, kq) = kind_filter(vq.kind.as_deref());
    let vqs = viewer_query(kq);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ViewerItem>> {
        let Some(rec) = idx.locate(id)? else {
            return Ok(None);
        };
        // The item must belong to this timeline (prepares per-person scoping).
        if !labels.contains(&rec.label) {
            return Ok(None);
        }
        // Step through what the timeline shows: one copy of each
        // duplicate, nothing hidden, and the photo/video filter.
        let hidden = idx.hidden(&me)?;
        let (newer, older) = idx.neighbours_where(&labels, &rec.taken_local, id, |n, r| {
            kind.is_none_or(|v| r.video.is_some() == v) && dups.shown(n, &labels, &hidden).shown
        })?;
        let mut prefetch = Vec::new();
        for n in [newer, older].into_iter().flatten() {
            if let Some(r) = idx.locate(n)? {
                // A video prefetches its poster, never the video itself.
                if r.video.is_none() || r.preview {
                    prefetch.push(photo_src(n, &r));
                }
            }
        }
        // EXIF for the info panel, read from the original (read-only).
        let exif = roots
            .get(&rec.label)
            .filter(|_| rec.video.is_none())
            .and_then(|root| {
                crate::source::read_limited(root, &index::rel_from_bytes(&rec.rel), max_file_bytes)
                    .ok()
            })
            .map(|b| crate::media::exif_details(&b))
            .unwrap_or_default();
        let transcript = match rec.video {
            Some(_) => idx.transcript(&rec.hash)?,
            None => None,
        };
        let mut copies = Vec::new();
        for c in dups.copies(id, &labels) {
            if let Some(r) = idx.locate(c)? {
                copies.push((c, r));
            }
        }
        let is_hidden = hidden.contains(&id);
        Ok(Some((
            rec, newer, older, prefetch, exif, transcript, copies, is_hidden,
        )))
    })
    .await;
    let (rec, newer, older, prefetch, exif, transcript, copies, is_hidden) = match res {
        Ok(Ok(Some(x))) => x,
        Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "no such photo").into_response(),
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    // Where it was taken: recorded, set by hand or estimated; worked out
    // once per index change. Without it the panel just says nothing is known.
    let (pc, idx2) = (st.positions.clone(), st.index.clone());
    let pos = tokio::task::spawn_blocking(move || pc.get(&idx2).map(|m| m.get(&id).copied()))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten();
    // Text read in the picture, if any.
    let (idx3, hash) = (st.index.clone(), rec.hash.clone());
    let read = if rec.video.is_none() {
        tokio::task::spawn_blocking(move || idx3.text(&hash))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    } else {
        None
    };
    let (textp, layer) = read
        .as_ref()
        .map(|t| render_text(t, vq.q.as_deref()))
        .unwrap_or_default();
    let mine = owned(st.auth.is_some(), &viewer).contains(&rec.label) && rec.lat.is_none();
    let act = mine.then_some((id, st.map.pmtiles.is_some(), true));
    let w = esc(&who);
    let link = |rel: &str, label: &str, target: Option<u64>| {
        match target {
        Some(t) => format!("<a class=\"{rel}\" rel=\"{rel}\" href=\"v/{w}/{t}{vqs}\" aria-label=\"{label}\">{arrow}</a>",
            arrow = if rel == "prev" { "‹" } else { "›" }),
        None => format!("<span class=\"{rel} none\" aria-hidden=\"true\">{}</span>", if rel == "prev" { "‹" } else { "›" }),
    }
    };
    let prefetch: String = prefetch
        .iter()
        .map(|u| format!("<link rel=\"prefetch\" href=\"{}\">", esc(u)))
        .collect();
    let when = if DateSource::is_weak(&rec.date_source) {
        format!(
            "{} <span class=\"weakdate\">(date from {})</span>",
            esc(&rec.taken_local.replace('T', " ")),
            esc(&rec.date_source)
        )
    } else {
        esc(&rec.taken_local.replace('T', " "))
    };
    let hide = {
        // Back to the timeline where this item was; showing it again
        // stays in the viewer.
        let back = format!("t/{who}?before={}&id={}#i{id}", rec.taken_local, id + 1);
        if is_hidden {
            format!(
                "<form class=\"hidef\" method=\"post\" action=\"unhide/{id}\"><input type=\"hidden\" name=\"back\" value=\"v/{w}/{id}\"><button type=\"submit\" title=\"Hidden for you\">Hidden for you · show again</button></form>"
            )
        } else {
            format!(
                "<form class=\"hidef\" method=\"post\" action=\"hide/{id}\"><input type=\"hidden\" name=\"back\" value=\"{b}\"><button type=\"submit\" title=\"Only your view changes\">Hide this for me</button></form>",
                b = esc(&back)
            )
        }
    };
    let body = format!(
        "<div class=\"viewer\">\n\
         <div class=\"vbar\"><a class=\"back\" href=\"t/{w}?before={taken}&amp;id={after}{kq}#i{id}\">← Timeline</a> <span class=\"when\">{when}</span>{tr}{textp}{info}<button type=\"button\" class=\"fs\" hidden title=\"Full screen (f)\" aria-label=\"Full screen\">⛶</button></div>\n\
         <div class=\"stage\">{prev}{photo}{layer}{next}</div>\n\
         <section class=\"under\" hidden></section>\n\
         {prefetch}</div>",
        taken = esc(&rec.taken_local),
        after = id + 1,
        photo = match vq.t.filter(|t| t.is_finite() && *t > 0.0) {
            // A W3C media fragment (#t=) starts the video at the hit.
            Some(t) if rec.video.is_some() => photo_html(id, &rec).replacen(
                &format!("src=\"original/{id}\""),
                &format!("src=\"original/{id}#t={t:.1}\""),
                1,
            ),
            _ => photo_html(id, &rec),
        },
        tr = transcript
            .as_ref()
            .map(|t| render_transcript(id, &who, t))
            .unwrap_or_default(),
        // Hide is an action on this picture, inside its info panel.
        info = render_copies(id, &who, &copies)
            + &render_info(&rec, &exif, st.places.as_deref(), pos.as_ref(), act)
                .replace(
                    "</dl></details>",
                    &format!("<div class=\"hiderow\">{hide}</div></dl></details>"),
                ),
        prev = link("prev", "Newer photo", newer),
        next = link("next", "Older photo", older),
    );
    page_for(&st, "lichtbild", &body).into_response()
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    who: Option<String>,
    kind: Option<String>,
}

/// GET /search?q=: one search box over transcripts, places, dates and
/// names (REQ-SEARCH-001), in the viewer's libraries only.
async fn search(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Query(sq): Query<SearchQuery>,
) -> Response {
    let q: String = sq.q.unwrap_or_default().chars().take(200).collect();
    // Without `who`, every library the viewer may see; result links open
    // the merged timeline, or the single one when there is only one.
    let who = match sq.who {
        Some(w) => w,
        None if viewer.labels.len() == 1 => viewer.labels[0].clone(),
        None => "all".to_string(),
    };
    let Some(labels) = scope(&viewer, &who) else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    let idx = st.index.clone();
    let sx = st.search.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let (kind, _) = kind_filter(sq.kind.as_deref());
    let q2 = q.clone();
    let res = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Vec<(crate::search::Hit, index::Record)>> {
            let hidden = idx.hidden(&me)?;
            let mut out: Vec<(crate::search::Hit, index::Record)> = Vec::new();
            for mut h in sx.query(&q2, &labels)?.into_iter().take(200) {
                // A hit on a copy shows the copy the timeline shows; a hit
                // on something hidden, with no copy shown, is left out.
                if !dups.shown(h.id, &labels, &hidden).shown {
                    match dups
                        .copies(h.id, &labels)
                        .into_iter()
                        .find(|c| dups.shown(*c, &labels, &hidden).shown)
                    {
                        Some(best) => h.id = best,
                        None => continue,
                    }
                }
                if let Some((prev, _)) = out.iter_mut().find(|(p, _)| p.id == h.id) {
                    for s in h.speech {
                        if prev.speech.len() < 3 && !prev.speech.contains(&s) {
                            prev.speech.push(s);
                        }
                    }
                    continue;
                }
                if let Some(rec) = idx
                    .locate(h.id)?
                    .filter(|r| kind.is_none_or(|v| r.video.is_some() == v))
                {
                    out.push((h, rec));
                }
            }
            Ok(out)
        },
    )
    .await;
    let hits = match res {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let body = format!(
        "{}{}",
        kind_switch(&format!("search?q={}", esc(&q)), kind),
        render_search(&who, &q, &hits, st.places.as_deref())
    );
    page_for(&st, "lichtbild search", &body).into_response()
}

/// The results page: each item as a tile with its date, place and the
/// transcript lines that matched (a time opens the video there).
pub fn render_search(
    who: &str,
    q: &str,
    hits: &[(crate::search::Hit, index::Record)],
    places: Option<&crate::places::Places>,
) -> String {
    let who = esc(who);
    let mut s = format!(
        "<section class=\"results\"><h2 class=\"rhead\">{n} {what} for \u{201c}{q}\u{201d}</h2>",
        n = hits.len(),
        what = if hits.len() == 1 { "result" } else { "results" },
        q = esc(q)
    );
    if q.trim().is_empty() {
        return "<section class=\"results\"><p class=\"dim\">Search what was said in videos, places, dates (2021, Juli, July) and file names.</p></section>".into();
    }
    for (h, rec) in hits {
        let id = h.id;
        let img = if rec.thumb == "ok" {
            format!(
                "<img src=\"thumb/{id}?h={hv}\" loading=\"lazy\" alt=\"\">",
                hv = esc(rec.hash.get(..12).unwrap_or(""))
            )
        } else if rec.video.is_some() {
            "<span class=\"nothumb\">video</span>".to_string()
        } else {
            "<span class=\"nothumb\">no preview</span>".to_string()
        };
        let place = match (places, rec.lat, rec.lon) {
            (Some(p), Some(lat), Some(lon)) => p
                .nearest(lat, lon)
                .map(|n| format!(" · {}", esc(&n.label)))
                .unwrap_or_default(),
            _ => String::new(),
        };
        let mut lines = String::new();
        for (t, text) in &h.speech {
            lines += &format!(
                "<li><a class=\"ts\" href=\"v/{who}/{id}?t={t:.1}\">{at}</a> {text}</li>",
                at = duration((*t * 1000.0) as u64),
                text = esc(text)
            );
        }
        if !lines.is_empty() {
            lines = format!("<ul class=\"said\">{lines}</ul>");
        }
        // Text read in the picture: the matching lines, and the picture
        // opens with them marked.
        let mut href = format!("v/{who}/{id}");
        if let Some(r) = &h.read {
            lines += &format!(
                "<p class=\"readhit\"><span class=\"dim\">in the picture:</span> {}</p>",
                esc(r)
            );
            href += &format!("?q={}", esc(&urlencode(q)));
        }
        s += &format!(
            "<div class=\"result\"><a class=\"item{vid}\" href=\"{href}\">{img}</a>\
             <div class=\"rmeta\"><span class=\"when\">{when}</span>{place}{lines}</div></div>",
            vid = if rec.video.is_some() { " video" } else { "" },
            when = esc(&rec.taken_local.replace('T', " ")),
        );
    }
    s += "</section>";
    s
}

#[derive(Deserialize)]
pub struct Back {
    /// Where to go afterwards: a path inside the app (auth::safe_next).
    back: Option<String>,
}

/// A change made by a page of this app, not by another site (as for
/// sign-out): browsers label cross-site requests in Sec-Fetch-Site.
fn same_site(headers: &axum::http::HeaderMap) -> bool {
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    site.is_none_or(|s| s == "same-origin" || s == "none")
}

fn back_to(st: &AppState, back: Option<&str>) -> Response {
    let next = crate::auth::safe_next(back.unwrap_or(""));
    Redirect::to(&format!("{}/{}", st.base_path, next)).into_response()
}

/// Run `change` on the item if the viewer may see it, then go back.
async fn choice(
    st: AppState,
    viewer: Viewer,
    id: u64,
    headers: axum::http::HeaderMap,
    back: Option<String>,
    change: fn(&index::Index, &crate::dups::Dups, &Viewer, &str, u64) -> anyhow::Result<()>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let idx = st.index.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let Some(rec) = idx.locate(id)? else {
            return Ok(false);
        };
        if !viewer.may_see(&rec.label) {
            return Ok(false);
        }
        change(&idx, &dups, &viewer, &me, id)?;
        Ok(true)
    })
    .await;
    match res {
        Ok(Ok(true)) => back_to(&st, back.as_deref()),
        Ok(Ok(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /hide/{id}: this person no longer sees the item (REQ-DUP-002).
async fn hide(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(b): axum::Form<Back>,
) -> Response {
    choice(st, viewer, id, headers, b.back, |idx, _, _, me, id| {
        idx.set_hidden(me, id, true)
    })
    .await
}

/// POST /unhide/{id}: show the item again.
async fn unhide(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(b): axum::Form<Back>,
) -> Response {
    choice(st, viewer, id, headers, b.back, |idx, _, _, me, id| {
        idx.set_hidden(me, id, false)
    })
    .await
}

/// POST /keep/{id}: show this copy, hide its other copies (for this person).
async fn keep(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(b): axum::Form<Back>,
) -> Response {
    choice(
        st,
        viewer,
        id,
        headers,
        b.back,
        |idx, dups, viewer, me, id| {
            for c in dups.copies(id, &viewer.labels) {
                idx.set_hidden(me, c, c != id)?;
            }
            Ok(())
        },
    )
    .await
}

/// GET /hidden: what this person hid, to show it again.
async fn hidden_page(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let idx = st.index.clone();
    let me = person(&viewer);
    let labels = viewer.labels.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(u64, index::Record)>> {
        let mut out = Vec::new();
        let mut ids: Vec<u64> = idx.hidden(&me)?.into_iter().collect();
        ids.sort_unstable();
        for id in ids {
            if let Some(r) = idx.locate(id)?.filter(|r| labels.contains(&r.label)) {
                out.push((id, r));
            }
        }
        Ok(out)
    })
    .await;
    let items = match res {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut body = format!(
        "<h1>Hidden</h1><p class=\"dim\">{} hidden by you; only your view changes, nothing in the library.</p><div class=\"grid hiddenlist\">",
        items.len()
    );
    for (id, r) in &items {
        let img = if r.thumb == "ok" {
            format!(
                "<img src=\"thumb/{id}?h={}\" loading=\"lazy\" alt=\"\">",
                esc(r.hash.get(..12).unwrap_or(""))
            )
        } else {
            "<span class=\"nothumb\">no preview</span>".to_string()
        };
        body += &format!(
            "<div class=\"hid\"><a class=\"item\" href=\"v/{l}/{id}\">{img}</a>\
             <form method=\"post\" action=\"unhide/{id}\"><input type=\"hidden\" name=\"back\" value=\"hidden\"><button type=\"submit\">Show again</button></form></div>",
            l = esc(&r.label)
        );
    }
    body += "</div>";
    page_for(&st, "lichtbild hidden", &body).into_response()
}

/// The libraries whose file paths this viewer may list: their own (the
/// library with their label), or all of them in operator mode (no login).
/// A library someone else owns is never listed, even when granted.
pub fn owned(login: bool, viewer: &Viewer) -> Vec<String> {
    match (login, &viewer.name) {
        (true, Some(me)) => viewer.labels.iter().filter(|l| *l == me).cloned().collect(),
        (true, None) => Vec::new(),
        (false, _) => viewer.labels.clone(),
    }
}

#[derive(Deserialize)]
pub struct PlaceForm {
    lat: f64,
    lon: f64,
}

/// Set or remove a hand-set position (REQ-PLACE-002): only for an item of
/// the viewer's own library that records no position itself, only from a
/// same-site form; then back to the item.
async fn set_place(
    st: AppState,
    viewer: Viewer,
    id: u64,
    headers: axum::http::HeaderMap,
    at: Option<(f64, f64)>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if at.is_some_and(|(lat, lon)| !crate::place::valid(lat, lon)) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let idx = st.index.clone();
    let own = owned(st.auth.is_some(), &viewer);
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<String>> {
        let Some(rec) = idx.locate(id)? else {
            return Ok(None);
        };
        if !own.contains(&rec.label) || rec.lat.is_some() {
            return Ok(None);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let placed = at.map(|(lat, lon)| crate::place::Placed {
            lat,
            lon,
            by: me,
            at: now,
        });
        idx.set_placed(id, placed.as_ref())?;
        Ok(Some(rec.label))
    })
    .await;
    match res {
        Ok(Ok(Some(label))) => {
            // Search finds pictures by place: rebuild it in the background.
            let (search, idx, places) = (st.search.clone(), st.index.clone(), st.places.clone());
            tokio::task::spawn_blocking(move || {
                if let Err(e) = search.rebuild(&idx, places.as_deref()) {
                    tracing::warn!("search rebuild after placing failed: {e:#}");
                }
            });
            back_to(&st, Some(&format!("v/{label}/{id}")))
        }
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /place/{id}: set where the item was taken.
async fn place(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<PlaceForm>,
) -> Response {
    set_place(st, viewer, id, headers, Some((f.lat, f.lon))).await
}

/// POST /unplace/{id}: forget the position set by hand.
async fn unplace(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
) -> Response {
    set_place(st, viewer, id, headers, None).await
}

/// Exact duplicate files in the viewer's own libraries: (label, kept
/// copy, the other copies).
type Cleanup = Vec<(String, index::Record, Vec<index::Record>)>;

async fn cleanup(st: &AppState, viewer: &Viewer) -> anyhow::Result<Cleanup> {
    let idx = st.index.clone();
    let dups = st.dups.clone();
    let labels = owned(st.auth.is_some(), viewer);
    tokio::task::spawn_blocking(move || -> anyhow::Result<Cleanup> {
        let mut out = Vec::new();
        for l in labels {
            for group in dups.exact_in(&idx, &l)? {
                let mut recs: Vec<index::Record> = Vec::new();
                for id in group {
                    if let Some(r) = idx.locate(id)? {
                        recs.push(r);
                    }
                }
                if recs.len() > 1 {
                    let keep = recs.remove(0);
                    out.push((l.clone(), keep, recs));
                }
            }
        }
        out.sort_by(|a, b| (&a.0, &a.1.rel).cmp(&(&b.0, &b.1.rel)));
        Ok(out)
    })
    .await?
}

/// GET /duplicates: exact duplicate files in your own libraries, and the
/// space their extra copies take (REQ-DUP-003). lichtbild deletes nothing.
async fn duplicates_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Response {
    let groups = match cleanup(&st, &viewer).await {
        Ok(g) => g,
        Err(e) => return internal(e),
    };
    let files: usize = groups.iter().map(|g| g.2.len()).sum();
    let bytes: u64 = groups.iter().flat_map(|g| g.2.iter()).map(|r| r.size).sum();
    let mut body = format!(
        "<h1>Duplicate files</h1><p>{n} {groups} of identical files in your library: {files} extra {copies}, {mb:.1} MB.</p>\
         <p class=\"dim\">lichtbild changes nothing in the library. Delete the extra copies where they are stored, if you want to; the next scan notices.</p>\
         <p><a href=\"duplicates.csv\" download>Download the list (CSV)</a></p><ol class=\"duplist\">",
        n = groups.len(),
        groups = if groups.len() == 1 { "group" } else { "groups" },
        copies = if files == 1 { "copy" } else { "copies" },
        mb = bytes as f64 / 1_048_576.0,
    );
    for (l, keep, others) in groups.iter().take(500) {
        body += &format!(
            "<li><span class=\"keep\">{l} · {k}</span><ul>",
            l = esc(l),
            k = esc(&String::from_utf8_lossy(&keep.rel))
        );
        for o in others {
            body += &format!("<li>{}</li>", esc(&String::from_utf8_lossy(&o.rel)));
        }
        body += "</ul></li>";
    }
    body += "</ol>";
    page_for(&st, "lichtbild duplicates", &body).into_response()
}

fn csv_field(s: &str) -> String {
    // Quoted; a leading = + - @ is defused so a spreadsheet does not run it.
    let s = if s.starts_with(['=', '+', '-', '@']) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// GET /duplicates.csv: the same list, one row per extra copy.
async fn duplicates_csv(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Response {
    let groups = match cleanup(&st, &viewer).await {
        Ok(g) => g,
        Err(e) => return internal(e),
    };
    let mut csv = String::from("library,keep,duplicate,bytes\n");
    for (l, keep, others) in &groups {
        for o in others {
            csv += &format!(
                "{},{},{},{}\n",
                csv_field(l),
                csv_field(&String::from_utf8_lossy(&keep.rel)),
                csv_field(&String::from_utf8_lossy(&o.rel)),
                o.size
            );
        }
    }
    (
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"lichtbild-duplicates.csv\"",
            ),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        csv,
    )
        .into_response()
}

/// GET /status: what the server is doing (REQ-WEB-009): scan,
/// transcription, duplicates, search, version. Counts and times only.
async fn status_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
) -> Response {
    use crate::status::{relative, span, Phase};
    let s = st.status.snapshot();
    let counts = match st.transcribe_model.clone() {
        Some(m) => {
            let idx = st.index.clone();
            let labels = viewer.labels.clone();
            match tokio::task::spawn_blocking(move || idx.transcript_counts(&labels, &m)).await {
                Ok(Ok(c)) => Some(c),
                Ok(Err(e)) => return internal(e),
                Err(e) => return internal(e),
            }
        }
        None => None,
    };
    let read_counts = if st.ocr_on {
        let (idx, labels) = (st.index.clone(), viewer.labels.clone());
        match tokio::task::spawn_blocking(move || idx.text_counts(&labels, crate::ocr::MODEL)).await
        {
            Ok(Ok(c)) => Some(c),
            Ok(Err(e)) => return internal(e),
            Err(e) => return internal(e),
        }
    } else {
        None
    };
    let positions = {
        let (pc, idx, labels) = (
            st.positions.clone(),
            st.index.clone(),
            viewer.labels.clone(),
        );
        match tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let pos = pc.get(&idx)?;
            Ok(crate::place::counts(&idx.all()?, &pos, &labels))
        })
        .await
        {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return internal(e),
            Err(e) => return internal(e),
        }
    };
    let now = std::time::SystemTime::now();
    let everything = st.labels.iter().all(|l| viewer.labels.contains(l));
    let mut rows: Vec<(&str, String)> = Vec::new();
    let (state, cls) = match &s.phase {
        Phase::Starting => ("Starting".to_string(), "busy"),
        Phase::Scanning { since } => (
            format!("Scanning the libraries, since {}", relative(*since, now)),
            "busy",
        ),
        Phase::Transcribing {
            since,
            done,
            total,
            report,
        } => (
            format!(
                "Transcribing videos: {done} of {total} done ({} with speech, {} without, {} failed), since {}",
                report.transcribed,
                report.no_speech,
                report.failed,
                relative(*since, now)
            ),
            "busy",
        ),
        Phase::Reading {
            since,
            done,
            total,
            report,
        } => (
            format!(
                "Reading text in photos: {done} of {total} done ({} with text, {} failed), since {}",
                report.with_text,
                report.failed,
                relative(*since, now)
            ),
            "busy",
        ),
        Phase::Idle { next: Some(n) } => (format!("Idle; next scan {}", relative(*n, now)), "ok"),
        Phase::Idle { next: None } => ("Idle".to_string(), "ok"),
    };
    let progress = match &s.phase {
        Phase::Transcribing { done, total, .. } | Phase::Reading { done, total, .. }
            if *total > 0 =>
        {
            format!("<progress max=\"{total}\" value=\"{done}\">{done} of {total}</progress>")
        }
        _ => String::new(),
    };
    rows.push((
        "Now",
        format!("<span class=\"dot {cls}\"></span>{}{progress}", esc(&state)),
    ));
    rows.push((
        "Last scan",
        match &s.last_scan {
            None => "not yet".into(),
            Some(d) => match () {
                _ if d.failed => format!(
                    "failed {}; the server's log says why",
                    relative(d.ended, now)
                ),
                _ if !everything => format!("{}, took {}", relative(d.ended, now), span(d.took)),
                _ => format!(
                    "{}, took {}: {} files found, {} indexed, {} skipped{}",
                    relative(d.ended, now),
                    span(d.took),
                    d.found,
                    d.indexed,
                    d.skipped,
                    if d.reconciled {
                        ""
                    } else {
                        " (did not reconcile)"
                    }
                ),
            },
        },
    ));
    rows.push((
        "Transcripts",
        match (st.transcribe_model.as_deref(), counts) {
            (Some(m), Some((videos, done, speech))) => format!(
                "{done} of {videos} videos transcribed, {speech} with speech <span class=\"dim\">(model {})</span>{}",
                esc(m),
                match &s.last_pass {
                    Some(p) => format!(
                        "<br><span class=\"dim\">last pass ended {}</span>",
                        relative(p.ended, now)
                    ),
                    None => String::new(),
                }
            ),
            _ => "off (no speech model configured)".into(),
        },
    ));
    rows.push((
        "Text in photos",
        match read_counts {
            Some((photos, done, with)) => format!(
                "{done} of {photos} photos read, {with} with text <span class=\"dim\">(model {})</span>{}",
                crate::ocr::MODEL,
                match s.last_read {
                    Some(t) => format!(
                        "<br><span class=\"dim\">last pass ended {}</span>",
                        relative(t, now)
                    ),
                    None => String::new(),
                }
            ),
            None => "off (no text model configured)".into(),
        },
    ));
    let c = positions;
    rows.push((
        "Positions",
        format!(
            "{} recorded, {} estimated ({} between two pictures, {} from one nearby), {} set by hand, {} without",
            c.recorded,
            c.between + c.near,
            c.between,
            c.near,
            c.by_hand,
            c.without
        ),
    ));
    // Totals over all libraries only for whoever sees all of them.
    if everything {
        rows.push((
            "Duplicates",
            match s.duplicates {
                Some((g, n)) => format!("{g} groups with {n} items, each shown once"),
                None => "not grouped yet".into(),
            },
        ));
        rows.push(("Search", format!("{} documents", s.search_docs)));
    }
    rows.push((
        "Map",
        if st.map.pmtiles.is_some() {
            "map file configured".into()
        } else {
            "no map file configured".into()
        },
    ));
    rows.push((
        "Place names",
        if st.places.is_some() { "on" } else { "off" }.into(),
    ));
    rows.push((
        "Running",
        format!(
            "lichtbild {}, started {}",
            esc(crate::VERSION),
            relative(s.started, now)
        ),
    ));
    // The list refreshes itself every 10 s (htmx); without JavaScript a
    // reload does the same.
    let mut list = String::from(
        "<dl class=\"status\" id=\"status\" hx-get=\"status\" hx-trigger=\"every 10s\" hx-select=\"#status\" hx-swap=\"outerHTML\">",
    );
    for (k, v) in rows {
        list += &format!("<dt>{k}</dt><dd>{v}</dd>");
    }
    list += "</dl>";
    if headers.contains_key("hx-request") {
        return Html(list).into_response();
    }
    page_for(&st, "lichtbild status", &format!("<h1>Status</h1>{list}")).into_response()
}

/// GET /words: the words most videos share, as bubbles (REQ-WORDS-001);
/// a word searches for it.
async fn words_page(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let idx = st.index.clone();
    let labels = viewer.labels.clone();
    let res = tokio::task::spawn_blocking(move || crate::words::cloud(&idx, &labels, 80)).await;
    let words = match res {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut body = String::from(
        "<h1>Words</h1><p class=\"dim\">What was said in your videos and written in your pictures: the larger a word, the more of them hold it. Common words are left out. A word searches for it.</p>",
    );
    if words.is_empty() {
        body += "<p>No words yet. Transcripts and the text in pictures are made in the background after each scan.</p>";
        return page_for(&st, "lichtbild words", &body).into_response();
    }
    let top_v = words.iter().map(|w| w.items).max().unwrap_or(1).max(1) as f64;
    let top_t = words.iter().map(|w| w.times).max().unwrap_or(1).max(1) as f64;
    // Sizes 1 to 6 on a log scale: mostly how many videos say a word, partly
    // how often (so a cloud of few videos still varies). The order mixes big
    // and small words, so the cloud does not read as a ranked list.
    let mut cloud: Vec<(u8, &crate::words::Word)> = words
        .iter()
        .map(|w| {
            let v = (w.items as f64).ln_1p() / top_v.ln_1p();
            let t = (w.times as f64).ln_1p() / top_t.ln_1p();
            let r = if top_v > 1.0 { 0.7 * v + 0.3 * t } else { t };
            (1 + (r * 5.0).round() as u8, w)
        })
        .collect();
    cloud.sort_by_key(|(_, w)| blake3::hash(w.word.as_bytes()).as_bytes()[0]);
    body += "<div class=\"cloud\">";
    for (size, w) in cloud {
        body += &format!(
            "<a class=\"w s{size}\" href=\"search?q={q}\" title=\"in {v} pictures and videos, {t} times\">{word}</a>",
            q = esc(&w.word),
            v = w.items,
            t = w.times,
            word = esc(&w.word)
        );
    }
    body += "</div>";
    page_for(&st, "lichtbild words", &body).into_response()
}

/// GET /preview/{id}: the JPEG preview of a HEIC original.
async fn preview(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
) -> Response {
    let dd = st.data_dir.clone();
    let idx = st.index.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        let Some(rec) = idx.locate(id)?.filter(|r| viewer.may_see(&r.label)) else {
            return Ok(None);
        };
        if !rec.preview {
            return Ok(None);
        }
        Ok(std::fs::read(crate::scan::preview_path(&dd, &rec.hash)).ok())
    })
    .await;
    match res {
        Ok(Ok(Some(b))) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            b,
        )
            .into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

async fn thumb(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
) -> Response {
    let dd = st.data_dir.clone();
    let idx = st.index.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        let Some(rec) = idx.locate(id)?.filter(|r| viewer.may_see(&r.label)) else {
            return Ok(None);
        };
        if rec.thumb != "ok" {
            return Ok(None);
        }
        Ok(std::fs::read(crate::scan::thumb_path(&dd, &rec.hash)).ok())
    })
    .await;
    match res {
        Ok(Ok(Some(b))) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            b,
        )
            .into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

fn content_type(p: &std::path::Path) -> &'static str {
    match p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("heic" | "heif") => "image/heic",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("3gp") => "video/3gpp",
        _ => "application/octet-stream",
    }
}

/// A `Range` header against a file of `len` bytes (RFC 9110 14.2): one
/// range only; several ranges or a malformed header get the whole file.
#[derive(Debug, PartialEq, Eq)]
pub enum ByteRange {
    Whole,
    /// First and last byte, inclusive.
    Part(u64, u64),
    Unsatisfiable,
}

pub fn byte_range(header: Option<&str>, len: u64) -> ByteRange {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return ByteRange::Whole;
    };
    if spec.contains(',') {
        return ByteRange::Whole;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return ByteRange::Whole;
    };
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    match (num(a), num(b), a.is_empty()) {
        // bytes=-n: the last n bytes.
        (None, Some(n), true) if n > 0 && len > 0 => {
            ByteRange::Part(len.saturating_sub(n), len - 1)
        }
        (None, Some(_), true) => ByteRange::Unsatisfiable,
        (Some(first), last, false) if b.is_empty() || last.is_some() => {
            if first >= len {
                return ByteRange::Unsatisfiable;
            }
            let last = last.unwrap_or(u64::MAX).min(len - 1);
            if last < first {
                ByteRange::Whole
            } else {
                ByteRange::Part(first, last)
            }
        }
        _ => ByteRange::Whole,
    }
}

async fn original(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
) -> Response {
    let idx = st.index.clone();
    let roots = st.roots.clone();
    let res = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Option<(std::fs::File, &'static str)>> {
            let Some(rec) = idx.locate(id)?.filter(|r| viewer.may_see(&r.label)) else {
                return Ok(None);
            };
            let rel = index::rel_from_bytes(&rec.rel);
            let Some(root) = roots.get(&rec.label) else {
                return Ok(None);
            };
            // SSC-004: the stored path must still resolve inside its root.
            match crate::source::open(root, &rel) {
                Ok(f) => Ok(Some((f, content_type(&rel)))),
                Err(_) => Ok(None),
            }
        },
    )
    .await;
    let (f, ct) = match res {
        Ok(Ok(Some(x))) => x,
        Ok(Ok(None)) => return StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let len = match f.metadata() {
        Ok(m) => m.len(),
        Err(e) => return internal(e),
    };
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    ranged(f, len, range, ct, "private, max-age=3600", None)
}

/// A file of `len` bytes as a whole (200), one range (206) or 416, for
/// originals and the map's PMTiles file.
pub(crate) fn ranged(
    f: std::fs::File,
    len: u64,
    range: Option<&str>,
    ct: &str,
    cache: &'static str,
    etag: Option<String>,
) -> Response {
    let mut common = vec![
        (header::CONTENT_TYPE, ct.to_string()),
        (header::CACHE_CONTROL, cache.to_string()),
        (header::ACCEPT_RANGES, "bytes".to_string()),
    ];
    if let Some(e) = etag {
        common.push((header::ETAG, e));
    }
    let common = axum::response::AppendHeaders(common);
    match byte_range(range, len) {
        ByteRange::Whole => (
            common,
            [(header::CONTENT_LENGTH, len.to_string())],
            tokio_util_stream(f, 0, len),
        )
            .into_response(),
        ByteRange::Part(first, last) => (
            StatusCode::PARTIAL_CONTENT,
            common,
            [
                (header::CONTENT_LENGTH, (last - first + 1).to_string()),
                (header::CONTENT_RANGE, format!("bytes {first}-{last}/{len}")),
            ],
            tokio_util_stream(f, first, last - first + 1),
        )
            .into_response(),
        ByteRange::Unsatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{len}"))],
        )
            .into_response(),
    }
}

/// Stream `count` bytes of a file from `start`, in chunks, without
/// loading it whole.
fn tokio_util_stream(mut f: std::fs::File, start: u64, count: u64) -> Body {
    use std::io::{Seek, SeekFrom};
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Body::empty();
    }
    let part = tokio::io::AsyncReadExt::take(tokio::fs::File::from_std(f), count);
    Body::from_stream(tokio_util::io::ReaderStream::new(part))
}

async fn static_file(UrlPath(file): UrlPath<String>) -> Response {
    let (body, ct) = match file.as_str() {
        "htmx-2.0.4.min.js" => (HTMX, "text/javascript; charset=utf-8"),
        "lichtbild.css" => (CSS, "text/css; charset=utf-8"),
        "lichtbild.js" => (JS, "text/javascript; charset=utf-8"),
        other => match crate::map::static_asset(other) {
            Some(x) => x,
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        body,
    )
        .into_response()
}

/// All embedded assets, for the no-outside-URL test (REQ-WEB-003).
/// The vendored map libraries are listed in `map::vendored`.
pub fn embedded_assets() -> [(&'static str, &'static str); 4] {
    [
        ("htmx-2.0.4.min.js", HTMX),
        ("lichtbild.css", CSS),
        ("lichtbild.js", JS),
        ("map.js", crate::map::MAP_JS),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ones_own_library_is_listed_for_cleanup() {
        let v = Viewer {
            labels: vec!["p2".into(), "p1".into()],
            name: Some("p2".into()),
        };
        // p2 may see p1, but p1's paths are p1's.
        assert_eq!(owned(true, &v), vec!["p2".to_string()]);
        let anon = Viewer {
            labels: vec!["p1".into()],
            name: None,
        };
        assert!(owned(true, &anon).is_empty());
        // Operator mode (loopback, no login): every library.
        assert_eq!(owned(false, &v), v.labels);
    }
}
