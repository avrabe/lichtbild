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
    get_host(app, uri, htmx, Some("localhost:8080")).await
}

async fn get_host(
    app: &axum::Router,
    uri: &str,
    htmx: bool,
    host: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(uri);
    if let Some(h) = host {
        req = req.header("host", h);
    }
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
        // The CSP blocks inline style and script, so pages must not use them.
        if !uri.starts_with("/static") && !uri.ends_with(".js") && !uri.ends_with(".css") {
            assert!(
                !text.contains(" style="),
                "{uri} has an inline style attribute"
            );
            assert!(
                !text.contains("<style"),
                "{uri} has an inline style element"
            );
            for tag in text.split("<script").skip(1) {
                assert!(
                    tag.trim_start().starts_with("src="),
                    "{uri} has an inline script"
                );
            }
        }
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
    // Newest first: day headings strictly descending, on both pages.
    let days = |h: &str| -> Vec<String> {
        h.split("<h4 class=\"day\">")
            .skip(1)
            .map(|x| x[..10].to_string())
            .collect()
    };
    let mut seen = days(&html);
    seen.extend(days(&frag));
    assert!(seen.windows(2).all(|w| w[0] > w[1]), "{seen:?}");
    // The merged view holds both people: page through it to the end.
    let mut uri = "/t/all".to_string();
    let (mut total, mut from_p1) = (0, 0);
    loop {
        let (_, _, body) = get(&app, &uri, uri != "/t/all").await;
        let page = String::from_utf8(body).unwrap();
        total += page.matches("class=\"item").count();
        from_p1 += page.matches("2021-06-15").count();
        match page.split("hx-get=\"").nth(1) {
            Some(rest) => {
                uri = format!("/{}", rest.split('"').next().unwrap().replace("&amp;", "&"))
            }
            None => break,
        }
    }
    assert_eq!(total, 301 + 5, "merged view lost or duplicated items");
    assert!(from_p1 > 0, "merged view has no item from p1");
    assert_eq!(get(&app, "/t/nobody", false).await.0, StatusCode::NOT_FOUND);
}

// rivet: verifies REQ-WEB-005
#[tokio::test]
async fn requests_for_other_hosts_are_refused() {
    let (_fx, mut cfg, idx) = scanned();
    cfg.allowed_hosts = vec!["photos.example".into()];
    let app = app(&cfg, &idx);
    for host in [
        "localhost",
        "localhost:8080",
        "127.0.0.1:18080",
        "[::1]:8080",
        "photos.example",
        "Photos.Example:443",
    ] {
        let (s, _, _) = get_host(&app, "/t/p1", false, Some(host)).await;
        assert_eq!(s, StatusCode::OK, "{host}");
    }
    for host in [
        Some("evil.example"),
        Some("photos.example.evil.example"),
        Some("localhost.evil.example:8080"),
        None,
    ] {
        let (s, h, body) = get_host(&app, "/t/p1", false, host).await;
        assert_eq!(s, StatusCode::MISDIRECTED_REQUEST, "{host:?}");
        assert!(h.get("content-security-policy").is_some());
        assert!(
            !String::from_utf8_lossy(&body).contains("item"),
            "{host:?} got content"
        );
        let (s, _, _) = get_host(&app, "/thumb/1", false, host).await;
        assert_eq!(s, StatusCode::MISDIRECTED_REQUEST, "{host:?}");
    }
}

