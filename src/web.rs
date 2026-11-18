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
        auth.logout(&sid);
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
        page(&st.base_path, title, &body, &st.nav_html, "", ""),
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

/// A page with this app's header: the name and the nav switcher.
fn page_for(st: &AppState, title: &str, body: &str) -> Html<String> {
    page(
        &st.base_path,
        title,
        body,
        &st.nav_html,
        "",
        &version_footer(),
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
    )
}

fn page(base: &str, title: &str, body: &str, nav: &str, head: &str, footer: &str) -> Html<String> {
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
<header><a href="./" class="brand">lichtbild</a><a href="map" class="views">Map</a><form class="search" action="search" method="get" role="search"><input type="search" name="q" placeholder="Search" aria-label="Search" maxlength="200"></form>{nav}</header>
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
pub fn scope(viewer: &Viewer, who: &str) -> Option<Vec<String>> {
    if who == "all" {
        (viewer.labels.len() > 1).then(|| viewer.labels.clone())
    } else {
        viewer.may_see(who).then(|| vec![who.to_string()])
    }
}

async fn home(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let idx = st.index.clone();
    let counts = match tokio::task::spawn_blocking(move || idx.counts()).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut body = String::from("<h1>Timelines</h1>\n<ul class=\"people\">\n");
    for l in viewer.labels.iter() {
        let n = counts.get(l).copied().unwrap_or(0);
        body += &format!(
            "<li><a href=\"t/{l}\">{l}</a> <span class=\"n\">{n}</span></li>\n",
            l = esc(l)
        );
    }
    // The merged view counts the libraries this person may see.
    let total: u64 = viewer
        .labels
        .iter()
        .map(|l| counts.get(l).copied().unwrap_or(0))
        .sum();
    if viewer.labels.len() > 1 {
        body +=
            &format!("<li><a href=\"t/all\">merged</a> <span class=\"n\">{total}</span></li>\n");
    }
    body += "</ul>\n";
    if st.auth.is_some() {
        body += "<form class=\"signout\" method=\"post\" action=\"auth/logout\"><button type=\"submit\">Sign out</button></form>\n";
    }
    page_for(&st, "lichtbild", &body).into_response()
}

#[derive(Deserialize)]
struct Cursor {
    before: Option<String>,
    id: Option<u64>,
    day: Option<String>,
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
pub fn render_jump(who: &str, months: &[(String, u64)]) -> String {
    if months.is_empty() {
        return String::new();
    }
    let who = esc(who);
    let name = |ym: &str| {
        let m: usize = ym.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(0);
        MONTHS.get(m.wrapping_sub(1)).copied().unwrap_or("")
    };
    let mut rail = String::from("<nav class=\"rail\" aria-label=\"Jump to date\">");
    let mut menu = String::from(
        "<details class=\"jumpmenu\"><summary>Jump to date</summary><div class=\"jm\">",
    );
    let mut year = "";
    for (ym, n) in months {
        let y = ym.get(..4).unwrap_or("");
        if y != year {
            if !year.is_empty() {
                menu += "</div></div>";
            }
            year = y;
            rail += &format!(
                "<a class=\"y\" href=\"t/{who}?from={y}\">{y}</a>",
                y = esc(y)
            );
            menu += &format!(
                "<div class=\"jy\"><a class=\"y\" href=\"t/{who}?from={y}\">{y}</a><div class=\"chips\">",
                y = esc(y)
            );
        }
        let short = name(ym).get(..3).unwrap_or("");
        rail += &format!(
            "<a class=\"m\" data-n=\"{n}\" data-ym=\"{ym}\" href=\"t/{who}?from={ym}\" title=\"{full} {y} · {n}\"><span>{short}</span></a>",
            ym = esc(ym),
            full = name(ym),
            y = esc(y),
        );
        menu += &format!(
            "<a href=\"t/{who}?from={ym}\">{short} <span class=\"n\">{n}</span></a>",
            ym = esc(ym)
        );
    }
    menu += "</div></div></div></details>";
    rail += "</nav>";
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
        s += &format!(
            "<a class=\"item{cls}{vid}\" id=\"i{id}\" href=\"v/{who}/{id}\" title=\"{t}\">{img}{dur}</a>",
            cls = if weak { " weak" } else { "" },
            vid = if it.video_ms.is_some() { " video" } else { "" },
            id = it.id,
            who = esc(who),
            t = esc(&title),
        );
    }
    if open {
        s += "</div>\n";
    }
    if more {
        if let (Some(last), Some(day)) = (items.last(), prev.as_deref()) {
            s += &format!(
                "<div class=\"more\" hx-get=\"t/{who}?before={b}&amp;id={id}&amp;day={d}\" hx-trigger=\"revealed\" hx-swap=\"outerHTML\">loading…</div>\n",
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
    // A continuation carries before/id; a jump carries from (id 0 sorts
    // before every real id, so the bound month is excluded entirely).
    let before = cur
        .before
        .clone()
        .zip(cur.id)
        .or_else(|| from_bound.clone().map(|b| (b, 0)));
    let res = tokio::task::spawn_blocking(move || {
        let items = idx.timeline(
            &labels,
            before.as_ref().map(|(t, i)| (t.as_str(), *i)),
            PAGE_SIZE as usize + 1,
        )?;
        let months = if full_page {
            idx.months(&labels)?
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
    let fragment = render_items(&who, &items, cur.day.as_deref(), more);
    if !full_page {
        return Html(fragment).into_response();
    }
    let title = if who == "all" {
        "merged".to_string()
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
                Some(ym) => format!("t/{}?from={}", esc(&who), esc(&ym)),
                None => format!("t/{}", esc(&who)),
            };
            format!("<p class=\"newer\"><a href=\"{href}\">↑ Newer photos</a></p>\n")
        }
    };
    let body = format!(
        "<h1>{}</h1>\n{}{}<div class=\"timeline\">\n{}</div>\n{}",
        esc(&title),
        render_jump(&who, &months),
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
);

#[derive(Deserialize)]
pub struct ViewerQuery {
    /// Start a video at this second (from a transcript search hit).
    t: Option<f32>,
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
         <div class=\"tpanel\"><p class=\"dim\">Transcript ({}{}), made in the house by {}</p><ol>",
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

pub fn render_info(
    rec: &index::Record,
    exif: &[(&'static str, String)],
    places: Option<&crate::places::Places>,
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
    match (rec.lat, rec.lon) {
        (Some(lat), Some(lon)) => {
            let coords = format!("{lat:.5}, {lon:.5}");
            let place = places.and_then(|p| p.nearest(lat, lon));
            let v = match place {
                Some(n) => format!(
                    "near {} <span class=\"dim\">({:.0} km)</span><br><span class=\"dim\">{}</span>",
                    esc(&n.label),
                    n.km.max(1.0),
                    coords
                ),
                None => coords,
            };
            rows.push(("Place", v));
        }
        _ => rows.push((
            "Place",
            "<span class=\"dim\">no position recorded</span>".into(),
        )),
    }
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
    let max_file_bytes = st.max_file_bytes;
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ViewerItem>> {
        let Some(rec) = idx.locate(id)? else {
            return Ok(None);
        };
        // The item must belong to this timeline (prepares per-person scoping).
        if !labels.contains(&rec.label) {
            return Ok(None);
        }
        let (newer, older) = idx.neighbours(&labels, &rec.taken_local, id)?;
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
        Ok(Some((rec, newer, older, prefetch, exif, transcript)))
    })
    .await;
    let (rec, newer, older, prefetch, exif, transcript) = match res {
        Ok(Ok(Some(x))) => x,
        Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "no such photo").into_response(),
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let w = esc(&who);
    let link = |rel: &str, label: &str, target: Option<u64>| {
        match target {
        Some(t) => format!("<a class=\"{rel}\" rel=\"{rel}\" href=\"v/{w}/{t}\" aria-label=\"{label}\">{arrow}</a>",
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
    let body = format!(
        "<div class=\"viewer\">\n\
         <div class=\"vbar\"><a class=\"back\" href=\"t/{w}?before={taken}&amp;id={after}#i{id}\">← Timeline</a> <span class=\"when\">{when}</span>{tr}{info}</div>\n\
         <div class=\"stage\">{prev}{photo}{next}</div>\n\
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
        info = render_info(&rec, &exif, st.places.as_deref()),
        prev = link("prev", "Newer photo", newer),
        next = link("next", "Older photo", older),
    );
    page_for(&st, "lichtbild", &body).into_response()
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    who: Option<String>,
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
    let q2 = q.clone();
    let res = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Vec<(crate::search::Hit, index::Record)>> {
            let mut out = Vec::new();
            for h in sx.query(&q2, &labels)?.into_iter().take(200) {
                if let Some(rec) = idx.locate(h.id)? {
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
    let body = render_search(&who, &q, &hits, st.places.as_deref());
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
        s += &format!(
            "<div class=\"result\"><a class=\"item{vid}\" href=\"v/{who}/{id}\">{img}</a>\
             <div class=\"rmeta\"><span class=\"when\">{when}</span>{place}{lines}</div></div>",
            vid = if rec.video.is_some() { " video" } else { "" },
            when = esc(&rec.taken_local.replace('T', " ")),
        );
    }
    s += "</section>";
    s
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
