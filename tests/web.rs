#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::*;
use std::sync::Arc;

use lichtbild::config::Config;
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

async fn get(
    app: &axum::Router,
    uri: &str,
    htmx: bool,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(uri);
    if htmx {
        req = req.header("hx-request", "true");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, body)
}

fn scanned() -> (Fixture, Config, Arc<Index>) {
    let fx = fixture();
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    assert!(
        scan::run(&cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    (fx, cfg, idx)
}

fn app(cfg: &Config, idx: &Arc<Index>) -> axum::Router {
    web::router(AppState::new(cfg, idx.clone()))
}

/// Absolute URLs or protocol-relative references to any host.
fn outside_urls(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for pat in ["http://", "https://", "//"] {
        let mut from = 0;
        while let Some(i) = text[from..].find(pat) {
            let at = from + i;
            let prev = text[..at].chars().last();
            // "//" only counts as protocol-relative inside an attribute or url().
            let counts = pat != "//" || matches!(prev, Some('"' | '\'' | '('));
            if counts {
                found.push(text[at..text.len().min(at + 60)].to_string());
            }
            from = at + pat.len();
        }
    }
    found
}

// rivet: verifies REQ-WEB-003
#[tokio::test]
async fn every_response_is_same_origin_only() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let mut pages = Vec::new();
    for uri in [
        "/",
        "/t/p1",
        "/t/p2",
        "/t/all",
        "/static/lichtbild.css",
        "/static/htmx-2.0.4.min.js",
        "/thumb/1",
        "/nope",
        "/t/nope",
        "/healthz",
    ] {
        let (status, h, body) = get(&app, uri, false).await;
        assert_eq!(
            h.get("content-security-policy")
                .map(|v| v.to_str().unwrap()),
            Some(web::CSP),
            "{uri} ({status}) has no CSP"
        );
        assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
        pages.push((uri, String::from_utf8_lossy(&body).into_owned()));
    }
    for (name, text) in web::embedded_assets() {
        pages.push((name, text.to_string()));
    }
    for (uri, text) in pages {
        let urls = outside_urls(&text);
        assert!(urls.is_empty(), "{uri} references {urls:?}");
    }
    // The CSP names no source other than 'self' and 'none'.
    for directive in web::CSP.split(';') {
        for src in directive.split_whitespace().skip(1) {
            assert!(src == "'self'" || src == "'none'", "CSP allows {src}");
        }
    }
}

#[test]
fn outside_url_detector_detects() {
    assert_eq!(
        outside_urls(r#"<img src="https://tiles.example/1.png">"#).len(),
        1
    );
    assert_eq!(
        outside_urls(r#"<script src="//cdn.example/x.js">"#).len(),
        1
    );
    assert_eq!(
        outside_urls("a { background: url(//x.example/a.png) }").len(),
        1
    );
    assert!(outside_urls(r#"<a href="t/p1">x</a> // comment"#).is_empty());
}

// rivet: verifies REQ-WEB-004
#[tokio::test]
async fn files_are_served_by_id_with_containment() {
    let (fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let id = idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.rel == b"a/plain.png")
        .unwrap()
        .0;
    let (s, h, body) = get(&app, &format!("/original/{id}"), false).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.get("content-type").unwrap(), "image/png");
    assert_eq!(body, std::fs::read(fx.root1.join("a/plain.png")).unwrap());
    let (s, h, _) = get(&app, &format!("/thumb/{id}"), false).await;
    assert_eq!(
        (s, h.get("content-type").unwrap().to_str().unwrap()),
        (StatusCode::OK, "image/jpeg")
    );

    // A stored path that escapes its root is refused, even from the index.
    idx.tamper_rel_for_test(id, b"../../outside/secret.jpg")
        .unwrap();
    assert!(fx.root1.join("../../outside/secret.jpg").exists());
    let (s, _, body) = get(&app, &format!("/original/{id}"), false).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(body.is_empty());

    // No route takes a path.
    for uri in [
        "/original/..%2F..%2Foutside%2Fsecret.jpg",
        "/original/a/plain.png",
        "/static/..%2FCargo.toml",
    ] {
        let (s, _, _) = get(&app, uri, false).await;
        assert_ne!(s, StatusCode::OK, "{uri}");
    }
}

// rivet: verifies REQ-WEB-002
#[tokio::test]
async fn base_path_prefixes_everything() {
    let (_fx, mut cfg, idx) = scanned();
    cfg.base_path = "/photos".into();
    let app = app(&cfg, &idx);
    let (s, _, body) = get(&app, "/photos/", false).await;
    assert_eq!(s, StatusCode::OK);
    let html = String::from_utf8(body).unwrap();
    assert!(html.contains(r#"<base href="/photos/">"#), "{html}");
    let rest = html.replace(r#"<base href="/photos/">"#, "");
    assert!(
        !rest.contains("href=\"/"),
        "absolute link bypasses the base path: {html}"
    );
    assert!(
        !rest.contains("src=\"/"),
        "absolute src bypasses the base path: {html}"
    );
    assert_eq!(
        get(&app, "/photos/static/lichtbild.css", false).await.0,
        StatusCode::OK
    );
    assert_eq!(get(&app, "/photos/t/p1", false).await.0, StatusCode::OK);
    let bare = get(&app, "/photos", false).await.0;
    assert!(
        bare == StatusCode::OK || bare.is_redirection() || bare == StatusCode::NOT_FOUND,
        "{bare}"
    );
    assert_eq!(get(&app, "/t/p1", false).await.0, StatusCode::NOT_FOUND);
}

// rivet: verifies REQ-WEB-001
#[tokio::test]
async fn timeline_groups_by_day_and_pages_with_htmx() {
    let fx = fixture();
    // 300 more photos on 3 days in root p2 (names carry the date).
    for i in 0..300u32 {
        let day = 10 + i / 100;
        std::fs::write(
            fx.root2
                .join(format!("IMG_202305{day}_{:06}.jpg", 100000 + i)),
            jpeg(8, 8, (i % 250) as u8),
        )
        .unwrap();
    }
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    assert!(
        scan::run(&cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    let app = app(&cfg, &idx);

    let (s, _, body) = get(&app, "/t/p2", false).await;
    assert_eq!(s, StatusCode::OK);
    let html = String::from_utf8(body).unwrap();
    assert_eq!(html.matches("class=\"item").count(), 240);
    assert_eq!(html.matches("<h2 class=\"year\">2023</h2>").count(), 1);
    assert_eq!(html.matches("<h3 class=\"month\">May</h3>").count(), 1);
    assert!(html.contains("<h4 class=\"day\">2023-05-12</h4>"));
    let more = html
        .split("hx-get=\"")
        .nth(1)
        .expect("no continuation")
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");

    let (_, _, body) = get(&app, &format!("/{more}"), true).await;
    let frag = String::from_utf8(body).unwrap();
    assert!(!frag.contains("<html"), "continuation is a fragment");
    // 61 left: 60 on 2023-05-10 and the 2022 photo.
    assert_eq!(frag.matches("class=\"item").count(), 61);
    assert!(
        !frag.contains("<h3 class=\"month\">May</h3>"),
        "month heading repeated"
    );
    assert!(frag.contains("<h2 class=\"year\">2022</h2>"));
    assert!(!frag.contains("hx-get"), "no more pages expected");

    // Weak dates are marked; the merged view holds both people.
    assert!(html.contains("item weak"));
    let (_, _, body) = get(&app, "/t/all", false).await;
    let all = String::from_utf8(body).unwrap();
    assert!(all.contains("2021-06-15") || all.contains("hx-get"));
    assert_eq!(get(&app, "/t/nobody", false).await.0, StatusCode::NOT_FOUND);
}