// rivet: verifies REQ-WEB-006
#[tokio::test]
async fn date_scrubber_jumps_to_years_and_months() {
    let fx = fixture();
    // 120 photos in May 2023 and 30 in January 2024 in p2, plus its 2022 photo.
    for i in 0..150u32 {
        let name = if i < 120 {
            format!("IMG_20230515_{:06}.jpg", 100000 + i)
        } else {
            format!("IMG_20240120_{:06}.jpg", 100000 + i)
        };
        std::fs::write(fx.root2.join(name), jpeg(8, 8, (i % 250) as u8)).unwrap();
    }
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    assert!(
        scan::run(&cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    let app = app(&cfg, &idx);

    // The rail lists every month with its count, newest first, and the
    // phone menu has the same links.
    let (_, _, body) = get(&app, "/t/p2", false).await;
    let html = String::from_utf8(body).unwrap();
    let pos = |needle: &str| {
        html.find(needle)
            .unwrap_or_else(|| panic!("missing {needle}"))
    };
    let jan24 = pos(r#"data-n="30" data-ym="2024-01""#);
    let may23 = pos(r#"data-n="120" data-ym="2023-05""#);
    let mar22 = pos(r#"data-n="1" data-ym="2022-03""#);
    assert!(jan24 < may23 && may23 < mar22, "rail not newest first");
    assert!(html.contains(r#"<a class="y" href="t/p2?from=2023">2023</a>"#));
    assert!(html.contains(r#"<details class="jumpmenu">"#));
    assert_eq!(
        html.matches(r#"href="t/p2?from=2023-05""#).count(),
        2,
        "rail and menu"
    );

    // Jump to a month: the page starts there and links back up.
    let (s, _, body) = get(&app, "/t/p2?from=2023-05", false).await;
    assert_eq!(s, StatusCode::OK);
    let page = String::from_utf8(body).unwrap();
    let timeline = page.split("<div class=\"timeline\">").nth(1).unwrap();
    assert_eq!(
        timeline.matches("class=\"item").count(),
        121,
        "May 2023 and the 2022 photo"
    );
    assert!(
        !timeline.contains("2024-01-20"),
        "a newer month leaked into the jump"
    );
    assert!(
        page.contains(r#"<p class="newer"><a href="t/p2?from=2024-01">"#),
        "{page}"
    );

    // Jump to a year; the newest month has a plain link back.
    let (_, _, body) = get(&app, "/t/p2?from=2022", false).await;
    let page = String::from_utf8(body).unwrap();
    let timeline = page.split("<div class=\"timeline\">").nth(1).unwrap();
    assert_eq!(timeline.matches("class=\"item").count(), 1);
    assert!(page.contains(r#"<p class="newer"><a href="t/p2?from=2023-05">"#));
    let (_, _, body) = get(&app, "/t/p2?from=2024-01", false).await;
    assert!(String::from_utf8(body)
        .unwrap()
        .contains(r#"<p class="newer"><a href="t/p2">"#));

    // Bad input is refused, not guessed.
    for bad in ["2023-13", "2023-5", "23", "2023-05-01", "abcd", "2023-00"] {
        let (s, _, _) = get(&app, &format!("/t/p2?from={bad}"), false).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }

    // The merged view's rail counts both people.
    let (_, _, body) = get(&app, "/t/all", false).await;
    let all = String::from_utf8(body).unwrap();
    assert!(
        all.contains(r#"data-ym="2021-06""#),
        "p1's month missing from merged rail"
    );
    assert!(all.contains(r#"data-n="120" data-ym="2023-05""#));
}

#[test]
fn from_bounds() {
    assert_eq!(
        web::parse_from("2023").as_deref(),
        Some("2024-01-01T00:00:00")
    );
    assert_eq!(
        web::parse_from("2023-05").as_deref(),
        Some("2023-06-01T00:00:00")
    );
    assert_eq!(
        web::parse_from("2023-12").as_deref(),
        Some("2024-01-01T00:00:00")
    );
    assert_eq!(web::parse_from("2023-13"), None);
    assert_eq!(web::parse_from("2023-00"), None);
    assert_eq!(web::parse_from("２０２３"), None);
}

// rivet: verifies REQ-WEB-001
#[tokio::test]
async fn home_shows_counts_including_merged() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let (_, _, body) = get(&app, "/", false).await;
    let html = String::from_utf8(body).unwrap();
    assert!(
        html.contains(r#"<a href="t/p1">p1</a> <span class="n">5</span>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<a href="t/p2">p2</a> <span class="n">1</span>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<a href="t/all">merged</a> <span class="n">6</span>"#),
        "{html}"
    );
}

fn item_ids(html: &str, who: &str) -> Vec<u64> {
    let pat = format!("href=\"v/{who}/");
    html.split(&pat)
        .skip(1)
        .map(|x| x.split('"').next().unwrap().parse().unwrap())
        .collect()
}

fn link_target(html: &str, class: &str) -> Option<u64> {
    let pat = format!("<a class=\"{class}\" rel=\"{class}\" href=\"v/");
    html.split(&pat).nth(1).map(|x| {
        x.split('"')
            .next()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    })
}

// rivet: verifies REQ-WEB-007
#[tokio::test]
async fn viewer_steps_through_the_timeline() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let (_, _, body) = get(&app, "/t/p1", false).await;
    let ids = item_ids(&String::from_utf8(body).unwrap(), "p1");
    assert_eq!(ids.len(), 5);
    for (i, id) in ids.iter().enumerate() {
        let (s, h, body) = get(&app, &format!("/v/p1/{id}"), false).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h.get("content-security-policy").is_some());
        let page = String::from_utf8(body).unwrap();
        // The original, or for HEIC its JPEG preview.
        assert!(
            page.contains(&format!(r#"<img class="photo" src="original/{id}""#))
                || page.contains(&format!(r#"<img class="photo" src="preview/{id}?h="#)),
            "{page}"
        );
        // Previous = newer = the item before it in the timeline; next = older.
        let want_prev = if i == 0 { None } else { Some(ids[i - 1]) };
        assert_eq!(link_target(&page, "prev"), want_prev, "prev of {id}");
        assert_eq!(
            link_target(&page, "next"),
            ids.get(i + 1).copied(),
            "next of {id}"
        );
        assert!(!page.contains(" style="), "inline style in viewer");
        for tag in page.split("<script").skip(1) {
            assert!(
                tag.trim_start().starts_with("src="),
                "inline script in viewer"
            );
        }
        // Back to the timeline lands on this photo: it is the page's first item.
        let back = page
            .split(r#"<a class="back" href=""#)
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .replace("&amp;", "&");
        assert!(back.ends_with(&format!("#i{id}")), "{back}");
        let (s, _, body) = get(
            &app,
            &format!("/{}", back.split('#').next().unwrap()),
            false,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let tl = String::from_utf8(body).unwrap();
        let timeline = tl.split("<div class=\"timeline\">").nth(1).unwrap();
        assert_eq!(
            item_ids(timeline, "p1").first(),
            Some(id),
            "back did not land on {id}"
        );
        assert!(tl.contains(&format!("id=\"i{id}\"")));
    }
}

// rivet: verifies REQ-WEB-007
#[tokio::test]
async fn viewer_respects_the_timeline_it_was_opened_from() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let (_, _, body) = get(&app, "/t/p2", false).await;
    let p2 = item_ids(&String::from_utf8(body).unwrap(), "p2");
    assert_eq!(p2.len(), 1);
    // p2's photo is not part of p1's timeline.
    assert_eq!(
        get(&app, &format!("/v/p1/{}", p2[0]), false).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&app, &format!("/v/p2/{}", p2[0]), false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        get(&app, &format!("/v/nobody/{}", p2[0]), false).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&app, "/v/p1/999999", false).await.0,
        StatusCode::NOT_FOUND
    );
    // The merged view steps across both people in date order.
    let (_, _, body) = get(&app, "/t/all", false).await;
    let all = item_ids(&String::from_utf8(body).unwrap(), "all");
    assert_eq!(all.len(), 6);
    for (i, id) in all.iter().enumerate() {
        let (_, _, body) = get(&app, &format!("/v/all/{id}"), false).await;
        let page = String::from_utf8(body).unwrap();
        assert_eq!(
            link_target(&page, "next"),
            all.get(i + 1).copied(),
            "merged next of {id}"
        );
    }
}

#[tokio::test]
async fn asset_urls_change_with_their_content() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let (_, _, body) = get(&app, "/", false).await;
    let html = String::from_utf8(body).unwrap();
    let v = web::asset_version();
    assert_eq!(v.len(), 10);
    assert!(
        html.contains(&format!(r#"href="static/lichtbild.css?v={v}""#)),
        "{html}"
    );
    assert!(html.contains(&format!(r#"src="static/lichtbild.js?v={v}""#)));
    // The versioned URL is served.
    let (s, h, _) = get(&app, &format!("/static/lichtbild.css?v={v}"), false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/css"));
}

// A replaced file gets a new thumbnail URL, so a browser's cached thumbnail
// for the old content is never shown for the new one.
#[tokio::test]
async fn thumbnail_urls_change_when_the_file_changes() {
    let fx = fixture();
    let cfg = config(&fx, &fx.data);
    let url_of = |html: &str| {
        html.split(r#"<img src=""#)
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string()
    };
    let first = {
        let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
        scan::run(&cfg, &idx, &Options::default()).unwrap();
        let (_, _, body) = get(&app(&cfg, &idx), "/t/p2", false).await;
        url_of(&String::from_utf8(body).unwrap())
    };
    assert!(
        first.starts_with("thumb/") && first.contains("?h="),
        "{first}"
    );
    std::fs::write(fx.root2.join("IMG_20220303_101010.jpg"), jpeg(24, 20, 99)).unwrap();
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    scan::run(&cfg, &idx, &Options::default()).unwrap();
    let app = app(&cfg, &idx);
    let (_, _, body) = get(&app, "/t/p2", false).await;
    let second = url_of(&String::from_utf8(body).unwrap());
    assert_ne!(first, second, "same thumbnail URL after the file changed");
    assert_eq!(
        first.split('?').next(),
        second.split('?').next(),
        "same item id"
    );
    assert_eq!(
        get(&app, &format!("/{second}"), false).await.0,
        StatusCode::OK
    );
}

// rivet: verifies REQ-MEDIA-002
#[tokio::test]
async fn viewer_shows_the_jpeg_preview_for_heic() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let all = idx.all().unwrap();
    let (heic_id, heic) = all.iter().find(|(_, r)| r.rel.ends_with(b".HEIC")).unwrap();
    let (jpg_id, _) = all
        .iter()
        .find(|(_, r)| r.rel == b"a/IMG_exif.jpg")
        .unwrap();
    let (_, _, body) = get(&app, &format!("/v/p1/{heic_id}"), false).await;
    let page = String::from_utf8(body).unwrap();
    let want = format!(
        r#"<img class="photo" src="preview/{heic_id}?h={}""#,
        &heic.hash[..12]
    );
    assert!(page.contains(&want), "{page}");
    let (s, h, body) = get(&app, &format!("/preview/{heic_id}"), false).await;
    assert_eq!(
        (s, h.get("content-type").unwrap().to_str().unwrap()),
        (StatusCode::OK, "image/jpeg")
    );
    assert!(image::load_from_memory(&body).is_ok());
    let (s, h, _) = get(&app, &format!("/original/{heic_id}"), false).await;
    assert_eq!(
        (s, h.get("content-type").unwrap().to_str().unwrap()),
        (StatusCode::OK, "image/heic")
    );
    // A JPEG is shown as itself and has no preview.
    let (_, _, body) = get(&app, &format!("/v/p1/{jpg_id}"), false).await;
    assert!(String::from_utf8(body)
        .unwrap()
        .contains(&format!(r#"src="original/{jpg_id}""#)));
    assert_eq!(
        get(&app, &format!("/preview/{jpg_id}"), false).await.0,
        StatusCode::NOT_FOUND
    );
}

fn synthetic_places() -> lichtbild::places::Places {
    let f = |n: &str| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(n)
    };
    lichtbild::places::Places::load(
        &f("places-cities.txt"),
        Some(&f("places-admin1.txt")),
        Some(&f("places-countries.txt")),
    )
    .unwrap()
}

// rivet: verifies REQ-WEB-008
// rivet: verifies REQ-PLACE-001
#[tokio::test]
async fn viewer_info_panel_shows_date_place_and_camera() {
    let (_fx, cfg, idx) = scanned();
    let all = idx.all().unwrap();
    let id_of = |rel: &[u8]| all.iter().find(|(_, r)| r.rel == rel).unwrap().0;
    let exif_id = id_of(b"a/IMG_exif.jpg");
    let named_id = id_of(b"a/IMG_20200102_030405.jpg");

    let with_places =
        web::router(AppState::new(&cfg, idx.clone()).with_places(Some(synthetic_places())));
    let (s, _, body) = get(&with_places, &format!("/v/p1/{exif_id}"), false).await;
    assert_eq!(s, StatusCode::OK);
    let page = String::from_utf8(body).unwrap();
    assert!(page.contains(r#"<details class="info">"#), "{page}");
    for want in [
        "2021-06-15 10:11:12",
        "UTC+02:00",
        "from camera (EXIF)",
        "near Ostdorf, Nordprovinz, Testland",
        "10.50000, 20.25000",
        "Lichtbild Testkamera 1",
        "1/120 s",
        "f/1.8",
        "ISO 50",
        "4.2 mm",
        "64 × 48",
        " kB</span>",
        "IMG_exif.jpg",
    ] {
        assert!(page.contains(want), "info panel lacks {want:?}");
    }
    assert!(!page.contains(" style="), "inline style in viewer");

    // No GPS: said so, no place.
    let (_, _, body) = get(&with_places, &format!("/v/p1/{named_id}"), false).await;
    let page = String::from_utf8(body).unwrap();
    assert!(page.contains("no position recorded"));
    assert!(page.contains("from file name"));
    assert!(
        !page.contains("near "),
        "a place for a photo without position"
    );

    // Without the GeoNames files: coordinates only.
    let without = app(&cfg, &idx);
    let (_, _, body) = get(&without, &format!("/v/p1/{exif_id}"), false).await;
    let page = String::from_utf8(body).unwrap();
    assert!(page.contains("10.50000, 20.25000"));
    assert!(!page.contains("Ostdorf"));
}

// rivet: verifies REQ-MEDIA-002
#[tokio::test]
async fn viewer_explains_an_undecodable_heic() {
    let fx = fixture();
    let cut = &HEIC_640X480[..HEIC_640X480.len() - 2000];
    std::fs::write(fx.root1.join("b/IMG_20190705_000000.HEIC"), cut).unwrap();
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    scan::run(&cfg, &idx, &Options::default()).unwrap();
    let id = idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.rel == b"b/IMG_20190705_000000.HEIC")
        .unwrap()
        .0;
    let (_, _, body) = get(&app(&cfg, &idx), &format!("/v/p1/{id}"), false).await;
    let page = String::from_utf8(body).unwrap();
    assert!(page.contains("could not be decoded"), "{page}");
    assert!(page.contains(&format!(r#"<a href="original/{id}" download>"#)));
    assert!(
        !page.contains(r#"<img class="photo""#),
        "a broken image instead of the note"
    );
}

// rivet: verifies REQ-NAV-001
#[tokio::test]
async fn nav_switcher_is_in_every_page_header() {
    let fx = fixture();
    let nav = fx.data.with_file_name("nav.json");
    std::fs::write(
        &nav,
        r#"{"version": 1, "home": {"name": "home", "url": "/"},
            "apps": [{"id": "lichtbild", "name": "lichtbild", "url": "/lichtbild/", "desc": "photos"},
                     {"id": "other", "name": "other app", "url": "/other/"},
                     {"id": "login", "name": "login", "url": "https://login.example.test/"}]}"#,
    )
    .unwrap();
    let mut cfg = config(&fx, &fx.data);
    cfg.nav_html = lichtbild::config::nav_html(Some(&nav), Some("lichtbild")).unwrap();
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    scan::run(&cfg, &idx, &Options::default()).unwrap();
    let app = app(&cfg, &idx);
    let id = idx.all().unwrap()[0].0;
    let label = idx.all().unwrap()[0].1.label.clone();
    for uri in ["/".to_string(), "/t/p1".into(), format!("/v/{label}/{id}")] {
        let (_, _, body) = get(&app, &uri, false).await;
        let page = String::from_utf8(body).unwrap();
        let header = page
            .split("<header>")
            .nth(1)
            .unwrap()
            .split("</header>")
            .next()
            .unwrap();
        assert!(
            header.contains(r#"<nav class="apps"><a class="home" href="/">← home</a>"#),
            "{uri}: {header}"
        );
        assert!(
            header.contains(
                r#"<a href="/lichtbild/" aria-current="page" title="photos">lichtbild</a>"#
            ),
            "{uri}"
        );
        assert!(header.contains(r#"<a href="/other/">other app</a>"#));
        // Links to other house apps are links, never resources the page loads.
        assert!(!header.contains("src="), "{uri}");
        assert_eq!(header.matches("href=\"https://").count(), 1);
    }
    // Both settings or neither; a bad file names the field.
    assert!(lichtbild::config::nav_html(Some(&nav), None).is_err());
    assert!(lichtbild::config::nav_html(None, Some("lichtbild")).is_err());
    assert!(lichtbild::config::nav_html(None, None).unwrap().is_empty());
    let e = lichtbild::config::nav_html(Some(&nav), Some("photos"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("self"), "{e}");
    std::fs::write(
        &nav,
        r#"{"version": 2, "home": {"name": "h", "url": "/"}, "apps": []}"#,
    )
    .unwrap();
    let e = lichtbild::config::nav_html(Some(&nav), Some("lichtbild"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("version"), "{e}");
}

// Every page behind login says which lichtbild build serves it.
// rivet: verifies REQ-WEB-001
#[tokio::test]
async fn pages_show_the_running_version() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let footer = format!(
        "<footer class=\"version\">lichtbild {}</footer>",
        lichtbild::VERSION
    );
    for uri in ["/", "/t/p1", "/search?q=x"] {
        let (s, h, body) = get(&app, uri, false).await;
        let body = String::from_utf8(body).unwrap();
        assert_eq!(s, StatusCode::OK, "{uri}");
        assert!(body.contains(&footer), "{uri}: {body}");
        // Pages are revalidated, so an upgrade never leaves an old page.
        assert_eq!(h["cache-control"], "no-cache, private", "{uri}");
    }
    // Static files keep their caching; their URLs carry the version.
    let (_, h, _) = get(&app, "/static/lichtbild.css", false).await;
    assert!(
        h["cache-control"].to_str().unwrap().contains("max-age"),
        "{h:?}"
    );
}

// The viewer offers full screen, and its script carries the zoom; both
// are same-origin and work without a server round trip.
// rivet: verifies REQ-WEB-007
#[tokio::test]
async fn viewer_offers_full_screen_and_zoom() {
    let (_fx, cfg, idx) = scanned();
    let app = app(&cfg, &idx);
    let (_, _, body) = get(&app, "/t/p2", false).await;
    let p2 = item_ids(&String::from_utf8(body).unwrap(), "p2");
    let (s, _, body) = get(&app, &format!("/v/p2/{}", p2[0]), false).await;
    let page = String::from_utf8(body).unwrap();
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("class=\"fs\""), "{page}");
    let (_, _, js) = get(&app, "/static/lichtbild.js", false).await;
    let js = String::from_utf8(js).unwrap();
    for needle in [
        "requestFullscreen",
        "\"wheel\"",
        "\"dblclick\"",
        "pointermove",
    ] {
        assert!(js.contains(needle), "{needle} missing from lichtbild.js");
    }
}
