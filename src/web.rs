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
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use crate::config::Config;
use crate::index::{self, Index};
use crate::media::DateSource;

const HTMX: &str = include_str!("../assets/htmx-2.0.4.min.js");
const CSS: &str = include_str!("../assets/lichtbild.css");
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
        }
    }
}

/// The full router, nested under the base path when one is set.
pub fn router(state: AppState) -> Router {
    let base = state.base_path.clone();
    let app: Router<AppState> = Router::new()
        .route("/", get(home))
        .route("/t/{who}", get(timeline))
        .route("/thumb/{id}", get(thumb))
        .route("/original/{id}", get(original))
        .route("/static/{file}", get(static_file))
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
    app.with_state(state)
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

fn page(base: &str, title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<base href="{base}/">
<title>{title}</title>
<link rel="stylesheet" href="static/lichtbild.css">
<script src="static/htmx-2.0.4.min.js" defer></script>
</head>
<body>
<header><a href="./" class="brand">lichtbild</a></header>
<main>
{body}
</main>
</body>
</html>
"#,
        base = esc(base),
        title = esc(title),
    ))
}

fn internal<E: std::fmt::Display>(e: E) -> Response {
    tracing::error!("request failed: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
}

async fn home(State(st): State<AppState>) -> Response {
    let idx = st.index.clone();
    let counts = match tokio::task::spawn_blocking(move || idx.counts()).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let mut body = String::from("<h1>Timelines</h1>\n<ul class=\"people\">\n");
    for l in st.labels.iter() {
        let n = counts.get(l).copied().unwrap_or(0);
        body += &format!(
            "<li><a href=\"t/{l}\">{l}</a> <span class=\"n\">{n}</span></li>\n",
            l = esc(l)
        );
    }
    body += "<li><a href=\"t/all\">merged</a></li>\n</ul>\n";
    page(&st.base_path, "lichtbild", &body).into_response()
}

#[derive(Deserialize)]
struct Cursor {
    before: Option<String>,
    id: Option<u64>,
    day: Option<String>,
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
                "<img src=\"thumb/{id}\" loading=\"lazy\" alt=\"\" width=\"{w}\" height=\"{h}\">",
                id = it.id,
                w = it.width.min(360),
                h = (u64::from(it.height) * u64::from(it.width.min(360))
                    / u64::from(it.width.max(1))),
            )
        } else {
            "<span class=\"nothumb\">no preview</span>".to_string()
        };
        s += &format!(
            "<a class=\"item{cls}\" href=\"original/{id}\" title=\"{t}\">{img}</a>",
            cls = if weak { " weak" } else { "" },
            id = it.id,
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
    UrlPath(who): UrlPath<String>,
    Query(cur): Query<Cursor>,
    headers: axum::http::HeaderMap,
) -> Response {
    let labels: Vec<String> = if who == "all" {
        st.labels.to_vec()
    } else if st.labels.contains(&who) {
        vec![who.clone()]
    } else {
        return (StatusCode::NOT_FOUND, "no such timeline").into_response();
    };
    let idx = st.index.clone();
    let before = cur.before.clone().zip(cur.id);
    let res = tokio::task::spawn_blocking(move || {
        idx.timeline(
            &labels,
            before.as_ref().map(|(t, i)| (t.as_str(), *i)),
            PAGE_SIZE as usize + 1,
        )
    })
    .await;
    let mut items = match res {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e),
    };
    let more = items.len() > PAGE_SIZE as usize;
    items.truncate(PAGE_SIZE as usize);
    let fragment = render_items(&who, &items, cur.day.as_deref(), more);
    if headers.contains_key("hx-request") {
        return Html(fragment).into_response();
    }
    let title = if who == "all" {
        "merged".to_string()
    } else {
        who.clone()
    };
    let body = format!(
        "<h1>{}</h1>\n<div class=\"timeline\">\n{}</div>\n{}",
        esc(&title),
        fragment,
        if items.is_empty() {
            "<p>No items yet. Run <code>lichtbild scan</code>.</p>"
        } else {
            ""
        }
    );
    page(&st.base_path, &title, &body).into_response()
}

async fn thumb(State(st): State<AppState>, UrlPath(id): UrlPath<u64>) -> Response {
    let dd = st.data_dir.clone();
    let idx = st.index.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        let Some(rec) = idx.locate(id)? else {
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
        _ => "application/octet-stream",
    }
}

async fn original(State(st): State<AppState>, UrlPath(id): UrlPath<u64>) -> Response {
    let idx = st.index.clone();
    let roots = st.roots.clone();
    let res = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Option<(std::fs::File, &'static str)>> {
            let Some(rec) = idx.locate(id)? else {
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
    match res {
        Ok(Ok(Some((f, ct)))) => {
            let stream = tokio_util_stream(f);
            (
                [
                    (header::CONTENT_TYPE, ct),
                    (header::CACHE_CONTROL, "private, max-age=3600"),
                ],
                stream,
            )
                .into_response()
        }
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

/// Stream a file in chunks without loading it whole.
fn tokio_util_stream(f: std::fs::File) -> Body {
    Body::from_stream(tokio_util::io::ReaderStream::new(
        tokio::fs::File::from_std(f),
    ))
}

async fn static_file(UrlPath(file): UrlPath<String>) -> Response {
    let (body, ct) = match file.as_str() {
        "htmx-2.0.4.min.js" => (HTMX, "text/javascript; charset=utf-8"),
        "lichtbild.css" => (CSS, "text/css; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
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
pub fn embedded_assets() -> [(&'static str, &'static str); 2] {
    [("htmx-2.0.4.min.js", HTMX), ("lichtbild.css", CSS)]
}
