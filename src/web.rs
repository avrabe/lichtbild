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
    /// Photos are looked at: text, things, what they show (DD-023, DD-024).
    pub vision_on: bool,
    /// The version results are stored with.
    pub vision_version: String,
    /// Picture vectors and the query encoder (REQ-SEE-002).
    pub space: Arc<crate::embed::Space>,
    /// The world map's files and positions cache (REQ-MAP-001).
    pub map: Arc<crate::map::MapState>,
    /// Faces and the names given to them (REQ-FACE-002, REQ-FACE-003).
    pub people: Arc<crate::people::People>,
    /// Search rebuilt after a correction, one at a time (REQ-FIX-001).
    pub rebuild: Arc<Later>,
    /// Thumbnail scores for the best shot of a series (REQ-WEB-021).
    pub best: Arc<crate::best::Best>,
}

/// A search rebuild in the background that runs again, once, if asked
/// while it runs: many corrections in a row rebuild at most twice.
#[derive(Debug, Default)]
pub struct Later {
    busy: std::sync::atomic::AtomicBool,
    again: std::sync::atomic::AtomicBool,
}

/// Rebuild search in the background, after a correction.
fn rebuild_search(st: &AppState) {
    use std::sync::atomic::Ordering::SeqCst;
    let later = st.rebuild.clone();
    later.again.store(true, SeqCst);
    if later.busy.swap(true, SeqCst) {
        return;
    }
    let (search, idx, places) = (st.search.clone(), st.index.clone(), st.places.clone());
    tokio::task::spawn_blocking(move || loop {
        later.again.store(false, SeqCst);
        if let Err(e) = search.rebuild(&idx, places.as_deref()) {
            tracing::warn!("search rebuild after a correction failed: {e:#}");
        }
        later.busy.store(false, SeqCst);
        if !later.again.load(SeqCst) || later.busy.swap(true, SeqCst) {
            break;
        }
    });
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
            vision_on: cfg.vision_dir.is_some(),
            vision_version: cfg.vision_version.clone(),
            space: Arc::new(crate::embed::Space::with_dir(cfg.vision_dir.as_deref())),
            map: Arc::new(crate::map::MapState::new(&cfg.map)),
            people: Arc::new(crate::people::People::default()),
            rebuild: Arc::new(Later::default()),
            best: Arc::new(crate::best::Best::default()),
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
        .route("/show/{who}", get(show))
        .route("/search", get(search))
        .route("/suggest", get(suggest))
        .route("/hide/{id}", axum::routing::post(hide))
        .route("/unhide/{id}", axum::routing::post(unhide))
        .route("/hide-many", axum::routing::post(hide_many))
        .route("/unhide-many", axum::routing::post(unhide_many))
        .route("/keep/{id}", axum::routing::post(keep))
        .route("/hidden", get(hidden_page))
        .route("/fav/{id}", axum::routing::post(fav))
        .route("/unfav/{id}", axum::routing::post(unfav))
        .route("/fav-many", axum::routing::post(fav_many))
        .route("/unfav-many", axum::routing::post(unfav_many))
        .route("/favourites", get(favourites_page))
        .route("/albums", get(albums_page).post(album_create))
        .route("/album/{id}", get(album_page))
        .route("/album-add", axum::routing::post(album_add))
        .route("/album/{id}/remove", axum::routing::post(album_remove))
        .route("/album/{id}/rename", axum::routing::post(album_rename))
        .route("/album/{id}/delete", axum::routing::post(album_delete))
        .route("/album/{id}/share", axum::routing::post(album_share))
        .route("/days/{who}", get(days_page))
        .route("/people", get(people_page))
        .route("/people/{item}/{face}", get(group_page))
        .route("/people/name", axum::routing::post(people_name))
        .route("/people/not", axum::routing::post(people_not))
        .route("/person/{id}", get(person_page))
        .route("/person/{id}/rename", axum::routing::post(person_rename))
        .route("/person/{id}/delete", axum::routing::post(person_delete))
        .route("/face/{item}/{face}", get(face_crop))
        .route("/status", get(status_page))
        .route("/place/{id}", axum::routing::post(place))
        .route("/unplace/{id}", axum::routing::post(unplace))
        .route("/words", get(words_page))
        .route("/things", get(things_page))
        .route("/corrections", get(corrections_page))
        .route("/corrections.jsonl", get(corrections_jsonl))
        .route("/fix/thing", axum::routing::post(fix_thing))
        .route("/fix/text", axum::routing::post(fix_text))
        .route("/fix/off", axum::routing::post(fix_off))
        .route("/trips", get(trips_page))
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
         <li><a href=\"trips\"><b>Trips</b><span>Days away from home, found by place</span></a></li>\
         <li><a href=\"people\"><b>People</b><span>Who is in your photos</span></a></li>\
         <li><a href=\"things\"><b>Things</b><span>Cats, bikes, cakes… in your photos</span></a></li>\
         <li><a href=\"words\"><b>Words</b><span>What was said and written</span></a></li>\
         <li class=\"sep\"><a href=\"favourites\"><b>Favourites</b><span>The ones you love</span></a></li>\
         <li><a href=\"albums\"><b>Albums</b><span>Your own collections</span></a></li>\
         <li><a href=\"hidden\"><b>Hidden</b><span>What you chose not to see</span></a></li>\
         <li><a href=\"duplicates\"><b>Duplicate files</b><span>Identical files to clean up</span></a></li>\
         <li><a href=\"status\"><b>Status</b><span>Scan, transcripts, version</span></a></li>\
         <li><button type=\"button\" class=\"keyhelp\"><b>Keys and gestures</b><span>Press ? on any page</span></button></li>\
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
<header><a href="./" class="brand">lichtbild</a>{menu}<form class="search" action="search" method="get" role="search"><input type="search" name="q" placeholder="Search" aria-label="Search" maxlength="200" autocomplete="off" autocapitalize="off" spellcheck="false" hx-get="suggest" hx-trigger="input changed delay:120ms" hx-target="next .suggest" hx-sync="this:replace" aria-autocomplete="list" aria-controls="suggest"><div id="suggest" class="suggest" role="listbox" aria-label="Suggestions"></div></form>{nav}</header>
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
    let md_today = today.get(5..10).unwrap_or("").to_string();
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
        body += &format!(
            "<section class=\"otd\"><h2><a href=\"days/{w}?md={md}\" title=\"Every picture of this day, in every year\">On this day</a></h2><div class=\"strip\">",
            w = esc(&who),
            md = esc(&md_today),
        );
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
             <a href=\"favourites\"><b>Favourites</b><span>The ones you love</span></a>\
             <a href=\"albums\"><b>Albums</b><span>Your own collections</span></a>\
             <a href=\"people\"><b>People</b><span>Who is in your photos</span></a>\
             <a href=\"map\"><b>Map</b><span>Where pictures were taken</span></a>\
             <a href=\"trips\"><b>Trips</b><span>Days away from home</span></a>\
             <a href=\"things\"><b>Things</b><span>What is in your photos</span></a>\
             <a href=\"words\"><b>Words</b><span>What was said and written</span></a>\
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
/// Seconds apart at most between two pictures of one series.
const SERIES_GAP: i64 = 120;

/// For each item: how many items its series holds when it leads one
/// (REQ-WEB-012), 0 when it belongs to the series before it, 1 when alone.
/// A series is photos of one day, each taken at most SERIES_GAP seconds
/// after the one before and looking at least SERIES_MIN alike.
pub fn series(items: &[index::Item], space: Option<&crate::embed::Space>) -> Vec<usize> {
    let mut out = vec![1; items.len()];
    let Some(space) = space else {
        return out;
    };
    let t = |it: &index::Item| {
        chrono::NaiveDateTime::parse_from_str(&it.taken_local, "%Y-%m-%dT%H:%M:%S").ok()
    };
    let mut lead = 0;
    for i in 1..items.len() {
        let (a, b) = (&items[i - 1], &items[i]);
        let joined = a.video_ms.is_none()
            && b.video_ms.is_none()
            && a.taken_local.get(..10) == b.taken_local.get(..10)
            && matches!((t(a), t(b)), (Some(x), Some(y)) if (x - y).num_seconds().abs() <= SERIES_GAP)
            && space
                .alike(&a.hash, &b.hash)
                .is_some_and(|c| c >= crate::embed::SERIES_MIN);
        if joined {
            out[lead] += 1;
            out[i] = 0;
        } else {
            lead = i;
        }
    }
    out
}

/// The series of `items` (REQ-WEB-012), each led by its best shot
/// (REQ-WEB-021); the thumbnails are read off the async threads.
async fn series_best(
    st: &AppState,
    mut items: Vec<index::Item>,
) -> Result<(Vec<index::Item>, Vec<usize>), tokio::task::JoinError> {
    let space = st.vision_on.then(|| st.space.clone());
    let (best, dd) = (st.best.clone(), st.data_dir.clone());
    tokio::task::spawn_blocking(move || {
        let runs = series(&items, space.as_deref());
        crate::best::best_first(&mut items, &runs, |it| best.of(&dd, it));
        (items, runs)
    })
    .await
}

pub fn render_items(
    who: &str,
    items: &[index::Item],
    series: &[usize],
    prev_day: Option<&str>,
    more: bool,
    kq: &str,
) -> String {
    let mut s = String::new();
    // Items of the open series still to come.
    let mut left = 0usize;
    let mut keep = String::new();
    let mut prev = prev_day.map(str::to_string);
    let mut open = prev.is_some();
    if open {
        s += "<div class=\"grid\">";
    }
    for (i, it) in items.iter().enumerate() {
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
            s += &format!(
                "<h4 class=\"day\"><a href=\"days/{w}?from={d}&amp;to={d}\" title=\"Only this day\">{d}</a></h4>\n<div class=\"grid\">",
                w = esc(who),
                d = esc(&day)
            );
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
        let dur = if it.fav {
            format!(
                "{dur}<span class=\"heart\" title=\"Favourite\" aria-label=\"Favourite\">♥</span>"
            )
        } else {
            dur
        };
        let n = series.get(i).copied().unwrap_or(1);
        let follows = n == 0 && left > 0;
        let dur = if n >= 2 {
            left = n - 1;
            s += &format!("<span class=\"series\" id=\"s{}\">", it.id);
            // Opened, the series offers to keep its best shot (this one,
            // REQ-WEB-021) and hide the others for this person.
            let others: Vec<String> = items[i + 1..(i + n).min(items.len())]
                .iter()
                .map(|o| o.id.to_string())
                .collect();
            keep = format!(
                "<button type=\"button\" class=\"keepbest\" data-ids=\"{}\" data-best=\"{}\">Keep the best, hide {}</button>",
                others.join(","),
                it.id,
                if others.len() == 1 { "the other".to_string() } else { format!("the other {}", others.len()) },
            );
            format!(
                "{dur}<span class=\"stack\" role=\"button\" tabindex=\"0\" aria-expanded=\"false\" title=\"{n} pictures taken one after another, the best first\">{n}</span>"
            )
        } else {
            dur
        };
        s += &format!(
            "<a class=\"item{cls}{vid}{later}\" id=\"i{id}\" href=\"v/{who}/{id}{vq}\" title=\"{t}\">{img}{dur}</a>",
            later = if follows { " later" } else { "" },
            cls = if weak { " weak" } else { "" },
            vid = if it.video_ms.is_some() { " video" } else { "" },
            id = it.id,
            who = esc(who),
            t = esc(&title),
            vq = viewer_query(kq),
        );
        if follows {
            left -= 1;
            if left == 0 {
                s += &std::mem::take(&mut keep);
                s += "</span>";
            }
        }
    }
    if left > 0 {
        s += &keep;
        s += "</span>";
    }
    if open {
        s += "</div>\n";
    }
    if more {
        // The oldest on the page: a series' best shot may have moved ahead.
        let last = items
            .iter()
            .min_by(|a, b| (&a.taken_local, a.id).cmp(&(&b.taken_local, b.id)));
        if let (Some(last), Some(day)) = (last, prev.as_deref()) {
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
        let favs = idx.favs(&me)?;
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
            it.fav = favs.contains(&it.id);
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
    let (items, runs) = match series_best(&st, items).await {
        Ok(x) => x,
        Err(e) => return internal(e),
    };
    let fragment = render_items(&who, &items, &runs, cur.day.as_deref(), more, kq);
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
    let albums = album_template(&albums_of(&st, person(&viewer)).await);
    let body = format!(
        "<div class=\"thead\"><h1>{}</h1>{}<a class=\"tbtn\" href=\"show/{w}\" title=\"Slideshow\">▶ Slideshow</a><button type=\"button\" class=\"tbtn select\" hidden aria-pressed=\"false\">Select</button></div>\n{}{}<div class=\"timeline\">\n{}</div>\n{}{albums}",
        esc(&title),
        kind_switch(&format!("t/{}", esc(&who)), kind),
        render_jump(&who, &months, kq),
        newer,
        fragment,
        if items.is_empty() {
            "<p>No items yet. Run <code>lichtbild scan</code>.</p>"
        } else {
            ""
        },
        w = esc(&who),
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
pub fn render_text(text: &crate::ocr::Text, q: Option<&str>, fixes: &str) -> (String, String) {
    if text.state != "ok" || text.lines.is_empty() {
        if fixes.is_empty() {
            return (String::new(), String::new());
        }
        // Every line was taken out: the panel stays, to undo it.
        return (
            format!("<details class=\"textp\"><summary title=\"Text in the picture\" aria-label=\"Text in the picture\">Aa</summary><div class=\"xpanel\"><p class=\"dim\">No text left in this picture.</p>{fixes}</div></details>"),
            String::new(),
        );
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
    let mut layer = String::from(
        "<div class=\"boxlayer textlayer\" data-for=\"textp\" hidden aria-hidden=\"true\">",
    );
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
    panel += fixes;
    panel += "</div></details>";
    layer += "</div>";
    (panel, layer)
}

/// The things found in a picture (REQ-SEE-001) and the pictures that look
/// like it (REQ-SEE-002): a panel with a chip per kind (a tap searches for
/// it) and a strip of look-alikes, and a layer of boxes over the picture
/// while the panel is open.
pub fn render_things(
    who: &str,
    id: u64,
    back: &str,
    hash: &str,
    raw: &[crate::objects::Object],
    fixes: &crate::fixes::Fixes,
    similar: &[(u64, index::Record)],
) -> (String, String) {
    if raw.is_empty() && similar.is_empty() {
        return (String::new(), String::new());
    }
    let objects = &fixes.apply(hash, raw.to_vec());
    let mut kinds: Vec<(u8, usize)> = Vec::new();
    for o in objects {
        match kinds.iter_mut().find(|(k, _)| *k == o.kind) {
            Some((_, n)) => *n += 1,
            None => kinds.push((o.kind, 1)),
        }
    }
    let mut panel = String::from(
        "<details class=\"things\"><summary title=\"Things in the picture\" aria-label=\"Things in the picture\">◎</summary><div class=\"kpanel\">",
    );
    if !kinds.is_empty() {
        panel += "<p class=\"dim\">Found in this picture, in the house. A thing searches for it.</p><p class=\"chips\">";
        for (k, n) in &kinds {
            let (_, one, many) = crate::objects::KINDS[*k as usize];
            let label = if *n > 1 {
                format!("{n} {many}")
            } else {
                one.to_string()
            };
            panel += &format!(
                "<a class=\"chip\" href=\"search?q={q}\">{l}</a>",
                q = esc(&urlencode(one)),
                l = esc(&label)
            );
        }
        panel += "</p>";
    }
    if !similar.is_empty() {
        panel += "<h3>Looks like this</h3><div class=\"simstrip\">";
        for (id, rec) in similar {
            panel += &format!(
                "<a href=\"v/{who}/{id}\" title=\"{t}\"><img src=\"thumb/{id}?h={h}\" loading=\"lazy\" alt=\"\"></a>",
                who = esc(who),
                t = esc(&rec.taken_local.replace('T', " ")),
                h = esc(rec.hash.get(..12).unwrap_or("")),
            );
        }
        panel += "</div>";
    }
    panel += &render_thing_fixes(id, back, hash, raw, fixes);
    panel += "</div></details>";
    let mut layer = String::from(
        "<div class=\"boxlayer thinglayer\" data-for=\"things\" hidden aria-hidden=\"true\">",
    );
    // Every find has its box, by its number in "Something wrong?"; one
    // left out shows only while its line there is pointed at.
    for (n, o) in raw.iter().enumerate() {
        let [x0, y0, x1, y1] = o.bbox;
        let kept = fixes.verdict(hash, o);
        let name = crate::fixes::kind_name(kept.unwrap_or(o.kind));
        layer += &format!(
            "<a href=\"search?q={q}\" data-n=\"{n}\"{gone} data-quad=\"{x0:.4},{y0:.4},{x1:.4},{y0:.4},{x1:.4},{y1:.4},{x0:.4},{y1:.4}\" title=\"{name}\"><span>{name}</span></a>",
            q = esc(&urlencode(name)),
            gone = if kept.is_none() { " class=\"gone\"" } else { "" },
            name = esc(name),
        );
    }
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

/// Photos a slideshow holds at most.
const SHOW_MAX: usize = 500;

#[derive(Deserialize)]
pub struct ShowQuery {
    /// Start at this photo; without it, at the newest.
    at: Option<u64>,
    /// Only this person's favourites (REQ-WEB-016), each one shown.
    fav: Option<u8>,
    /// Only this album of theirs (REQ-WEB-017), each picture shown.
    album: Option<u64>,
    /// Only photos of this person (REQ-FACE-003).
    person: Option<u64>,
    /// Only these days (a trip, a day, this day in every year).
    from: Option<String>,
    to: Option<String>,
    md: Option<String>,
}

/// "19 April 2014, 12:00" from "2014-04-19T12:00:00".
fn long_date(taken: &str) -> String {
    let m: usize = taken.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(0);
    match (
        taken.get(8..10).and_then(|d| d.parse::<u32>().ok()),
        MONTHS.get(m.wrapping_sub(1)),
        taken.get(..4),
        taken.get(11..16),
    ) {
        (Some(d), Some(name), Some(y), Some(hm)) => format!("{d} {name} {y}, {hm}"),
        _ => taken.replace('T', " "),
    }
}

/// GET /show/{who}?at=: a slideshow of the timeline's photos from `at` on,
/// older and older (REQ-WEB-013): full screen, one photo of each series,
/// no videos, nothing hidden. assets/lichtbild.js plays it.
async fn show(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(who): UrlPath<String>,
    Query(sq): Query<ShowQuery>,
) -> Response {
    let Some(labels) = scope(&viewer, &who) else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    let idx = st.index.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let space = st.vision_on.then(|| st.space.clone());
    let fav_only = sq.fav.is_some();
    let album = sq.album;
    let of = sq.person;
    let people = st.people.clone();
    let days = DaysQuery {
        from: sq.from.clone(),
        to: sq.to.clone(),
        md: sq.md.clone(),
        ..Default::default()
    };
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(u64, String, String)>> {
        let hidden = idx.hidden(&me)?;
        // A chosen set: favourites, one of this person's albums, or the
        // photos of someone.
        let dq = DaysQuery {
            from: sq.from.clone(),
            to: sq.to.clone(),
            md: sq.md.clone(),
            ..Default::default()
        };
        let favs = match (sq.fav, sq.album) {
            _ if dq.valid() => Some(
                day_items(&idx, &dups, &labels, &hidden, &dq, true)?
                    .into_iter()
                    .map(|i| i.id)
                    .collect(),
            ),
            _ if sq.person.is_some() => {
                let who = crate::people::Who::Person(sq.person.unwrap_or_default());
                let (items, _) = photos_of(&idx, &people, &labels, &me, who)?;
                Some(items.into_iter().map(|i| i.id).collect())
            }
            (_, Some(a)) => match seen_album(&idx, a, &me)? {
                Some(_) => Some(idx.album_items(a)?),
                None => return Ok(Vec::new()),
            },
            (Some(_), None) => Some(idx.favs(&me)?),
            (None, None) => None,
        };
        let start = match sq.at {
            Some(at) => match idx.locate(at)?.filter(|r| labels.contains(&r.label)) {
                Some(r) => Some((r.taken_local, at + 1)),
                None => return Ok(Vec::new()),
            },
            None => None,
        };
        let items = match &favs {
            // A chosen set is read by id, not by scanning the library.
            Some(f) => {
                let mut v = idx.items_of(f.iter().copied(), &labels, |id, rec| {
                    rec.video.is_none() && !hidden.contains(&id)
                })?;
                if let Some((t, i)) = &start {
                    v.retain(|it| (it.taken_local.as_str(), it.id) < (t.as_str(), *i));
                }
                v.truncate(SHOW_MAX);
                v
            }
            None => idx.timeline_where(
                &labels,
                start.as_ref().map(|(t, i)| (t.as_str(), *i)),
                SHOW_MAX,
                |id, rec| rec.video.is_none() && dups.shown(id, &labels, &hidden).shown,
            )?,
        };
        let runs = match favs {
            Some(_) => vec![1; items.len()],
            None => series(&items, space.as_deref()),
        };
        let mut out = Vec::new();
        for (it, n) in items.iter().zip(runs) {
            if n == 0 {
                continue;
            }
            if let Some(rec) = idx.locate(it.id)? {
                out.push((it.id, photo_src(it.id, &rec), long_date(&it.taken_local)));
            }
        }
        Ok(out)
    })
    .await;
    let slides = match res {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let back = match (album, slides.first()) {
        _ if days.valid() => format!("days/{}?{}", esc(&who), days.query()),
        _ if of.is_some() => format!("person/{}", of.unwrap_or_default()),
        (Some(a), _) => format!("album/{a}"),
        _ if fav_only => "favourites".to_string(),
        (None, Some((id, _, _))) => format!("v/{}/{id}", esc(&who)),
        (None, None) => format!("t/{}", esc(&who)),
    };
    let list: String = slides
        .iter()
        .map(|(id, src, when)| {
            format!(
                "<li data-src=\"{}\" data-when=\"{}\" data-href=\"v/{}/{id}\"></li>",
                esc(src),
                esc(when),
                esc(&who)
            )
        })
        .collect();
    let body = format!(
        "<div class=\"show\" data-interval=\"5000\" role=\"region\" aria-label=\"Slideshow\">\
         <img class=\"slide on\" alt=\"\"><img class=\"slide\" alt=\"\">\
         <div class=\"sbar\"><span class=\"cap\" aria-live=\"polite\"></span>\
         <span class=\"sctl\"><button type=\"button\" class=\"sprev\" aria-label=\"Previous\">‹</button>\
         <button type=\"button\" class=\"play\" aria-label=\"Pause\">❚❚</button>\
         <button type=\"button\" class=\"snext\" aria-label=\"Next\">›</button>\
         <button type=\"button\" class=\"sfs\" hidden aria-label=\"Full screen\">⛶</button>\
         <a class=\"close\" href=\"{back}\" aria-label=\"Close\">✕</a></span></div>\
         <p class=\"send\" hidden>The end. <a href=\"{back}\">Back</a></p>\
         <ol class=\"slides\" hidden>{list}</ol></div>{empty}",
        empty = if slides.is_empty() {
            "<p>No photos to show.</p>"
        } else {
            ""
        }
    );
    page_for(&st, "lichtbild slideshow", &body).into_response()
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
        let is_fav = idx.favs(&me)?.contains(&id);
        Ok(Some((
            rec, newer, older, prefetch, exif, transcript, copies, is_hidden, is_fav,
        )))
    })
    .await;
    let (rec, newer, older, prefetch, exif, transcript, copies, is_hidden, is_fav) = match res {
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
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(idx3.raw_text(&hash)?.map(|t| (t, idx3.text_fixes(&hash))))
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
        .and_then(|(t, f)| Some((t, f.ok()?)))
    } else {
        None
    };
    let back = format!("v/{who}/{id}{vqs}");
    let (textp, layer) = read
        .map(|(raw, fixes)| {
            let fixhtml = render_text_fixes(id, &back, &raw, &fixes);
            let t = crate::fixes::apply_text(raw, &fixes);
            render_text(&t, vq.q.as_deref(), &fixhtml)
        })
        .unwrap_or_default();
    // Things in it, and pictures that look like it (one copy each, nothing
    // hidden, in this timeline's libraries).
    let (things, thinglayer) = if rec.video.is_none() && st.vision_on {
        let (idx4, space, dups, hash, me) = (
            st.index.clone(),
            st.space.clone(),
            st.dups.clone(),
            rec.hash.clone(),
            person(&viewer),
        );
        let labels = scope(&viewer, &who).unwrap_or_default();
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let objects = idx4.raw_objects(&hash)?;
            let fixes = idx4.fixes()?;
            let hidden = idx4.hidden(&me)?;
            let mut similar = Vec::new();
            for (ids, _) in space.similar(&hash, &labels, 40) {
                let Some(id) = ids
                    .into_iter()
                    .find(|i| dups.shown(*i, &labels, &hidden).shown)
                else {
                    continue;
                };
                if let Some(r) = idx4.locate(id)? {
                    similar.push((id, r));
                }
                if similar.len() >= 12 {
                    break;
                }
            }
            Ok((objects, fixes, hash, similar))
        })
        .await;
        match res {
            Ok(Ok((o, f, h, s))) => render_things(&who, id, &back, &h, &o, &f, &s),
            _ => Default::default(),
        }
    } else {
        Default::default()
    };
    // Who is in it (REQ-FACE-003).
    let faces = if rec.video.is_none() && !st.people.is_empty() {
        let (idx5, people, hash, back, v5) = (
            st.index.clone(),
            st.people.clone(),
            rec.hash.clone(),
            format!("v/{who}/{id}{vqs}"),
            viewer.clone(),
        );
        tokio::task::spawn_blocking(move || render_faces(&idx5, &people, &v5, id, &hash, &back))
            .await
            .unwrap_or_default()
    } else {
        String::new()
    };
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
    // Take the original along: a download under its own name, and the
    // system share sheet where the browser offers one (assets/lichtbild.js
    // shows the button), so a photo goes straight to Photos or a message.
    let get = {
        let name = index::rel_from_bytes(&rec.rel)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!(
            "<p class=\"getrow\"><a class=\"dl\" href=\"original/{id}\" download=\"{n}\">Download</a><button type=\"button\" class=\"share\" hidden data-src=\"original/{id}\" data-name=\"{n}\">Share…</button></p>",
            n = esc(&name)
        )
    };
    // Into one of this person's albums, or a new one (REQ-WEB-017).
    let to_album = {
        let opts = album_options(&albums_of(&st, person(&viewer)).await);
        format!(
            "<form class=\"albumf\" method=\"post\" action=\"album-add\"><input type=\"hidden\" name=\"ids\" value=\"{id}\"><input type=\"hidden\" name=\"back\" value=\"v/{w}/{id}{vqs}\">\
             <select name=\"album\" aria-label=\"Album\">{opts}<option value=\"new\">New album…</option></select>\
             <input type=\"text\" name=\"name\" maxlength=\"80\" placeholder=\"Name of the new album\" aria-label=\"Name of the new album\">\
             <button type=\"submit\">Add to album</button></form>"
        )
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
         <div class=\"vbar\"><a class=\"back\" href=\"t/{w}?before={taken}&amp;id={after}{kq}#i{id}\" aria-label=\"Timeline\">←<span class=\"lbl\"> Timeline</span></a> <a class=\"when\" href=\"days/{w}?from={day}&amp;to={day}#i{id}\" title=\"All pictures of this day\">{when}</a>{tr}{textp}{things}{faces}{info}{heart}<a class=\"slideshow\" href=\"show/{w}?at={id}\" title=\"Slideshow from here (s)\" aria-label=\"Slideshow from here\">▶</a><button type=\"button\" class=\"fs\" hidden title=\"Full screen (f)\" aria-label=\"Full screen\">⛶</button></div>\n\
         <div class=\"stage\">{prev}{photo}{layer}{thinglayer}{next}</div>\n\
         <section class=\"under\" hidden></section>\n\
         {prefetch}</div>",
        taken = esc(&rec.taken_local),
        day = esc(rec.taken_local.get(..10).unwrap_or("")),
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
                    &format!("<div class=\"hiderow\">{get}{to_album}{hide}</div></dl></details>"),
                ),
        prev = link("prev", "Newer photo", newer),
        next = link("next", "Older photo", older),
        // A favourite (REQ-WEB-016): a form, so it works without JavaScript;
        // assets/lichtbild.js toggles it in place.
        heart = if is_fav {
            format!("<form class=\"favf\" method=\"post\" action=\"unfav/{id}\"><input type=\"hidden\" name=\"back\" value=\"v/{w}/{id}{vqs}\"><button type=\"submit\" class=\"heart on\" aria-pressed=\"true\" title=\"Favourite (.)\" aria-label=\"Favourite\">♥</button></form>")
        } else {
            format!("<form class=\"favf\" method=\"post\" action=\"fav/{id}\"><input type=\"hidden\" name=\"back\" value=\"v/{w}/{id}{vqs}\"><button type=\"submit\" class=\"heart\" aria-pressed=\"false\" title=\"Favourite (.)\" aria-label=\"Favourite\">♡</button></form>")
        },
    );
    page_for(&st, "lichtbild", &body).into_response()
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    who: Option<String>,
    kind: Option<String>,
}

/// Search results: exact hits with their records, pictures that look like
/// the words (REQ-SEE-002), and terms that narrow them down
/// (REQ-SEARCH-003).
type Found = (
    Vec<(crate::search::Hit, index::Record)>,
    Vec<(u64, index::Record)>,
    Vec<crate::search::Narrow>,
);

/// Completions for the search box as it is typed (REQ-SEARCH-002): only
/// terms that items in the viewer's libraries hold.
async fn suggest(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Query(sq): Query<SearchQuery>,
) -> Response {
    let q: String = sq.q.unwrap_or_default().chars().take(200).collect();
    let labels = viewer.labels.clone();
    let sx = st.search.clone();
    let idx = st.index.clone();
    let me = person(&viewer);
    // Favourites words (REQ-WEB-016) are not in the index: the rest is
    // completed and the word kept in front; a partly typed one is offered.
    let (rest, fav) = crate::search::split_fav(&q);
    let fav_word = q
        .split_whitespace()
        .find(|w| crate::search::split_fav(w).1)
        .map(str::to_string);
    let typing = !q.ends_with(char::is_whitespace);
    let last = q.split_whitespace().last().filter(|_| typing).unwrap_or("");
    let offer = crate::search::fav_completion(last).filter(|_| !fav);
    // "Katze ♥": the favourites word was the last one typed, so "Katze"
    // is finished, not being typed.
    let done = !typing || crate::search::split_fav(last).1;
    let ask = match (fav, done) {
        (false, _) => q.clone(),
        (true, true) if !rest.is_empty() => format!("{rest} "),
        (true, _) => rest,
    };
    let people = st.people.clone();
    let typed = last.to_lowercase();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        // Names of people the viewer has photos of (REQ-FACE-003), that
        // the word being typed begins, with how many photos.
        let mut names: Vec<(String, usize)> = Vec::new();
        if typed.chars().count() >= 2 && !people.is_empty() {
            let hidden = idx.hidden(&me)?;
            let seen = seen_faces(&idx, &people, &labels, &hidden)?;
            for (id, p) in idx.people()? {
                if !p
                    .name
                    .split_whitespace()
                    .any(|w| w.to_lowercase().starts_with(&typed))
                {
                    continue;
                }
                let n = seen
                    .iter()
                    .filter(|f| f.face.who == crate::people::Who::Person(id))
                    .map(|f| f.face.key.0.as_str())
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                if n > 0 {
                    names.push((p.name, n));
                }
            }
            names.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            names.truncate(3);
        }
        // As many as the Favourites page shows: in these libraries and
        // not hidden.
        let n = match offer {
            Some(_) => {
                let hidden = idx.hidden(&me)?;
                idx.items_of(idx.favs(&me)?, &labels, |id, _| !hidden.contains(&id))?
                    .len()
            }
            None => 0,
        };
        let found = if fav && ask.trim().is_empty() {
            Vec::new()
        } else {
            sx.suggest(&ask, &labels, 8)
        };
        Ok((found, n, names))
    })
    .await;
    let (mut found, favs, names) = match res {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut h = String::new();
    if let (Some(word), true) = (offer, favs > 0) {
        let head = q.trim_end().strip_suffix(last).unwrap_or("");
        h += &format!(
            "<a role=\"option\" href=\"search?q={q}\"><span class=\"sk\" title=\"your favourites\" aria-label=\"your favourites\">♥</span><span class=\"st\">{head}<b>{word}</b></span><span class=\"dim\">{favs}</span></a>",
            q = urlencode(&format!("{head}{word}")),
            head = esc(head),
        );
    }
    for (name, n) in &names {
        let head = q.trim_end().strip_suffix(last).unwrap_or("");
        h += &format!(
            "<a role=\"option\" href=\"search?q={q}\"><span class=\"sk\" title=\"person\" aria-label=\"person\">☺</span><span class=\"st\">{head}<b>{name}</b></span><span class=\"dim\">{n}</span></a>",
            q = urlencode(&format!("{head}{name}")),
            head = esc(head),
            name = esc(name),
        );
    }
    if let Some(w) = fav_word {
        for s in &mut found {
            s.query = format!("{w} {}", s.query);
        }
    }
    for s in found {
        let (icon, what) = match s.kind {
            crate::search::Kind::Thing => ("◎", "thing"),
            crate::search::Kind::Place => ("⌖", "place"),
            crate::search::Kind::Date => ("◷", "date"),
            crate::search::Kind::Word => ("❝", "said or written"),
        };
        let head = s.query.strip_suffix(&s.term).unwrap_or("");
        let mut note = s.hint.clone();
        if !note.is_empty() {
            note += " · ";
        }
        note += &s.items.to_string();
        h += &format!(
            "<a role=\"option\" href=\"search?q={q}\"><span class=\"sk\" title=\"{what}\" aria-label=\"{what}\">{icon}</span><span class=\"st\">{head}<b>{term}</b></span><span class=\"dim\">{note}</span></a>",
            q = urlencode(&s.query),
            head = esc(head),
            term = esc(&s.term),
            note = esc(&note),
        );
    }
    ([(axum::http::header::CACHE_CONTROL, "no-store")], Html(h)).into_response()
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
    let explicit_who = sq.who.clone();
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
    let places = st.places.clone();
    let positions = st.positions.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let (kind, _) = kind_filter(sq.kind.as_deref());
    // "Favoriten" or "♥" asks for the viewer's favourites (REQ-WEB-016):
    // the other words are searched, then only favourites are kept; alone,
    // it is the Favourites page.
    let (q2, fav_only) = crate::search::split_fav(&q);
    if fav_only && q2.is_empty() {
        return Redirect::to(&format!("{}/favourites", st.base_path)).into_response();
    }
    // A person's name asks for their photos (REQ-FACE-003): only names
    // of people the viewer has photos of; the other words are searched
    // in those photos. Alone, it is the person's page.
    let of = {
        let (idx, people, labels, me, q2) = (
            st.index.clone(),
            st.people.clone(),
            labels.clone(),
            me.clone(),
            q2.clone(),
        );
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            if people.is_empty() {
                return Ok((q2, None));
            }
            let hidden = idx.hidden(&me)?;
            let seen = seen_faces(&idx, &people, &labels, &hidden)?;
            let names: Vec<(u64, String)> = idx
                .people()?
                .into_iter()
                .filter(|(id, _)| {
                    seen.iter()
                        .any(|f| f.face.who == crate::people::Who::Person(*id))
                })
                .map(|(id, p)| (id, p.name))
                .collect();
            let (rest, pid) = crate::search::split_name(&q2, &names);
            let hashes = pid.map(|p| {
                seen.iter()
                    .filter(|f| f.face.who == crate::people::Who::Person(p))
                    .map(|f| f.face.key.0.clone())
                    .collect::<std::collections::HashSet<String>>()
            });
            Ok((rest, pid.zip(hashes)))
        })
        .await
    };
    let (q2, of) = match of {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    if let Some((pid, _)) = &of {
        if q2.is_empty() {
            return Redirect::to(&format!("{}/person/{pid}", st.base_path)).into_response();
        }
    }
    let space = st.vision_on.then(|| st.space.clone());
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Found> {
        let hidden = idx.hidden(&me)?;
        let favs = if fav_only { Some(idx.favs(&me)?) } else { None };
        let in_photo = |id: u64, hs: &std::collections::HashSet<String>| {
            idx.locate(id)
                .ok()
                .flatten()
                .is_some_and(|r| hs.contains(&r.hash))
        };
        // A favourite counts through any of its copies; so does a photo of
        // the person asked for.
        let wanted = |id: u64| {
            favs.as_ref().is_none_or(|f| {
                f.contains(&id) || dups.copies(id, &labels).iter().any(|c| f.contains(c))
            }) && of.as_ref().is_none_or(|(_, hs)| {
                in_photo(id, hs) || dups.copies(id, &labels).iter().any(|c| in_photo(*c, hs))
            })
        };
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
            if !wanted(h.id) {
                continue;
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
        // Pictures that show what the words describe (REQ-SEE-002),
        // after the exact hits; photos only, so not under "Videos".
        let mut looks: Vec<(u64, index::Record)> = Vec::new();
        if let Some(space) = space.filter(|_| kind != Some(true) && q2.trim().chars().count() >= 3)
        {
            for (ids, _) in space.search(&q2, &labels, 120)? {
                let shown = ids
                    .iter()
                    .copied()
                    .find(|id| dups.shown(*id, &labels, &hidden).shown);
                let Some(id) = shown.or_else(|| {
                    ids.iter()
                        .flat_map(|i| dups.copies(*i, &labels))
                        .find(|c| dups.shown(*c, &labels, &hidden).shown)
                }) else {
                    continue;
                };
                if !wanted(id)
                    || out.iter().any(|(h, _)| h.id == id)
                    || looks.iter().any(|(l, _)| *l == id)
                {
                    continue;
                }
                if let Some(rec) = idx.locate(id)? {
                    looks.push((id, rec));
                }
                if looks.len() >= 60 {
                    break;
                }
            }
        }
        // What the results hold, to narrow them down (REQ-SEARCH-003).
        let pos = positions.get(&idx)?;
        let mut held: Vec<Vec<(crate::search::Kind, String)>> = Vec::new();
        let fixes = idx.fixes()?;
        for (h, rec) in &out {
            let mut terms = Vec::new();
            if let Some(y) = rec
                .taken_local
                .get(..4)
                .filter(|y| y.bytes().all(|b| b.is_ascii_digit()))
            {
                terms.push((crate::search::Kind::Date, y.to_string()));
            }
            if rec.video.is_none() {
                let mut kinds: Vec<u8> = fixes
                    .apply(&rec.hash, idx.raw_objects(&rec.hash)?)
                    .iter()
                    .map(|o| o.kind)
                    .collect();
                kinds.sort_unstable();
                kinds.dedup();
                for k in kinds {
                    if let Some((_, de, _)) = crate::objects::KINDS.get(k as usize) {
                        terms.push((crate::search::Kind::Thing, de.to_string()));
                    }
                }
            }
            if let (Some(p), Some(at)) = (places.as_deref(), pos.get(&h.id)) {
                if let Some(n) = p.nearest(at.lat, at.lon) {
                    terms.push((crate::search::Kind::Place, n.label));
                }
            }
            held.push(terms);
        }
        // A chip must find what it promises: words match within one
        // document, and the year or place of a hit on what was said is
        // not in that document. Each chip is run, counted as the results
        // page would count it, and dropped when it finds nothing.
        let mut narrow = Vec::new();
        for mut n in crate::search::narrow(&q2, &held) {
            let query = format!("{} {}", q2.trim(), crate::search::plain(&n.term));
            let ids: std::collections::HashSet<u64> = sx
                .query(&query, &labels)?
                .into_iter()
                .map(|h| h.id)
                .filter(|id| dups.shown(*id, &labels, &hidden).shown && wanted(*id))
                .filter(|id| {
                    kind.is_none_or(|v| {
                        idx.locate(*id)
                            .ok()
                            .flatten()
                            .is_some_and(|r| r.video.is_some() == v)
                    })
                })
                .collect();
            if !ids.is_empty() {
                n.items = ids.len();
                narrow.push(n);
            }
        }
        Ok((out, looks, narrow))
    })
    .await;
    let (hits, looks, narrow) = match res {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let body = format!(
        "{}{}{}{}",
        kind_switch(&format!("search?q={}", esc(&q)), kind),
        render_narrow(&q, &narrow, &narrow_query(kind, explicit_who.as_deref())),
        // No exact hit but look-alikes: their heading says enough.
        if hits.is_empty() && !looks.is_empty() {
            String::new()
        } else {
            render_search(
                &who,
                &q,
                &hits,
                st.places.as_deref(),
                &st.positions.get(&st.index).unwrap_or_default(),
            )
        },
        render_looks(&who, &q, &looks)
    );
    page_for(&st, "lichtbild search", &body).into_response()
}

/// `&kind=…&who=…` for a link that keeps the photo/video filter and the
/// library searched.
fn narrow_query(kind: Option<bool>, who: Option<&str>) -> String {
    let mut s = match kind {
        Some(true) => "&amp;kind=video".to_string(),
        Some(false) => "&amp;kind=photo".to_string(),
        None => String::new(),
    };
    if let Some(w) = who {
        s += &format!("&amp;who={}", urlencode(w));
    }
    s
}

/// Terms that narrow the results down (REQ-SEARCH-003), each a link to
/// the query with it added.
pub fn render_narrow(q: &str, narrow: &[crate::search::Narrow], kq: &str) -> String {
    if narrow.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "<nav class=\"narrow\" aria-label=\"Narrow down\"><span class=\"dim\">Narrow down:</span>",
    );
    for n in narrow {
        let icon = match n.kind {
            crate::search::Kind::Thing => "◎",
            crate::search::Kind::Place => "⌖",
            crate::search::Kind::Date => "◷",
            crate::search::Kind::Word => "❝",
        };
        s += &format!(
            "<a href=\"search?q={q}{kq}\"><span aria-hidden=\"true\">{icon}</span> {term} <span class=\"n\">{c}</span></a>",
            q = urlencode(&format!("{} {}", q.trim(), crate::search::plain(&n.term))),
            term = esc(&n.term),
            c = n.items,
        );
    }
    s + "</nav>"
}

/// Pictures that look like what was searched (REQ-SEE-002): tiles, best
/// first.
pub fn render_looks(who: &str, q: &str, looks: &[(u64, index::Record)]) -> String {
    if looks.is_empty() {
        return String::new();
    }
    let mut s = format!(
        "<section class=\"looks\"><h2 class=\"rhead\">Pictures that look like \u{201c}{}\u{201d}</h2><div class=\"grid\">",
        esc(q)
    );
    for (id, rec) in looks {
        s += &format!(
            "<a class=\"item\" href=\"v/{who}/{id}\" title=\"{t}\"><img src=\"thumb/{id}?h={h}\" loading=\"lazy\" alt=\"\"></a>",
            who = esc(who),
            t = esc(&rec.taken_local.replace('T', " ")),
            h = esc(rec.hash.get(..12).unwrap_or("")),
        );
    }
    s += "</div></section>";
    s
}

/// The results page: each item as a tile with its date, place and the
/// transcript lines that matched (a time opens the video there).
pub fn render_search(
    who: &str,
    q: &str,
    hits: &[(crate::search::Hit, index::Record)],
    places: Option<&crate::places::Places>,
    pos: &HashMap<u64, crate::place::Position>,
) -> String {
    let who = esc(who);
    let mut s = format!(
        "<section class=\"results\"><h2 class=\"rhead\">{n} {what} for \u{201c}{q}\u{201d}</h2>",
        n = hits.len(),
        what = if hits.len() == 1 { "result" } else { "results" },
        q = esc(q)
    );
    if q.trim().is_empty() {
        return "<section class=\"results\"><p class=\"dim\">Search what was said in videos, the text in pictures, things (Katze, Fahrrad), places, dates (2021, Juli, July) and file names, or describe a picture in your own words (\u{201c}Kinder am Strand\u{201d}).</p></section>".into();
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
        // Recorded, set by hand or estimated (REQ-PLACE-002).
        let place = match (places, pos.get(&id)) {
            (Some(p), Some(at)) => p
                .nearest(at.lat, at.lon)
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

/// Items one request hides or shows again at most.
const MANY: usize = 2000;

#[derive(Deserialize)]
pub struct Many {
    /// Item ids, separated by commas.
    ids: String,
    back: Option<String>,
}

/// Hide (or show again) the listed items the viewer may see, for this
/// person (REQ-WEB-014); others are left alone. Answers with how many
/// changed, or goes back when a `back` is given (a form without
/// JavaScript).
async fn many(
    st: AppState,
    viewer: Viewer,
    headers: axum::http::HeaderMap,
    m: Many,
    set: fn(&index::Index, &str, u64) -> anyhow::Result<()>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let ids: Vec<u64> = m
        .ids
        .split(',')
        .filter_map(|i| i.trim().parse().ok())
        .take(MANY)
        .collect();
    let idx = st.index.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let mut n = 0;
        for id in ids {
            if idx.locate(id)?.is_some_and(|r| viewer.may_see(&r.label)) {
                set(&idx, &me, id)?;
                n += 1;
            }
        }
        Ok(n)
    })
    .await;
    match res {
        Ok(Ok(_)) if m.back.is_some() => back_to(&st, m.back.as_deref()),
        Ok(Ok(n)) => n.to_string().into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /hide-many: hide the selected items for this person.
async fn hide_many(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(m): axum::Form<Many>,
) -> Response {
    many(st, viewer, headers, m, |idx, me, id| {
        idx.set_hidden(me, id, true)
    })
    .await
}

/// POST /unhide-many: show them again (the undo after hiding).
async fn unhide_many(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(m): axum::Form<Many>,
) -> Response {
    many(st, viewer, headers, m, |idx, me, id| {
        idx.set_hidden(me, id, false)
    })
    .await
}

/// POST /fav/{id}: the item becomes this person's favourite (REQ-WEB-016).
async fn fav(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(b): axum::Form<Back>,
) -> Response {
    choice(st, viewer, id, headers, b.back, |idx, _, _, me, id| {
        idx.set_fav(me, id, true)
    })
    .await
}

/// POST /unfav/{id}: not a favourite any more.
async fn unfav(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(id): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(b): axum::Form<Back>,
) -> Response {
    choice(st, viewer, id, headers, b.back, |idx, _, _, me, id| {
        idx.set_fav(me, id, false)
    })
    .await
}

/// POST /fav-many: the selected items become favourites.
async fn fav_many(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(m): axum::Form<Many>,
) -> Response {
    many(st, viewer, headers, m, |idx, me, id| {
        idx.set_fav(me, id, true)
    })
    .await
}

/// POST /unfav-many: the selected items are favourites no more.
async fn unfav_many(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(m): axum::Form<Many>,
) -> Response {
    many(st, viewer, headers, m, |idx, me, id| {
        idx.set_fav(me, id, false)
    })
    .await
}

/// Pictures a favourites or album page shows at most, newest first.
const SET_MAX: usize = 5000;

/// The count in a favourites or album header: all of them, or which part
/// a capped page shows.
fn set_count(total: usize) -> String {
    if total > SET_MAX {
        format!("newest {SET_MAX} of {total}")
    } else {
        total.to_string()
    }
}

/// An album name as given: trimmed, spaces folded, at most 80 characters;
/// None when nothing is left.
fn album_name(s: &str) -> Option<String> {
    let n: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();
    (!n.is_empty()).then_some(n)
}

/// The album if it is this person's own (REQ-WEB-017); anyone else's
/// album does not exist for them.
fn own_album(idx: &index::Index, id: u64, me: &str) -> anyhow::Result<Option<index::Album>> {
    Ok(idx.album(id)?.filter(|a| a.owner == me))
}

/// The album if this person may look at it: their own, or one its owner
/// shares with the household. Changing it still needs `own_album`.
fn seen_album(idx: &index::Index, id: u64, me: &str) -> anyhow::Result<Option<index::Album>> {
    Ok(idx.album(id)?.filter(|a| a.owner == me || a.shared))
}

/// The timeline to link pictures into: the viewer's one library, or all.
fn default_who(viewer: &Viewer) -> String {
    if viewer.labels.len() == 1 {
        viewer.labels[0].clone()
    } else {
        "all".to_string()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `<option>`s for this person's albums, to add pictures to one.
pub fn album_options(albums: &[(u64, index::Album)]) -> String {
    albums
        .iter()
        .map(|(id, a)| format!("<option value=\"{id}\">{}</option>", esc(&a.name)))
        .collect()
}

/// This person's albums for the selection bar (assets/lichtbild.js).
fn album_template(albums: &[(u64, index::Album)]) -> String {
    format!(
        "<template id=\"albumopts\">{}</template>",
        album_options(albums)
    )
}

/// This person's albums, or none when they cannot be read.
async fn albums_of(st: &AppState, me: String) -> Vec<(u64, index::Album)> {
    let idx = st.index.clone();
    tokio::task::spawn_blocking(move || idx.albums(&me))
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default()
}

/// GET /albums: this person's albums, newest first, each with its cover
/// and count, and a field to make a new one.
async fn albums_page(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let idx = st.index.clone();
    let me = person(&viewer);
    let me2 = me.clone();
    let labels = viewer.labels.clone();
    type Row = (u64, index::Album, usize, Option<(u64, String)>);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Row>> {
        let hidden = idx.hidden(&me)?;
        let mut out = Vec::new();
        let mut all = idx.albums(&me)?;
        all.extend(idx.shared_albums(&me)?);
        for (id, a) in all {
            let mut shown: Vec<(String, u64, String)> = Vec::new();
            for i in idx.album_items(id)? {
                if hidden.contains(&i) {
                    continue;
                }
                if let Some(r) = idx.locate(i)?.filter(|r| labels.contains(&r.label)) {
                    if r.thumb == "ok" {
                        shown.push((r.taken_local.clone(), i, r.hash.clone()));
                    } else {
                        shown.push((String::new(), i, String::new()));
                    }
                }
            }
            let n = shown.len();
            shown.sort();
            let cover = shown
                .into_iter()
                .rev()
                .find(|(_, _, h)| !h.is_empty())
                .map(|(_, i, h)| (i, h));
            out.push((id, a, n, cover));
        }
        Ok(out)
    })
    .await;
    let rows = match res {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut body = String::from(
        "<h1>Albums</h1>\
         <form class=\"newalbum\" method=\"post\" action=\"albums\"><input type=\"text\" name=\"name\" maxlength=\"80\" required placeholder=\"New album\" aria-label=\"Name of a new album\"><button type=\"submit\">Make</button></form>",
    );
    let (mine, theirs): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| r.1.owner == me2);
    if mine.is_empty() {
        body += "<p class=\"dim\">No albums yet. Make one here, or choose pictures in a timeline with Select and add them to a new album.</p>";
    }
    for (head, rows) in [("", &mine), ("<h2>Shared with you</h2>", &theirs)] {
        if rows.is_empty() {
            continue;
        }
        body += head;
        body += "<section class=\"thingtiles albums\">";
        for (id, a, n, cover) in rows.iter() {
            let img = match cover {
                Some((i, h)) => format!(
                    "<img src=\"thumb/{i}?h={}\" loading=\"lazy\" alt=\"\">",
                    esc(h.get(..12).unwrap_or(""))
                ),
                None => "<span class=\"nothumb\">empty</span>".to_string(),
            };
            let by = if a.owner == me2 {
                if a.shared {
                    "<small>shared</small>".to_string()
                } else {
                    String::new()
                }
            } else {
                format!("<small>from {}</small>", esc(&a.owner))
            };
            body += &format!(
                "<a class=\"thing\" href=\"album/{id}\">{img}<b>{}</b><i>{n}</i>{by}</a>",
                esc(&a.name)
            );
        }
        body += "</section>";
    }
    page_for(&st, "lichtbild albums", &body).into_response()
}

#[derive(Deserialize)]
pub struct AlbumForm {
    /// An album id, or "new" (with `name`).
    album: Option<String>,
    name: Option<String>,
    /// Item ids, separated by commas.
    ids: Option<String>,
    back: Option<String>,
}

/// POST /albums: a new album, then to it.
async fn album_create(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<AlbumForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = f.name.as_deref().and_then(album_name) else {
        return (StatusCode::BAD_REQUEST, "an album needs a name").into_response();
    };
    let idx = st.index.clone();
    let me = person(&viewer);
    match tokio::task::spawn_blocking(move || idx.create_album(&me, &name, now_secs())).await {
        Ok(Ok(id)) => back_to(&st, Some(&format!("album/{id}"))),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /album-add: put the listed items the viewer may see into one of
/// their albums, or into a new one ("new" and a name). Answers with the
/// album's id, or goes back when a `back` is given (a form without
/// JavaScript).
async fn album_add(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<AlbumForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let ids: Vec<u64> = f
        .ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|i| i.trim().parse().ok())
        .take(MANY)
        .collect();
    let target = match f.album.as_deref() {
        Some("new") => match f.name.as_deref().and_then(album_name) {
            Some(n) => Err(n),
            None => return (StatusCode::BAD_REQUEST, "an album needs a name").into_response(),
        },
        Some(a) => match a.parse::<u64>() {
            Ok(id) => Ok(id),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let idx = st.index.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<u64>> {
        let album = match target {
            Ok(id) => match own_album(&idx, id, &me)? {
                Some(_) => id,
                None => return Ok(None),
            },
            Err(name) => idx.create_album(&me, &name, now_secs())?,
        };
        for id in ids {
            if idx.locate(id)?.is_some_and(|r| viewer.may_see(&r.label)) {
                idx.set_in_album(album, id, true)?;
            }
        }
        Ok(Some(album))
    })
    .await;
    match res {
        Ok(Ok(Some(_))) if f.back.is_some() => back_to(&st, f.back.as_deref()),
        Ok(Ok(Some(a))) => a.to_string().into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /album/{id}/remove: take the listed items out of the album.
async fn album_remove(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(album): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<AlbumForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let ids: Vec<u64> = f
        .ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|i| i.trim().parse().ok())
        .take(MANY)
        .collect();
    let idx = st.index.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<usize>> {
        if own_album(&idx, album, &me)?.is_none() {
            return Ok(None);
        }
        let held = idx.album_items(album)?;
        let mut n = 0;
        for id in ids.into_iter().filter(|i| held.contains(i)) {
            idx.set_in_album(album, id, false)?;
            n += 1;
        }
        Ok(Some(n))
    })
    .await;
    match res {
        Ok(Ok(Some(_))) if f.back.is_some() => back_to(&st, f.back.as_deref()),
        Ok(Ok(Some(n))) => n.to_string().into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /album/{id}/rename.
async fn album_rename(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(album): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<AlbumForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = f.name.as_deref().and_then(album_name) else {
        return (StatusCode::BAD_REQUEST, "an album needs a name").into_response();
    };
    let idx = st.index.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        if own_album(&idx, album, &me)?.is_none() {
            return Ok(false);
        }
        idx.rename_album(album, &name)?;
        Ok(true)
    })
    .await;
    match res {
        Ok(Ok(true)) => back_to(&st, Some(&format!("album/{album}"))),
        Ok(Ok(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /album/{id}/share: `name=on` shares the album with the
/// household, anything else stops sharing it. Only the owner.
async fn album_share(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(album): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<AlbumForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let on = f.name.as_deref() == Some("on");
    let idx = st.index.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        if own_album(&idx, album, &me)?.is_none() {
            return Ok(false);
        }
        idx.share_album(album, on)?;
        Ok(true)
    })
    .await;
    match res {
        Ok(Ok(true)) => back_to(&st, Some(&format!("album/{album}"))),
        Ok(Ok(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /album/{id}/delete: the album goes; its pictures stay.
async fn album_delete(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(album): UrlPath<u64>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let idx = st.index.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        if own_album(&idx, album, &me)?.is_none() {
            return Ok(false);
        }
        idx.delete_album(album)?;
        Ok(true)
    })
    .await;
    match res {
        Ok(Ok(true)) => back_to(&st, Some("albums")),
        Ok(Ok(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// GET /album/{id}: the album's pictures the viewer may see, newest
/// first, without the hidden ones, with its slideshow, renaming and
/// deleting.
async fn album_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(album): UrlPath<u64>,
) -> Response {
    let who = default_who(&viewer);
    let Some(labels) = scope(&viewer, &who) else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    let idx = st.index.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Option<(index::Album, Vec<index::Item>, usize)>> {
            let Some(a) = seen_album(&idx, album, &me)? else {
                return Ok(None);
            };
            let held = idx.album_items(album)?;
            if held.is_empty() {
                return Ok(Some((a, Vec::new(), 0)));
            }
            let hidden = idx.hidden(&me)?;
            let favs = idx.favs(&me)?;
            let mut items =
                idx.items_of(held.iter().copied(), &labels, |id, _| !hidden.contains(&id))?;
            let total = items.len();
            items.truncate(SET_MAX);
            for it in &mut items {
                it.copies = dups.shown(it.id, &labels, &hidden).copies;
                it.fav = favs.contains(&it.id);
            }
            Ok(Some((a, items, total)))
        },
    )
    .await;
    let (a, items, total) = match res {
        Ok(Ok(Some(x))) => x,
        Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "no such album").into_response(),
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let ones = vec![1; items.len()];
    let w = esc(&who);
    let name = esc(&a.name);
    let owner = a.owner == person(&viewer);
    // Sharing is the owner's choice, off until turned on (REQ-WEB-017).
    let share = if a.shared {
        format!("<form method=\"post\" action=\"album/{album}/share\"><input type=\"hidden\" name=\"name\" value=\"off\"><button type=\"submit\">Stop sharing</button> <span class=\"dim\">Everyone in the house sees this album, each only the pictures they may see.</span></form>")
    } else {
        format!("<form method=\"post\" action=\"album/{album}/share\"><input type=\"hidden\" name=\"name\" value=\"on\"><button type=\"submit\">Share with the house</button> <span class=\"dim\">Only you see it now. Others could look, not change it.</span></form>")
    };
    let manage = format!(
        "<details class=\"albumtools\"><summary>Share, rename or delete</summary>\
         {share}<form method=\"post\" action=\"album/{album}/rename\"><input type=\"text\" name=\"name\" value=\"{name}\" maxlength=\"80\" required aria-label=\"Album name\"><button type=\"submit\">Rename</button></form>\
         <form method=\"post\" action=\"album/{album}/delete\"><button type=\"submit\" class=\"danger\">Delete this album</button> <span class=\"dim\">The pictures stay where they are.</span></form></details>"
    );
    let mark = if a.shared { " · shared" } else { "" };
    // Someone else's shared album: to look at, with its owner named; only
    // the owner shares, renames, deletes or takes pictures out.
    let (manage, from, data) = if owner {
        (manage, String::new(), format!(" data-album=\"{album}\""))
    } else {
        (
            String::new(),
            format!("<p class=\"dim\">Shared by {}.</p>", esc(&a.owner)),
            String::new(),
        )
    };
    let body = if items.is_empty() {
        format!(
            "<h1>{name}</h1>{from}<p class=\"dim\">Empty. Choose pictures in a timeline with Select and add them here, or use “Add to album” in a photo’s details.</p>{manage}"
        )
    } else {
        format!(
            "<div class=\"thead\"><h1>{name}</h1><span class=\"dim\">{n}{mark}</span><a class=\"tbtn\" href=\"show/{w}?album={album}\" title=\"Slideshow\">▶ Slideshow</a><button type=\"button\" class=\"tbtn select\" hidden aria-pressed=\"false\">Select</button></div>\n{from}{manage}<div class=\"timeline album\"{data}>\n{}</div>\n{albums}",
            render_items(&who, &items, &ones, None, false, ""),
            n = set_count(total),
            albums = album_template(&albums_of(&st, person(&viewer)).await),
        )
    };
    page_for(&st, "lichtbild album", &body).into_response()
}

/// GET /favourites: this person's favourites in the libraries they may
/// see, newest first, as a timeline (REQ-WEB-016).
async fn favourites_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Response {
    let who = if viewer.labels.len() == 1 {
        viewer.labels[0].clone()
    } else {
        "all".to_string()
    };
    let Some(labels) = scope(&viewer, &who) else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    let idx = st.index.clone();
    let dups = st.dups.clone();
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<index::Item>, usize)> {
        let favs = idx.favs(&me)?;
        if favs.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let hidden = idx.hidden(&me)?;
        let mut items =
            idx.items_of(favs.iter().copied(), &labels, |id, _| !hidden.contains(&id))?;
        let total = items.len();
        items.truncate(SET_MAX);
        for it in &mut items {
            it.copies = dups.shown(it.id, &labels, &hidden).copies;
            it.fav = true;
        }
        Ok((items, total))
    })
    .await;
    let (items, total) = match res {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    // Each favourite is its own choice: no series folding here.
    let ones = vec![1; items.len()];
    let body = if items.is_empty() {
        "<h1>Favourites</h1><p class=\"dim\">None yet. Tap ♡ on a photo, press <kbd>.</kbd>, or choose several in a timeline with Select.</p>".to_string()
    } else {
        format!(
            "<div class=\"thead\"><h1>Favourites</h1><span class=\"dim\">{n}</span><a class=\"tbtn\" href=\"show/{w}?fav=1\" title=\"Slideshow\">▶ Slideshow</a><button type=\"button\" class=\"tbtn select\" hidden aria-pressed=\"false\">Select</button></div>\n<div class=\"timeline favs\">\n{}</div>\n{albums}",
            render_items(&who, &items, &ones, None, false, ""),
            n = set_count(total),
            w = esc(&who),
            albums = album_template(&albums_of(&st, person(&viewer)).await),
        )
    };
    page_for(&st, "lichtbild favourites", &body).into_response()
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
#[derive(Deserialize, Default)]
pub struct HiddenQuery {
    /// "new" (default), "old" or "name".
    sort: Option<String>,
    /// Keep only items whose file name, folder or date holds this.
    q: Option<String>,
}

/// GET /hidden: what this person hid (REQ-WEB-020), newest first, oldest
/// first or by file name, narrowed by a word in the name, folder or date;
/// several are chosen and shown again at once.
async fn hidden_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    Query(hq): Query<HiddenQuery>,
) -> Response {
    let idx = st.index.clone();
    let me = person(&viewer);
    let labels = viewer.labels.clone();
    // File names only from the viewer's own library, as on the duplicates
    // page; a granted library's items show their date alone.
    let own = owned(st.auth.is_some(), &viewer);
    let sort = match hq.sort.as_deref() {
        Some("old") => "old",
        Some("name") => "name",
        _ => "new",
    };
    let word = hq.q.as_deref().unwrap_or("").trim().to_lowercase();
    let word2 = word.clone();
    type Hidden = (usize, Vec<(u64, index::Record, String)>);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Hidden> {
        let mut out = Vec::new();
        let mut all = 0;
        for id in idx.hidden(&me)? {
            let Some(r) = idx.locate(id)?.filter(|r| labels.contains(&r.label)) else {
                continue;
            };
            all += 1;
            let path = if own.contains(&r.label) {
                String::from_utf8_lossy(&r.rel).into_owned()
            } else {
                String::new()
            };
            if !word2.is_empty()
                && !path.to_lowercase().contains(&word2)
                && !r.taken_local.contains(&word2)
            {
                continue;
            }
            out.push((id, r, path));
        }
        let name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
        match sort {
            "old" => out.sort_by(|a, b| (&a.1.taken_local, a.0).cmp(&(&b.1.taken_local, b.0))),
            "name" => out.sort_by_key(|a| (name(&a.2), a.0)),
            _ => out.sort_by(|a, b| (&b.1.taken_local, b.0).cmp(&(&a.1.taken_local, a.0))),
        }
        Ok((all, out))
    })
    .await;
    let (all, items) = match res {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let back = format!(
        "hidden?sort={sort}{}",
        if word.is_empty() {
            String::new()
        } else {
            format!("&q={}", urlencode(&word))
        }
    );
    let opt = |v: &str, label: &str| {
        format!(
            "<option value=\"{v}\"{}>{label}</option>",
            if v == sort { " selected" } else { "" }
        )
    };
    let shown = if word.is_empty() {
        String::new()
    } else {
        format!(" {} with “{}”.", items.len(), esc(&word))
    };
    let mut body = format!(
        "<h1>Hidden</h1><p class=\"dim\">{all} hidden by you; only your view changes, nothing in the library.{shown}</p>\
         <form class=\"hidtools\" method=\"get\" action=\"hidden\"><input type=\"search\" name=\"q\" value=\"{q}\" placeholder=\"Word in the file name or date\" aria-label=\"Word in the file name or date\">\
         <select name=\"sort\" aria-label=\"Order\">{}{}{}</select><button type=\"submit\">Show</button></form>\
         <form class=\"hidmany\" method=\"post\" action=\"unhide-many\"><input type=\"hidden\" name=\"ids\" value=\"\"><input type=\"hidden\" name=\"back\" value=\"{b}\">\
         <button type=\"button\" class=\"all\">Choose all</button><button type=\"submit\" class=\"again\" disabled>Show the chosen again</button><span class=\"dim n\"></span></form>\
         <div class=\"grid hiddenlist\">",
        opt("new", "Newest first"),
        opt("old", "Oldest first"),
        opt("name", "By name"),
        q = esc(&word),
        b = esc(&back),
    );
    for (id, r, path) in &items {
        let img = if r.thumb == "ok" {
            format!(
                "<img src=\"thumb/{id}?h={}\" loading=\"lazy\" alt=\"\">",
                esc(r.hash.get(..12).unwrap_or(""))
            )
        } else {
            "<span class=\"nothumb\">no preview</span>".to_string()
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        body += &format!(
            "<div class=\"hid\"><a class=\"item\" href=\"v/{l}/{id}\">{img}</a>\
             <label class=\"pick\"><input type=\"checkbox\" value=\"{id}\"> <span>{when}</span></label>\
             <small class=\"dim fname\" title=\"{p}\">{n}</small>\
             <form method=\"post\" action=\"unhide/{id}\"><input type=\"hidden\" name=\"back\" value=\"{b}\"><button type=\"submit\">Show again</button></form></div>",
            l = esc(&r.label),
            when = esc(r.taken_local.get(..10).unwrap_or("")),
            p = esc(path),
            n = esc(name),
            b = esc(&back),
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
    let read_counts = if st.vision_on {
        let (idx, labels, version) = (
            st.index.clone(),
            viewer.labels.clone(),
            st.vision_version.clone(),
        );
        match tokio::task::spawn_blocking(move || idx.look_counts(&labels, &version)).await {
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
        Phase::Looking {
            since,
            done,
            total,
            report,
        } => (
            format!(
                "Looking at photos: {done} of {total} done ({} with text, {} with things, {} failed), since {}",
                report.with_text,
                report.with_things,
                report.failed,
                relative(*since, now)
            ),
            "busy",
        ),
        Phase::Faces {
            since,
            done,
            total,
            report,
        } => (
            if everything {
                format!(
                    "Looking for faces: {done} of {total} photos done ({} faces, {} failed), since {}",
                    report.faces,
                    report.failed,
                    relative(*since, now)
                )
            } else {
                format!("Looking for faces, since {}", relative(*since, now))
            },
            "busy",
        ),
        Phase::Idle { next: Some(n) } => (format!("Idle; next scan {}", relative(*n, now)), "ok"),
        Phase::Idle { next: None } => ("Idle".to_string(), "ok"),
    };
    let progress = match &s.phase {
        Phase::Transcribing { done, total, .. }
        | Phase::Looking { done, total, .. }
        | Phase::Faces { done, total, .. }
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
        "Photos looked at",
        match read_counts {
            Some((photos, done, with, things)) => format!(
                "{done} of {photos} photos looked at: {with} with text, {things} with things <span class=\"dim\">({})</span>{}",
                esc(&st.vision_version),
                match s.last_read {
                    Some(t) => format!(
                        "<br><span class=\"dim\">last pass ended {}</span>",
                        relative(t, now)
                    ),
                    None => String::new(),
                }
            ),
            None => "off (no vision models configured)".into(),
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

/// GET /things: the kinds of thing found in the viewer's photos, each with
/// how many photos show it and the photo with the surest find; a tile
/// searches for it (REQ-SEE-001).
async fn things_page(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let (idx, dups, labels, me) = (
        st.index.clone(),
        st.dups.clone(),
        viewer.labels.clone(),
        person(&viewer),
    );
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let hidden = idx.hidden(&me)?;
        let things = idx.things(&labels, |id| dups.shown(id, &labels, &hidden).shown)?;
        let mut out = Vec::new();
        for (kind, n, id) in things {
            if let Some(rec) = idx.locate(id)? {
                out.push((kind, n, id, rec));
            }
        }
        Ok(out)
    })
    .await;
    let things = match res {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut body = String::from(
        "<h1>Things</h1><p class=\"dim\">What is in your photos, found in the house. A thing shows the photos with it.</p>",
    );
    if things.is_empty() {
        body += if st.vision_on {
            "<p>Nothing found yet. Photos are looked at in the background after each scan.</p>"
        } else {
            "<p>Off: the operator sets <code>LICHTBILD_VISION_DIR</code> to the vision models.</p>"
        };
        return page_for(&st, "lichtbild things", &body).into_response();
    }
    body += "<section class=\"thingtiles\">";
    for (kind, n, id, rec) in &things {
        let (_, one, many) = crate::objects::KINDS[*kind as usize];
        body += &format!(
            "<a class=\"thing\" href=\"search?q={q}\"><img src=\"thumb/{id}?h={h}\" loading=\"lazy\" alt=\"\"><span><b>{name}</b><i>{n}</i></span></a>",
            q = esc(&urlencode(one)),
            h = esc(rec.hash.get(..12).unwrap_or("")),
            name = esc(many),
        );
    }
    body += "</section><p class=\"dim\">Something found that is not there? Say so in the photo, under ◎ “Something wrong?”; <a href=\"corrections\">corrections</a> hold for the house.</p>";
    page_for(&st, "lichtbild things", &body).into_response()
}

/// "3–9 July 2021", "30 June – 2 July 2021", "30 December 2021 – 2 January
/// 2022".
fn day_span(a: chrono::NaiveDate, b: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    let m = |d: chrono::NaiveDate| MONTHS[d.month0() as usize];
    if a == b {
        format!("{} {} {}", a.day(), m(a), a.year())
    } else if a.year() != b.year() {
        format!(
            "{} {} {} – {} {} {}",
            a.day(),
            m(a),
            a.year(),
            b.day(),
            m(b),
            b.year()
        )
    } else if a.month() != b.month() {
        format!("{} {} – {} {} {}", a.day(), m(a), b.day(), m(b), b.year())
    } else {
        format!("{}–{} {} {}", a.day(), b.day(), m(b), b.year())
    }
}

/// GET /trips: days away from home, grouped and named by place
/// (REQ-TRIP-001); a trip opens the timeline at its last day.
async fn trips_page(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let (idx, dups, labels, me) = (
        st.index.clone(),
        st.dups.clone(),
        viewer.labels.clone(),
        person(&viewer),
    );
    let (pc, places) = (st.positions.clone(), st.places.clone());
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let hidden = idx.hidden(&me)?;
        let items: Vec<(u64, index::Record)> = idx
            .all()?
            .into_iter()
            .filter(|(id, r)| {
                !r.missing && labels.contains(&r.label) && dups.shown(*id, &labels, &hidden).shown
            })
            .collect();
        let pos = pc.get(&idx)?;
        let trips = crate::trips::find(&items, &pos, places.as_deref());
        let hashes: HashMap<u64, String> = items.into_iter().map(|(id, r)| (id, r.hash)).collect();
        Ok(trips
            .into_iter()
            .map(|t| {
                let h = hashes.get(&t.cover).cloned().unwrap_or_default();
                (t, h)
            })
            .collect::<Vec<_>>())
    })
    .await;
    let trips = match res {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let who = if viewer.labels.len() == 1 {
        viewer.labels[0].clone()
    } else {
        "all".to_string()
    };
    let mut body = String::from(
        "<h1>Trips</h1><p class=\"dim\">Days on which your pictures were taken more than 50 km from home, one after another. Home is where most days of pictures were taken. A trip opens its pictures alone, with a slideshow.</p>",
    );
    if trips.is_empty() {
        body += "<p>No trips found yet. Trips come from the positions of pictures: recorded, estimated from neighbours or set by hand.</p>";
        return page_for(&st, "lichtbild trips", &body).into_response();
    }
    body += "<section class=\"thingtiles trips\">";
    for (t, hash) in &trips {
        let title = match t.places.as_slice() {
            [] => "Away".to_string(),
            [one] => one.clone(),
            [one, two] => format!("{one} and {two}"),
            [one, rest @ ..] => format!("{one} and {} more places", rest.len()),
        };
        body += &format!(
            "<a class=\"thing\" href=\"days/{w}?from={start}&amp;to={end}&amp;title={tq}\"><img src=\"thumb/{id}?h={h}\" loading=\"lazy\" alt=\"\"><span><b>{title}</b><i>{n}</i></span><small>{when}</small></a>",
            w = esc(&who),
            start = t.start.format("%Y-%m-%d"),
            end = t.end.format("%Y-%m-%d"),
            tq = urlencode(&title),
            id = t.cover,
            h = esc(hash.get(..12).unwrap_or("")),
            title = esc(&title),
            n = t.items,
            when = esc(&day_span(t.start, t.end)),
        );
    }
    body += "</section>";
    page_for(&st, "lichtbild trips", &body).into_response()
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

// ---- Days (a trip, a day, or one day in every year) ----

#[derive(Deserialize, Default, Clone)]
pub struct DaysQuery {
    /// First and last day, YYYY-MM-DD (both included).
    from: Option<String>,
    to: Option<String>,
    /// One day of the year in every year, MM-DD.
    md: Option<String>,
    /// A heading for the page (a trip's places).
    title: Option<String>,
    /// One day, YYYY-MM-DD, from the date picker: the same as from=to=day.
    day: Option<String>,
}

fn is_day(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn is_month_day(s: &str) -> bool {
    s.len() == 5
        && s.bytes().enumerate().all(|(i, b)| {
            if i == 2 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

impl DaysQuery {
    /// Whether a picture taken at `taken` (local "YYYY-MM-DDTHH...") is in.
    fn holds(&self, taken: &str) -> bool {
        let day = taken.get(..10).unwrap_or("");
        match (&self.md, &self.from, &self.to) {
            (Some(md), _, _) => day.get(5..10) == Some(md.as_str()),
            (None, Some(f), Some(t)) => day >= f.as_str() && day <= t.as_str(),
            _ => false,
        }
    }

    fn valid(&self) -> bool {
        match (&self.md, &self.from, &self.to) {
            (Some(md), _, _) => is_month_day(md),
            (None, Some(f), Some(t)) => is_day(f) && is_day(t) && f <= t,
            _ => false,
        }
    }

    /// The query again, for links (slideshow).
    fn query(&self) -> String {
        match (&self.md, &self.from, &self.to) {
            (Some(md), _, _) => format!("md={}", urlencode(md)),
            (None, Some(f), Some(t)) => format!("from={}&amp;to={}", urlencode(f), urlencode(t)),
            _ => String::new(),
        }
    }

    fn heading(&self) -> String {
        let day = |d: &str| {
            long_date(&format!("{d}T00:00"))
                .trim_end_matches(", 00:00")
                .to_string()
        };
        if let Some(t) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return t.chars().take(120).collect();
        }
        match (&self.md, &self.from, &self.to) {
            (Some(md), _, _) => {
                let m: usize = md.get(..2).and_then(|m| m.parse().ok()).unwrap_or(0);
                let d: u32 = md.get(3..).and_then(|d| d.parse().ok()).unwrap_or(0);
                format!(
                    "{d} {} in every year",
                    MONTHS.get(m.wrapping_sub(1)).copied().unwrap_or("")
                )
            }
            (None, Some(f), Some(t)) if f == t => day(f),
            (None, Some(f), Some(t)) => format!("{} – {}", day(f), day(t)),
            _ => String::new(),
        }
    }
}

/// The pictures of a day range the viewer may see, one copy each, nothing
/// hidden, newest first.
fn day_items(
    idx: &index::Index,
    dups: &crate::dups::Dups,
    labels: &[String],
    hidden: &std::collections::HashSet<u64>,
    dq: &DaysQuery,
    photos_only: bool,
) -> anyhow::Result<Vec<index::Item>> {
    let before = match (&dq.md, &dq.to) {
        (None, Some(t)) => Some(format!("{t}T99")),
        _ => None,
    };
    let mut items = idx.timeline_where(
        labels,
        before.as_deref().map(|b| (b, 0)),
        usize::MAX,
        |id, rec| {
            dq.holds(&rec.taken_local)
                && (!photos_only || rec.video.is_none())
                && dups.shown(id, labels, hidden).shown
        },
    )?;
    items.truncate(SET_MAX);
    Ok(items)
}

/// GET /days/{who}?from=&to= (a trip, a day) or ?md= (this day in every
/// year): just those pictures, as a timeline, with a slideshow.
async fn days_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(who): UrlPath<String>,
    Query(mut dq): Query<DaysQuery>,
) -> Response {
    let Some(labels) = scope(&viewer, &who) else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    if let Some(d) = dq.day.take() {
        dq.from = Some(d.clone());
        dq.to = Some(d);
        dq.md = None;
    }
    if !dq.valid() {
        return (StatusCode::BAD_REQUEST, "which days?").into_response();
    }
    let (idx, dups, me) = (st.index.clone(), st.dups.clone(), person(&viewer));
    let space = st.vision_on.then(|| st.space.clone());
    let (best, dd) = (st.best.clone(), st.data_dir.clone());
    let dq2 = dq.clone();
    type Days = (Vec<index::Item>, Vec<usize>, Option<String>, Option<String>);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Days> {
        let hidden = idx.hidden(&me)?;
        let favs = idx.favs(&me)?;
        let mut items = day_items(&idx, &dups, &labels, &hidden, &dq2, false)?;
        for it in &mut items {
            it.copies = dups.shown(it.id, &labels, &hidden).copies;
            it.fav = favs.contains(&it.id);
        }
        let runs = series(&items, space.as_deref());
        crate::best::best_first(&mut items, &runs, |it| best.of(&dd, it));
        // The days before and after with a picture the viewer would see here.
        let (mut before, mut after) = (None, None);
        if let (None, Some(f), Some(t)) = (&dq2.md, &dq2.from, &dq2.to) {
            let shown = |id: u64, _: &index::Record| dups.shown(id, &labels, &hidden).shown;
            before = idx
                .timeline_where(&labels, Some((f.as_str(), 0)), 1, shown)?
                .pop()
                .and_then(|it| it.taken_local.get(..10).map(str::to_string));
            after = idx
                .first_from(&labels, &format!("{t}T99"), shown)?
                .and_then(|it| it.taken_local.get(..10).map(str::to_string));
        }
        Ok((items, runs, before, after))
    })
    .await;
    let (items, runs, before, after) = match res {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let head = esc(&dq.heading());
    let w = esc(&who);
    let nav = day_nav(&w, &dq, before.as_deref(), after.as_deref());
    let body = if items.is_empty() {
        format!("<h1>{head}</h1>{nav}<p class=\"dim\">No pictures from these days.</p>")
    } else {
        format!(
            "<div class=\"thead\"><h1>{head}</h1><span class=\"dim\">{n}</span><a class=\"tbtn\" href=\"show/{w}?{q}\" title=\"Slideshow\">▶ Slideshow</a><button type=\"button\" class=\"tbtn select\" hidden aria-pressed=\"false\">Select</button></div>\n{nav}<div class=\"timeline days\">\n{}</div>\n{nav}{albums}",
            render_items(&who, &items, &runs, None, false, ""),
            n = set_count(items.len()),
            q = dq.query(),
            albums = album_template(&albums_of(&st, person(&viewer)).await),
        )
    };
    page_for(&st, "lichtbild days", &body).into_response()
}

/// Earlier and later day with pictures, and a date picker for any day
/// (REQ-WEB-019). `before` and `after` are YYYY-MM-DD.
fn day_nav(w: &str, dq: &DaysQuery, before: Option<&str>, after: Option<&str>) -> String {
    let day = |d: &str| {
        long_date(&format!("{d}T00:00"))
            .trim_end_matches(", 00:00")
            .to_string()
    };
    let link = |d: Option<&str>, rel: &str, arrow_first: bool| match d {
        Some(d) => {
            let (d, label) = (esc(d), esc(&day(d)));
            let text = if arrow_first {
                format!("‹ {label}")
            } else {
                format!("{label} ›")
            };
            format!("<a rel=\"{rel}\" href=\"days/{w}?from={d}&amp;to={d}\">{text}</a>")
        }
        None => "<span></span>".to_string(),
    };
    let value = dq.from.as_deref().filter(|_| dq.md.is_none()).unwrap_or("");
    format!(
        "<nav class=\"daynav\">{}<form class=\"daypick\" method=\"get\" action=\"days/{w}\"><label>Day <input type=\"date\" name=\"day\" value=\"{}\" required></label><button type=\"submit\">Go</button></form>{}</nav>\n",
        link(before, "prev", true),
        esc(value),
        link(after, "next", false),
    )
}

// ---- People (REQ-FACE-002, REQ-FACE-003, DD-025) ----

/// A face the viewer may see, with one item holding its photo.
struct SeenFace {
    face: crate::people::FaceInfo,
    item: u64,
}

/// The faces whose photo is in a library the viewer may see and not hidden
/// by them; a photo in several places is held by its lowest item id.
fn seen_faces(
    idx: &index::Index,
    people: &crate::people::People,
    labels: &[String],
    hidden: &std::collections::HashSet<u64>,
) -> anyhow::Result<Vec<SeenFace>> {
    let mut at: HashMap<String, u64> = HashMap::new();
    for (id, r) in idx.all()? {
        if r.missing || r.video.is_some() || !labels.contains(&r.label) || hidden.contains(&id) {
            continue;
        }
        at.entry(r.hash)
            .and_modify(|e| *e = (*e).min(id))
            .or_insert(id);
    }
    Ok(people
        .all()
        .into_iter()
        .filter_map(|f| at.get(&f.key.0).map(|&item| SeenFace { face: f, item }))
        .collect())
}

/// The face with number `k` in item `item`, if the viewer may see the item.
fn face_at(
    idx: &index::Index,
    people: &crate::people::People,
    viewer: &Viewer,
    item: u64,
    k: u32,
) -> anyhow::Result<Option<(index::Record, crate::people::FaceInfo)>> {
    let Some(rec) = idx.locate(item)?.filter(|r| viewer.may_see(&r.label)) else {
        return Ok(None);
    };
    let f = people
        .in_photo(&rec.hash)
        .into_iter()
        .find(|f| f.key.1 == k);
    Ok(f.map(|f| (rec, f)))
}

/// A person's or group's tile: the best face to show (one someone named,
/// then the largest).
fn cover(faces: &[&SeenFace]) -> Option<(u64, u32)> {
    faces
        .iter()
        .max_by(|a, b| {
            (a.face.named, a.face.b[2] * a.face.b[3])
                .partial_cmp(&(b.face.named, b.face.b[2] * b.face.b[3]))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|f| (f.item, f.face.key.1))
}

fn face_img(item: u64, k: u32) -> String {
    format!("<img class=\"face\" src=\"face/{item}/{k}\" loading=\"lazy\" alt=\"\" width=\"80\" height=\"80\">")
}

/// Photos with this many faces of one unnamed group or more are offered
/// for naming.
const GROUP_MIN: usize = 2;
/// At most this many unnamed groups on the people page.
const GROUPS_MAX: usize = 48;

/// GET /people: the named people the viewer has photos of, then groups of
/// one unnamed person each, biggest first, to be named.
async fn people_page(State(st): State<AppState>, Extension(viewer): Extension<Viewer>) -> Response {
    let (idx, people) = (st.index.clone(), st.people.clone());
    let me = person(&viewer);
    let labels = viewer.labels.clone();
    type Tile = (String, String, usize, Option<(u64, u32)>);
    let res =
        tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<Tile>, Vec<Tile>, usize)> {
            let hidden = idx.hidden(&me)?;
            let seen = seen_faces(&idx, &people, &labels, &hidden)?;
            let names: HashMap<u64, String> = idx
                .people()?
                .into_iter()
                .map(|(id, p)| (id, p.name))
                .collect();
            let mut by: HashMap<crate::people::Who, Vec<&SeenFace>> = HashMap::new();
            for f in &seen {
                by.entry(f.face.who).or_default().push(f);
            }
            let photos = |v: &[&SeenFace]| {
                v.iter()
                    .map(|f| f.face.key.0.as_str())
                    .collect::<std::collections::HashSet<_>>()
                    .len()
            };
            let (mut named, mut groups, mut singles) = (Vec::new(), Vec::new(), 0);
            for (who, v) in &by {
                match who {
                    crate::people::Who::Person(p) => {
                        let Some(name) = names.get(p) else { continue };
                        named.push((format!("person/{p}"), name.clone(), photos(v), cover(v)));
                    }
                    crate::people::Who::Group(_) => {
                        let n = photos(v);
                        if n < GROUP_MIN {
                            singles += 1;
                            continue;
                        }
                        let c = cover(v);
                        let href = c
                            .map(|(i, k)| format!("people/{i}/{k}"))
                            .unwrap_or_default();
                        groups.push((href, String::new(), n, c));
                    }
                }
            }
            named.sort_by(|a, b| {
                b.2.cmp(&a.2)
                    .then(a.1.to_lowercase().cmp(&b.1.to_lowercase()))
            });
            groups.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
            groups.truncate(GROUPS_MAX);
            Ok((named, groups, singles))
        })
        .await;
    let (named, groups, singles) = match res {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut body = String::from("<h1>People</h1>");
    if named.is_empty() && groups.is_empty() {
        body += if st.people.is_empty() {
            "<p class=\"dim\">No faces yet. With the face models in the vision directory, lichtbild looks for faces after it has looked at the photos.</p>"
        } else {
            "<p class=\"dim\">No one twice yet in the photos you see.</p>"
        };
    }
    let tiles = |rows: &[Tile], label: &str| -> String {
        let mut o = String::from("<section class=\"thingtiles people\">");
        for (href, name, n, c) in rows {
            let img = c.map(|(i, k)| face_img(i, k)).unwrap_or_default();
            let name = if name.is_empty() {
                format!("<b class=\"dim\">{label}</b>")
            } else {
                format!("<b>{}</b>", esc(name))
            };
            o += &format!(
                "<a class=\"thing person\" href=\"{href}\">{img}<span>{name}<i>{n}</i></span></a>"
            );
        }
        o + "</section>"
    };
    if !named.is_empty() {
        body += &tiles(&named, "");
    }
    if !groups.is_empty() {
        body += "<h2>Who is this?</h2><p class=\"dim\">The same face in several photos. Give it a name, and lichtbild finds that person in the others too. Names are the same for everyone in the house.</p>";
        body += &tiles(&groups, "Name?");
    }
    if singles > 0 {
        body += &format!("<p class=\"dim\">{singles} more faces were seen in one photo each.</p>");
    }
    page_for(&st, "lichtbild people", &body).into_response()
}

/// The photos holding faces of `who` that the viewer may see, as items.
fn photos_of(
    idx: &index::Index,
    people: &crate::people::People,
    labels: &[String],
    me: &str,
    who: crate::people::Who,
) -> anyhow::Result<(Vec<index::Item>, usize)> {
    let hidden = idx.hidden(me)?;
    let favs = idx.favs(me)?;
    let seen = seen_faces(idx, people, labels, &hidden)?;
    let ids: std::collections::BTreeSet<u64> = seen
        .iter()
        .filter(|f| f.face.who == who)
        .map(|f| f.item)
        .collect();
    let mut items = idx.items_of(ids, labels, |id, _| !hidden.contains(&id))?;
    let total = items.len();
    items.truncate(SET_MAX);
    for it in &mut items {
        it.fav = favs.contains(&it.id);
    }
    Ok((items, total))
}

/// GET /person/{id}: the photos of a named person the viewer may see.
async fn person_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(pid): UrlPath<u64>,
) -> Response {
    let who = default_who(&viewer);
    let labels = viewer.labels.clone();
    let (idx, people) = (st.index.clone(), st.people.clone());
    let me = person(&viewer);
    let res = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Option<(index::Person, Vec<index::Item>, usize)>> {
            let Some(p) = idx.person(pid)? else {
                return Ok(None);
            };
            let (items, total) =
                photos_of(&idx, &people, &labels, &me, crate::people::Who::Person(pid))?;
            // A name with no photo the viewer may see does not exist for them.
            if items.is_empty() {
                return Ok(None);
            }
            Ok(Some((p, items, total)))
        },
    )
    .await;
    let (p, items, total) = match res {
        Ok(Ok(Some(x))) => x,
        Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "no such person").into_response(),
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let ones = vec![1; items.len()];
    let name = esc(&p.name);
    let w = esc(&who);
    let manage = format!(
        "<details class=\"albumtools\"><summary>Rename or take the name off</summary>\
         <form method=\"post\" action=\"person/{pid}/rename\"><input type=\"text\" name=\"name\" value=\"{name}\" maxlength=\"80\" required aria-label=\"Name\"><button type=\"submit\">Rename</button> <span class=\"dim\">The name of someone already named puts both together.</span></form>\
         <form method=\"post\" action=\"person/{pid}/delete\"><button type=\"submit\" class=\"danger\">Take this name off</button> <span class=\"dim\">The photos stay; the faces wait for a name again.</span></form></details>"
    );
    let body = format!(
        "<div class=\"thead\"><h1>{name}</h1><span class=\"dim\">{n}</span><a class=\"tbtn\" href=\"show/{w}?person={pid}\" title=\"Slideshow\">▶ Slideshow</a><button type=\"button\" class=\"tbtn select\" hidden aria-pressed=\"false\">Select</button></div>\n{manage}<div class=\"timeline person\">\n{}</div>\n{albums}",
        render_items(&who, &items, &ones, None, false, ""),
        n = set_count(total),
        albums = album_template(&albums_of(&st, person(&viewer)).await),
    );
    page_for(&st, "lichtbild person", &body).into_response()
}

/// The people the viewer has a photo of (not hidden): the only names that
/// exist for them (REQ-FACE-003).
fn seen_people(
    idx: &index::Index,
    people: &crate::people::People,
    viewer: &Viewer,
) -> anyhow::Result<std::collections::HashSet<u64>> {
    let hidden = idx.hidden(&person(viewer))?;
    Ok(seen_faces(idx, people, &viewer.labels, &hidden)?
        .into_iter()
        .filter_map(|f| match f.face.who {
            crate::people::Who::Person(p) => Some(p),
            crate::people::Who::Group(_) => None,
        })
        .collect())
}

/// The names the viewer knows, for a name field's suggestions.
fn names_list(idx: &index::Index, seen: &std::collections::HashSet<u64>) -> String {
    let mut names: Vec<String> = idx
        .people()
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, _)| seen.contains(id))
        .map(|(_, p)| p.name)
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    let opts: String = names
        .iter()
        .map(|n| format!("<option value=\"{}\">", esc(n)))
        .collect();
    format!("<datalist id=\"names\">{opts}</datalist>")
}

/// GET /people/{item}/{face}: the unnamed group this face belongs to, to
/// be named; a named face's person page instead.
async fn group_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath((item, k)): UrlPath<(u64, u32)>,
) -> Response {
    let who = default_who(&viewer);
    let labels = viewer.labels.clone();
    let (idx, people) = (st.index.clone(), st.people.clone());
    let me = person(&viewer);
    let v2 = viewer.clone();
    type Group = (crate::people::Who, Vec<index::Item>, usize, String);
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Group>> {
        let Some((_, f)) = face_at(&idx, &people, &v2, item, k)? else {
            return Ok(None);
        };
        if let crate::people::Who::Person(_) = f.who {
            return Ok(Some((f.who, Vec::new(), 0, String::new())));
        }
        let (items, total) = photos_of(&idx, &people, &labels, &me, f.who)?;
        let seen = seen_people(&idx, &people, &v2)?;
        Ok(Some((f.who, items, total, names_list(&idx, &seen))))
    })
    .await;
    let (g, items, total, names) = match res {
        Ok(Ok(Some(x))) => x,
        Ok(Ok(None)) => return (StatusCode::NOT_FOUND, "no such face").into_response(),
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    if let crate::people::Who::Person(p) = g {
        return Redirect::to(&format!("{}/person/{p}", st.base_path)).into_response();
    }
    let ones = vec![1; items.len()];
    let body = format!(
        "<div class=\"thead\">{img}<h1>Who is this?</h1><span class=\"dim\">{n}</span></div>\
         <form class=\"namef\" method=\"post\" action=\"people/name\"><input type=\"hidden\" name=\"item\" value=\"{item}\"><input type=\"hidden\" name=\"face\" value=\"{k}\">\
         <input type=\"text\" name=\"name\" list=\"names\" maxlength=\"80\" required autocomplete=\"off\" placeholder=\"Name\" aria-label=\"Name of this person\"><button type=\"submit\">Name</button>{names}</form>\
         <p class=\"dim\">The name goes to every photo here, and to this person in other photos as lichtbild finds them. Everyone in the house sees the name.</p>\
         <div class=\"timeline group\">\n{}</div>",
        render_items(&who, &items, &ones, None, false, ""),
        img = face_img(item, k),
        n = set_count(total),
    );
    page_for(&st, "lichtbild who is this", &body).into_response()
}

#[derive(Deserialize)]
pub struct NameForm {
    item: u64,
    face: u32,
    name: Option<String>,
    /// Name only this face, not its whole group.
    one: Option<String>,
    /// For "not them": the person.
    person: Option<u64>,
    back: Option<String>,
}

/// The person with this name (any case) among those the viewer knows, or
/// a new one: a name only someone else has photos of is not found, so
/// naming tells nothing about it (REQ-FACE-003).
fn person_named(
    idx: &index::Index,
    name: &str,
    by: &str,
    seen: &std::collections::HashSet<u64>,
) -> anyhow::Result<u64> {
    let low = name.to_lowercase();
    if let Some((id, _)) = idx
        .people()?
        .into_iter()
        .find(|(id, p)| seen.contains(id) && p.name.to_lowercase() == low)
    {
        return Ok(id);
    }
    idx.create_person(name, by, now_secs())
}

/// POST /people/name: give a name to a face and, unless `one` is set, to
/// every face of its group the viewer may see.
async fn people_name(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<NameForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = f.name.as_deref().and_then(album_name) else {
        return (StatusCode::BAD_REQUEST, "a name is needed").into_response();
    };
    let (idx, people) = (st.index.clone(), st.people.clone());
    let me = person(&viewer);
    let labels = viewer.labels.clone();
    let one = f.one.is_some();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<u64>> {
        let Some((_, face)) = face_at(&idx, &people, &viewer, f.item, f.face)? else {
            return Ok(None);
        };
        let keys: Vec<crate::people::Key> = if one {
            vec![face.key.clone()]
        } else {
            let hidden = idx.hidden(&me)?;
            seen_faces(&idx, &people, &labels, &hidden)?
                .into_iter()
                .filter(|s| s.face.who == face.who)
                .map(|s| s.face.key)
                .collect()
        };
        let seen = seen_people(&idx, &people, &viewer)?;
        let pid = person_named(&idx, &name, &me, &seen)?;
        idx.name_faces(&keys, pid)?;
        people.relabel(&idx)?;
        Ok(Some(pid))
    })
    .await;
    match res {
        Ok(Ok(Some(pid))) => {
            let back = f.back.clone().unwrap_or_else(|| format!("person/{pid}"));
            back_to(&st, Some(&back))
        }
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /people/not: this face is not `person`.
async fn people_not(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<NameForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(pid) = f.person else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (idx, people) = (st.index.clone(), st.people.clone());
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let Some((_, face)) = face_at(&idx, &people, &viewer, f.item, f.face)? else {
            return Ok(false);
        };
        idx.not_person(&face.key.0, face.key.1, pid)?;
        people.relabel(&idx)?;
        Ok(true)
    })
    .await;
    match res {
        Ok(Ok(true)) => back_to(&st, f.back.as_deref()),
        Ok(Ok(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// Whether the viewer has a photo of this person: only then may they see
/// or change the name.
fn sees_person(
    idx: &index::Index,
    people: &crate::people::People,
    viewer: &Viewer,
    pid: u64,
) -> anyhow::Result<bool> {
    let hidden = idx.hidden(&person(viewer))?;
    Ok(seen_faces(idx, people, &viewer.labels, &hidden)?
        .iter()
        .any(|f| f.face.who == crate::people::Who::Person(pid)))
}

/// POST /person/{id}/rename: a new name; someone else's name puts both
/// together.
async fn person_rename(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(pid): UrlPath<u64>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<AlbumForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = f.name.as_deref().and_then(album_name) else {
        return (StatusCode::BAD_REQUEST, "a name is needed").into_response();
    };
    let (idx, people) = (st.index.clone(), st.people.clone());
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<u64>> {
        if idx.person(pid)?.is_none() || !sees_person(&idx, &people, &viewer, pid)? {
            return Ok(None);
        }
        // Only someone the viewer knows is put together with this one.
        let seen = seen_people(&idx, &people, &viewer)?;
        let low = name.to_lowercase();
        let other = idx
            .people()?
            .into_iter()
            .find(|(id, p)| *id != pid && seen.contains(id) && p.name.to_lowercase() == low);
        let to = match other {
            Some((into, _)) => {
                idx.merge_people(pid, into)?;
                into
            }
            None => {
                idx.rename_person(pid, &name)?;
                pid
            }
        };
        people.relabel(&idx)?;
        Ok(Some(to))
    })
    .await;
    match res {
        Ok(Ok(Some(to))) => back_to(&st, Some(&format!("person/{to}"))),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /person/{id}/delete: take the name off; the faces are unnamed again.
async fn person_delete(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath(pid): UrlPath<u64>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (idx, people) = (st.index.clone(), st.people.clone());
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        if idx.person(pid)?.is_none() || !sees_person(&idx, &people, &viewer, pid)? {
            return Ok(false);
        }
        idx.delete_person(pid)?;
        people.relabel(&idx)?;
        Ok(true)
    })
    .await;
    match res {
        Ok(Ok(true)) => back_to(&st, Some("people")),
        Ok(Ok(false)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// GET /face/{item}/{face}: the small picture of a face.
async fn face_crop(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    UrlPath((item, k)): UrlPath<(u64, u32)>,
) -> Response {
    let dd = st.data_dir.clone();
    let idx = st.index.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        let Some(rec) = idx.locate(item)?.filter(|r| viewer.may_see(&r.label)) else {
            return Ok(None);
        };
        Ok(std::fs::read(crate::faces::crop_path(&dd, &rec.hash, k)).ok())
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

/// The faces of a photo for the viewer: each small, with its name (a link)
/// or a field to name it, and "not them" for a name lichtbild passed along.
fn render_faces(
    idx: &index::Index,
    people: &crate::people::People,
    viewer: &Viewer,
    id: u64,
    hash: &str,
    back: &str,
) -> String {
    let faces = people.in_photo(hash);
    if faces.is_empty() {
        return String::new();
    }
    let names: HashMap<u64, String> = idx
        .people()
        .unwrap_or_default()
        .into_iter()
        .map(|(i, p)| (i, p.name))
        .collect();
    let b = esc(back);
    let mut o = format!(
        "<details class=\"faces\"><summary title=\"People in the picture\" aria-label=\"People in the picture\">☺ {}</summary><div class=\"kpanel\"><ul class=\"facelist\">",
        faces.len()
    );
    for f in &faces {
        let k = f.key.1;
        let img = face_img(id, k);
        let row = match f.who {
            crate::people::Who::Person(p) if names.contains_key(&p) => {
                let not = format!(
                    "<form method=\"post\" action=\"people/not\"><input type=\"hidden\" name=\"item\" value=\"{id}\"><input type=\"hidden\" name=\"face\" value=\"{k}\"><input type=\"hidden\" name=\"person\" value=\"{p}\"><input type=\"hidden\" name=\"back\" value=\"{b}\"><button type=\"submit\" class=\"not\" title=\"This is someone else\">Not {n}</button></form>",
                    n = esc(&names[&p])
                );
                format!(
                    "<a href=\"person/{p}\">{}</a>{}{not}",
                    esc(&names[&p]),
                    if f.named { "" } else { " <small class=\"dim\">found</small>" }
                )
            }
            _ => format!(
                "<form class=\"namef\" method=\"post\" action=\"people/name\"><input type=\"hidden\" name=\"item\" value=\"{id}\"><input type=\"hidden\" name=\"face\" value=\"{k}\"><input type=\"hidden\" name=\"one\" value=\"1\"><input type=\"hidden\" name=\"back\" value=\"{b}\"><input type=\"text\" name=\"name\" list=\"names\" maxlength=\"80\" required autocomplete=\"off\" placeholder=\"Who is this?\" aria-label=\"Name of this person\"><button type=\"submit\">Name</button></form>"
            ),
        };
        o += &format!("<li>{img}<span>{row}</span></li>");
    }
    let seen = seen_people(idx, people, viewer).unwrap_or_default();
    o + "</ul>" + &names_list(idx, &seen) + "</div></details>"
}

// ---- Corrections (REQ-FIX-001, REQ-FIX-002) ----

/// The kinds of thing to pick from, German and English.
fn kinds_datalist() -> String {
    let mut s = String::from("<datalist id=\"kinds\">");
    for (en, de, _) in crate::objects::KINDS {
        s += &format!(
            "<option value=\"{}\"><option value=\"{}\">",
            esc(de),
            esc(en)
        );
    }
    s + "</datalist>"
}

/// The kind a name stands for: German singular or plural, or English.
fn kind_of(name: &str) -> Option<u8> {
    let n = crate::search::fold(name.trim());
    crate::objects::KINDS
        .iter()
        .position(|(en, de, pl)| [en, de, pl].iter().any(|w| crate::search::fold(w) == n))
        .map(|k| k as u8)
}

/// The "Something wrong?" part of the things panel: each find with what
/// the house said about it, and buttons to say it is right, not there or
/// another kind.
fn render_thing_fixes(
    id: u64,
    back: &str,
    hash: &str,
    raw: &[crate::objects::Object],
    fixes: &crate::fixes::Fixes,
) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "<details class=\"fix\"><summary>Something wrong?</summary><p class=\"dim\">What you say here holds for everyone in the house, and the next look at the photos keeps it. <a href=\"corrections\">All corrections</a></p><ul class=\"fixlist\">",
    );
    for (n, o) in raw.iter().enumerate() {
        let about = fixes.about(hash, o);
        let state = match about {
            Some(f) if f.right() => "marked right".to_string(),
            Some(f) => match f.now {
                None => "marked not there".to_string(),
                Some(k) => format!("is a {}", crate::fixes::kind_name(k)),
            },
            None if fixes.off.contains_key(&o.kind) => "never in our house".to_string(),
            None if o.score < fixes.bar(o.kind) => {
                "left out: too unsure, after earlier corrections".to_string()
            }
            None => String::new(),
        };
        let (en, _, many) = crate::objects::KINDS[o.kind as usize];
        s += &format!(
            "<li data-n=\"{n}\"><b>{name}</b> <span class=\"dim\">{pct:.0} %</span> <i>{state}</i>\
             <form method=\"post\" action=\"fix/thing\"><input type=\"hidden\" name=\"item\" value=\"{id}\"><input type=\"hidden\" name=\"n\" value=\"{n}\"><input type=\"hidden\" name=\"back\" value=\"{b}\">\
             <button type=\"submit\" name=\"now\" value=\"right\" title=\"It is there\" aria-label=\"It is there\">✓</button>\
             <button type=\"submit\" name=\"now\" value=\"wrong\" title=\"It is not there\" aria-label=\"It is not there\">✕</button>\
             <input name=\"kind\" list=\"kinds\" placeholder=\"It is a…\" aria-label=\"It is a…\" size=\"9\"><button type=\"submit\" name=\"now\" value=\"kind\">Change</button>{undo}</form>{off}</li>",
            name = esc(o.name()),
            pct = o.score * 100.0,
            state = esc(&state),
            b = esc(back),
            undo = if about.is_some() {
                "<button type=\"submit\" name=\"now\" value=\"undo\">Undo</button>"
            } else {
                ""
            },
            off = if about.is_some_and(|f| !f.right()) && !fixes.off.contains_key(&o.kind) {
                format!(
                    "<form method=\"post\" action=\"fix/off\"><input type=\"hidden\" name=\"kind\" value=\"{k}\"><input type=\"hidden\" name=\"off\" value=\"1\"><input type=\"hidden\" name=\"back\" value=\"{b}\"><button type=\"submit\" title=\"Leave out every {en} found, in every photo\">No {many} in our house</button></form>",
                    k = o.kind,
                    b = esc(back),
                    en = esc(en),
                    many = esc(many),
                )
            } else {
                String::new()
            },
        );
    }
    s += "</ul>";
    s += &kinds_datalist();
    s += "</details>";
    s
}

/// The "Something wrong?" part of the text panel: each line read, to
/// correct or take out.
fn render_text_fixes(
    id: u64,
    back: &str,
    raw: &crate::ocr::Text,
    fixes: &[crate::fixes::TextFix],
) -> String {
    if raw.state != "ok" || raw.lines.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "<details class=\"fix\"><summary>Something wrong?</summary><p class=\"dim\">Correct a line or take it out, for everyone in the house; the next reading keeps it.</p><ul class=\"fixlist\">",
    );
    for (n, l) in raw.lines.iter().enumerate() {
        let about = crate::fixes::text_about(fixes, crate::fixes::quad_box(&l.quad), &l.text);
        let (value, state) = match about {
            Some(f) => match &f.now {
                Some(t) => (t.as_str(), format!("read “{}”", l.text)),
                None => (l.text.as_str(), "taken out".to_string()),
            },
            None => (l.text.as_str(), String::new()),
        };
        s += &format!(
            "<li><form method=\"post\" action=\"fix/text\"><input type=\"hidden\" name=\"item\" value=\"{id}\"><input type=\"hidden\" name=\"n\" value=\"{n}\"><input type=\"hidden\" name=\"back\" value=\"{b}\">\
             <input name=\"text\" value=\"{v}\" aria-label=\"Line {line}\" maxlength=\"500\"><button type=\"submit\" name=\"now\" value=\"save\">Save</button>\
             <button type=\"submit\" name=\"now\" value=\"wrong\" title=\"There is no such text\" aria-label=\"There is no such text\">✕</button>{undo}</form> <i class=\"dim\">{state}</i></li>",
            b = esc(back),
            v = esc(value),
            line = n + 1,
            state = esc(&state),
            undo = if about.is_some() {
                "<button type=\"submit\" name=\"now\" value=\"undo\">Undo</button>"
            } else {
                ""
            },
        );
    }
    s + "</ul></details>"
}

#[derive(Deserialize)]
pub struct FixForm {
    item: u64,
    /// The find or line, by its place in what the model returned.
    n: usize,
    /// "right", "wrong", "kind", "save" or "undo".
    now: String,
    kind: Option<String>,
    text: Option<String>,
    back: Option<String>,
}

/// The photo `item` if the viewer may see it.
fn fix_photo(
    idx: &index::Index,
    viewer: &Viewer,
    item: u64,
) -> anyhow::Result<Option<index::Record>> {
    Ok(idx
        .locate(item)?
        .filter(|r| viewer.may_see(&r.label) && r.video.is_none()))
}

/// POST /fix/thing: a thing found is right, not there, another kind, or
/// the word about it is taken back.
async fn fix_thing(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<FixForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let idx = st.index.clone();
    let me = person(&viewer);
    let back = f.back.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<StatusCode> {
        let Some(rec) = fix_photo(&idx, &viewer, f.item)? else {
            return Ok(StatusCode::NOT_FOUND);
        };
        let raw = idx.raw_objects(&rec.hash)?;
        let Some(o) = raw.get(f.n) else {
            return Ok(StatusCode::NOT_FOUND);
        };
        let now = match f.now.as_str() {
            "right" => Some(o.kind),
            "wrong" => None,
            "kind" => match f.kind.as_deref().and_then(kind_of) {
                Some(k) => Some(k),
                None => return Ok(StatusCode::BAD_REQUEST),
            },
            "undo" => {
                let fixes = idx.fixes()?;
                let list = fixes.things.get(&rec.hash).cloned().unwrap_or_default();
                let at =
                    crate::fixes::about(&list, o).and_then(|a| list.iter().position(|x| x == a));
                idx.fix_thing(&rec.hash, None, at)?;
                return Ok(StatusCode::OK);
            }
            _ => return Ok(StatusCode::BAD_REQUEST),
        };
        let fix = crate::fixes::ThingFix {
            kind: o.kind,
            bbox: o.bbox,
            score: o.score,
            now,
            by: me,
            at: now_secs(),
        };
        idx.fix_thing(&rec.hash, Some(fix), None)?;
        Ok(StatusCode::OK)
    })
    .await;
    fixed(&st, res, back.as_deref())
}

/// After a correction: search learns it in the background, then back.
fn fixed(
    st: &AppState,
    res: Result<anyhow::Result<StatusCode>, tokio::task::JoinError>,
    back: Option<&str>,
) -> Response {
    match res {
        Ok(Ok(StatusCode::OK)) => {
            rebuild_search(st);
            back_to(st, back)
        }
        Ok(Ok(code)) => code.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// POST /fix/text: a line read is corrected, taken out, or the
/// correction is taken back.
async fn fix_text(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<FixForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let idx = st.index.clone();
    let me = person(&viewer);
    let back = f.back.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<StatusCode> {
        let Some(rec) = fix_photo(&idx, &viewer, f.item)? else {
            return Ok(StatusCode::NOT_FOUND);
        };
        let Some(l) = idx
            .raw_text(&rec.hash)?
            .and_then(|t| t.lines.into_iter().nth(f.n))
        else {
            return Ok(StatusCode::NOT_FOUND);
        };
        let bbox = crate::fixes::quad_box(&l.quad);
        let now = match f.now.as_str() {
            "wrong" => None,
            "save" => {
                let t = f.text.as_deref().unwrap_or("").trim();
                if t.is_empty() || t.chars().count() > 500 {
                    return Ok(StatusCode::BAD_REQUEST);
                }
                if t == l.text {
                    // Unchanged: nothing to say, an earlier word goes.
                    let fixes = idx.text_fixes(&rec.hash)?;
                    let at = crate::fixes::text_about(&fixes, bbox, &l.text)
                        .and_then(|a| fixes.iter().position(|x| x == a));
                    idx.fix_text(&rec.hash, None, at)?;
                    return Ok(StatusCode::OK);
                }
                Some(t.to_string())
            }
            "undo" => {
                let fixes = idx.text_fixes(&rec.hash)?;
                let at = crate::fixes::text_about(&fixes, bbox, &l.text)
                    .and_then(|a| fixes.iter().position(|x| x == a));
                idx.fix_text(&rec.hash, None, at)?;
                return Ok(StatusCode::OK);
            }
            _ => return Ok(StatusCode::BAD_REQUEST),
        };
        let fix = crate::fixes::TextFix {
            bbox,
            was: l.text,
            now,
            by: me,
            at: now_secs(),
        };
        idx.fix_text(&rec.hash, Some(fix), None)?;
        Ok(StatusCode::OK)
    })
    .await;
    fixed(&st, res, back.as_deref())
}

#[derive(Deserialize)]
pub struct OffForm {
    /// A kind's number, or its name typed.
    kind: String,
    /// "1": never in the house; anything else: it may be again.
    off: Option<String>,
    back: Option<String>,
}

/// POST /fix/off: a kind is never in the house, or may be again.
async fn fix_off(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
    headers: axum::http::HeaderMap,
    axum::Form(f): axum::Form<OffForm>,
) -> Response {
    if !same_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let kind = match f.kind.parse::<u8>() {
        Ok(k) if (k as usize) < crate::objects::KINDS.len() => Some(k),
        Ok(_) => None,
        Err(_) => kind_of(&f.kind),
    };
    let Some(kind) = kind else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let idx = st.index.clone();
    let mark = (f.off.as_deref() == Some("1")).then(|| crate::fixes::Mark {
        by: person(&viewer),
        at: now_secs(),
    });
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<StatusCode> {
        idx.set_kind_off(kind, mark.as_ref())?;
        Ok(StatusCode::OK)
    })
    .await;
    fixed(&st, res, f.back.as_deref())
}

/// The photos the viewer may see and did not hide, by content: hash ->
/// (the first such item, its library).
fn seen_photos(
    idx: &index::Index,
    viewer: &Viewer,
) -> anyhow::Result<HashMap<String, (u64, String)>> {
    let hidden = idx.hidden(&person(viewer))?;
    let mut at: HashMap<String, (u64, String)> = HashMap::new();
    for (id, r) in idx.all()? {
        if r.missing || r.video.is_some() || !viewer.may_see(&r.label) || hidden.contains(&id) {
            continue;
        }
        match at.get_mut(&r.hash) {
            Some(e) if e.0 <= id => {}
            Some(e) => *e = (id, r.label),
            None => {
                at.insert(r.hash, (id, r.label));
            }
        }
    }
    Ok(at)
}

/// GET /corrections: what the house corrected in the photos the viewer
/// sees, the kinds never in the house and the bars learned.
async fn corrections_page(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Response {
    let idx = st.index.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let seen = seen_photos(&idx, &viewer)?;
        let fixes = idx.fixes()?;
        let mut body = String::from(
            "<h1>Corrections</h1><p class=\"dim\">What the house said the looking got wrong, in your photos. Corrections hold for everyone in the house and stay when the photos are looked at again. Make them under “Something wrong?” in a photo’s ◎ and Aa panels. <a href=\"corrections.jsonl\" download>Download them all</a> (no pictures, only which find was wrong), to teach a model one day.</p>",
        );
        // Kinds never in the house.
        body += "<h2>Never in our house</h2>";
        let mut off: Vec<(&u8, &crate::fixes::Mark)> = fixes.off.iter().collect();
        off.sort_by_key(|(k, _)| crate::fixes::kind_name(**k));
        if off.is_empty() {
            body += "<p class=\"dim\">Nothing yet.</p>";
        } else {
            body += "<ul class=\"fixlist\">";
            for (k, _) in off {
                body += &format!(
                    "<li><b>{}</b><form method=\"post\" action=\"fix/off\"><input type=\"hidden\" name=\"kind\" value=\"{k}\"><input type=\"hidden\" name=\"back\" value=\"corrections\"><button type=\"submit\">Find them again</button></form></li>",
                    esc(crate::objects::KINDS[*k as usize].2),
                );
            }
            body += "</ul>";
        }
        body += "<form method=\"post\" action=\"fix/off\" class=\"fixadd\"><input type=\"hidden\" name=\"off\" value=\"1\"><input type=\"hidden\" name=\"back\" value=\"corrections\"><input name=\"kind\" list=\"kinds\" placeholder=\"A kind of thing\" aria-label=\"A kind of thing never in our house\" required><button type=\"submit\">Never in our house</button></form>";
        body += &kinds_datalist();
        // Bars learned.
        let mut bars: Vec<(u8, f32)> = (0..crate::objects::KINDS.len() as u8)
            .map(|k| (k, fixes.bar(k)))
            .filter(|(_, b)| *b > crate::objects::MIN_SCORE)
            .collect();
        if !bars.is_empty() {
            bars.sort_by_key(|(k, _)| crate::fixes::kind_name(*k));
            body += "<h2>Learned</h2><p class=\"dim\">Kinds marked wrong more than once are kept only when the looking is surer than it was then.</p><ul class=\"fixlist\">";
            for (k, b) in bars {
                body += &format!(
                    "<li><b>{}</b> from {:.0} % on</li>",
                    esc(crate::objects::KINDS[k as usize].2),
                    b * 100.0
                );
            }
            body += "</ul>";
        }
        // Things.
        let mut rows: Vec<(u64, String, String, String)> = Vec::new();
        for (hash, list) in &fixes.things {
            let Some((id, label)) = seen.get(hash) else {
                continue;
            };
            for f in list {
                let said = match f.now {
                    _ if f.right() => "right".to_string(),
                    None => "not there".to_string(),
                    Some(k) => format!("a {}", crate::fixes::kind_name(k)),
                };
                rows.push((*id, label.clone(), crate::fixes::kind_name(f.kind).to_string(), said));
            }
        }
        rows.sort();
        body += &format!("<h2>Things <span class=\"dim\">{}</span></h2>", rows.len());
        if rows.is_empty() {
            body += "<p class=\"dim\">Nothing corrected yet.</p>";
        } else {
            body += "<ul class=\"fixlist fixrows\">";
            for (id, label, was, now) in &rows {
                body += &format!(
                    "<li><a href=\"v/{l}/{id}\"><img src=\"thumb/{id}\" loading=\"lazy\" alt=\"\"></a> {was} → <b>{now}</b></li>",
                    l = esc(label),
                    was = esc(was),
                    now = esc(now),
                );
            }
            body += "</ul>";
        }
        // Text.
        let mut lines: Vec<(u64, String, String, Option<String>)> = Vec::new();
        for (hash, list) in idx.all_text_fixes()? {
            let Some((id, label)) = seen.get(&hash) else {
                continue;
            };
            for f in list {
                lines.push((*id, label.clone(), f.was, f.now));
            }
        }
        lines.sort();
        body += &format!("<h2>Text <span class=\"dim\">{}</span></h2>", lines.len());
        if lines.is_empty() {
            body += "<p class=\"dim\">Nothing corrected yet.</p>";
        } else {
            body += "<ul class=\"fixlist fixrows\">";
            for (id, label, was, now) in &lines {
                body += &format!(
                    "<li><a href=\"v/{l}/{id}\"><img src=\"thumb/{id}\" loading=\"lazy\" alt=\"\"></a> “{was}” → {now}</li>",
                    l = esc(label),
                    was = esc(was),
                    now = match now {
                        Some(t) => format!("<b>“{}”</b>", esc(t)),
                        None => "<b>no text</b>".into(),
                    },
                );
            }
            body += "</ul>";
        }
        Ok(body)
    })
    .await;
    match res {
        Ok(Ok(body)) => page_for(&st, "lichtbild corrections", &body).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// GET /corrections.jsonl: every correction in the photos the viewer sees,
/// a JSON object per line, to teach a model: which photo (by content
/// hash), where, what the model said and what is right. No pictures, and
/// not who said it.
async fn corrections_jsonl(
    State(st): State<AppState>,
    Extension(viewer): Extension<Viewer>,
) -> Response {
    let idx = st.index.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let seen = seen_photos(&idx, &viewer)?;
        let fixes = idx.fixes()?;
        let mut out = String::new();
        let mut line = |v: serde_json::Value| {
            out += &v.to_string();
            out.push('\n');
        };
        let en = |k: u8| crate::objects::KINDS.get(k as usize).map_or("?", |t| t.0);
        let mut things: Vec<_> = fixes
            .things
            .iter()
            .filter(|(h, _)| seen.contains_key(*h))
            .collect();
        things.sort_by_key(|(h, _)| *h);
        for (h, list) in things {
            for f in list {
                line(serde_json::json!({
                    "type": "thing", "photo": h, "box": f.bbox,
                    "model": en(f.kind), "score": f.score, "is": f.now.map(en),
                }));
            }
        }
        let mut off: Vec<u8> = fixes.off.keys().copied().collect();
        off.sort_unstable();
        for k in off {
            line(serde_json::json!({"type": "never", "kind": en(k)}));
        }
        let mut texts = idx.all_text_fixes()?;
        texts.retain(|(h, _)| seen.contains_key(h));
        texts.sort_by(|a, b| a.0.cmp(&b.0));
        for (h, list) in texts {
            for f in list {
                line(serde_json::json!({
                    "type": "text", "photo": h, "box": f.bbox, "read": f.was, "is": f.now,
                }));
            }
        }
        // Faces: named faces say who they are, "not them" who they are not
        // (people by the house's number, not by name).
        let mut named = idx.named_faces()?;
        named.retain(|((h, _), _)| seen.contains_key(h));
        named.sort();
        for ((h, n), p) in named {
            line(serde_json::json!({"type": "face", "photo": h, "face": n, "person": p}));
        }
        let mut not = idx.not_faces()?;
        not.retain(|(h, _, _)| seen.contains_key(h));
        not.sort();
        for (h, n, p) in not {
            line(serde_json::json!({"type": "face", "photo": h, "face": n, "not_person": p}));
        }
        Ok(out)
    })
    .await;
    match res {
        Ok(Ok(body)) => (
            [
                (header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"lichtbild-corrections.jsonl\"",
                ),
                (header::CACHE_CONTROL, "private, no-store"),
            ],
            body,
        )
            .into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
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
