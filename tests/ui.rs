//! The UI round (REQ-WEB-009 to REQ-WEB-011, REQ-WORDS-001): the app menu,
//! the status page, the words cloud, the photo/video filter and "on this
//! day". Synthetic pictures and the synthetic test video only.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::{config, fixture, jpeg};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::status::{Phase, ScanDone};
use lichtbild::stt::{Piece, Transcript};
use lichtbild::web::{self, AppState};

const VIDEO: &[u8] = include_bytes!("fixtures/video/synthetic-h264-160x120.mp4");

async fn get(app: &axum::Router, uri: &str, htmx: bool) -> (StatusCode, String) {
    let mut r = Request::builder().uri(uri).header("host", "localhost:8080");
    if htmx {
        r = r.header("hx-request", "true");
    }
    let res = app
        .clone()
        .oneshot(r.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let s = res.status();
    let b = res.into_body().collect().await.unwrap().to_bytes();
    (s, String::from_utf8_lossy(&b).into_owned())
}

struct Env {
    _fx: common::Fixture,
    idx: Arc<Index>,
    state: AppState,
    video: u64,
}

/// The common fixture, plus a video in p1 with a transcript, and a photo
/// in p2 taken on today's month and day one year ago.
fn env() -> Env {
    let fx = fixture();
    std::fs::write(fx.root1.join("a/clip.mp4"), VIDEO).unwrap();
    let today = chrono::Local::now();
    let last_year = format!(
        "IMG_{}{}_120000.jpg",
        today.format("%Y").to_string().parse::<i32>().unwrap() - 1,
        today.format("%m%d")
    );
    std::fs::write(fx.root2.join(&last_year), jpeg(32, 24, 9)).unwrap();
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    let opts = Options {
        keyframe_decoder: Some(env!("CARGO_BIN_EXE_lichtbild").into()),
        ..Options::default()
    };
    assert!(scan::run(&cfg, &idx, &opts).unwrap().reconciled);
    let (video, rec) = idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.video.is_some())
        .unwrap();
    let piece = |t: &str| Piece {
        start: 1.0,
        end: 2.0,
        lang: "de".into(),
        text: t.into(),
    };
    idx.set_transcript(
        &rec.hash,
        &Transcript {
            state: "ok".into(),
            model: "whisper-base".into(),
            audio_seconds: 1.0,
            partial: false,
            pieces: vec![
                piece("Wir fahren heute zum Leuchtturm, und der Leuchtturm ist hoch."),
                piece("Aber nicht jetzt, sagt sie."),
            ],
        },
    )
    .unwrap();
    let state = AppState::new(&cfg, idx.clone());
    state.search.rebuild(&idx, None).unwrap();
    Env {
        _fx: fx,
        idx,
        state,
        video,
    }
}

// Every page behind login carries the app menu with all places; the
// house's nav stays beside it.
// rivet: verifies REQ-WEB-011
#[tokio::test]
async fn every_page_has_the_app_menu() {
    let e = env();
    let app = web::router(e.state.clone());
    for uri in [
        "/",
        "/t/p1",
        "/search?q=x",
        "/status",
        "/words",
        "/map",
        "/hidden",
        "/duplicates",
    ] {
        let (s, page) = get(&app, uri, false).await;
        assert_eq!(s, StatusCode::OK, "{uri}");
        let header = page
            .split("<header>")
            .nth(1)
            .unwrap()
            .split("</header>")
            .next()
            .unwrap();
        assert!(
            header.contains(r#"<details class="appmenu">"#),
            "{uri}: {header}"
        );
        for href in ["./", "map", "words", "hidden", "duplicates", "status"] {
            assert!(
                header.contains(&format!(r#"<a href="{href}">"#)),
                "{uri}: {href}"
            );
        }
    }
}

// The status page says what runs now and what ran last, and refreshes
// itself through htmx with just the list.
// rivet: verifies REQ-WEB-009
#[tokio::test]
async fn the_status_page_shows_scan_and_transcription() {
    let e = env();
    let st = e.state.status.clone();
    let app = web::router(e.state.clone());
    let (_, page) = get(&app, "/status", false).await;
    assert!(page.contains("Starting"), "{page}");
    assert!(page.contains(r#"hx-trigger="every 10s""#), "{page}");

    st.scan_done(ScanDone {
        ended: std::time::SystemTime::now(),
        took: std::time::Duration::from_secs(125),
        found: 17,
        indexed: 15,
        skipped: 2,
        reconciled: true,
        failed: false,
    });
    st.set_phase(Phase::Transcribing {
        since: std::time::SystemTime::now(),
        done: 3,
        total: 8,
        report: Default::default(),
    });
    st.set_duplicates(4, 9);
    let (_, list) = get(&app, "/status", true).await;
    assert!(
        list.starts_with(r#"<dl class="status""#),
        "the fragment only: {list}"
    );
    assert!(list.contains("3 of 8 done"), "{list}");
    assert!(list.contains(r#"<progress max="8" value="3">"#), "{list}");
    assert!(
        list.contains("took 2 min 5 s: 17 files found, 15 indexed, 2 skipped"),
        "{list}"
    );
    assert!(list.contains("4 groups with 9 items"), "{list}");
    assert!(list.contains(lichtbild::VERSION), "{list}");
    // The operator sees every library, so the totals too.
    assert!(list.contains("<dt>Search</dt>"), "{list}");

    // A failed scan says so, but not why: the error can name paths.
    st.scan_done(ScanDone {
        ended: std::time::SystemTime::now(),
        took: std::time::Duration::from_secs(1),
        found: 0,
        indexed: 0,
        skipped: 0,
        reconciled: false,
        failed: true,
    });
    let (_, list) = get(&app, "/status", true).await;
    assert!(list.contains("the server's log says why"), "{list}");
}

// The words cloud: frequent words of the viewer's videos, without common
// words, each a search; nothing from a library the viewer may not see.
// rivet: verifies REQ-WORDS-001
#[tokio::test]
async fn words_come_from_the_viewers_videos_without_common_words() {
    let e = env();
    let app = web::router(e.state.clone());
    let (s, page) = get(&app, "/words", false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains(r#"href="search?q=leuchtturm""#), "{page}");
    assert!(page.contains(r#"href="search?q=fahren""#), "{page}");
    for common in ["aber", "nicht", "jetzt", "sagt", "heute"] {
        assert!(
            !page.contains(&format!("search?q={common}\"")),
            "{common}: {page}"
        );
    }
    // Said twice in one video: once for "videos", twice for "times".
    let w = lichtbild::words::cloud(&e.idx, &["p1".into()], 10).unwrap();
    let lt = w.iter().find(|w| w.word == "leuchtturm").unwrap();
    assert_eq!((lt.videos, lt.times), (1, 2));
    // p2 holds no video: its cloud is empty.
    assert!(lichtbild::words::cloud(&e.idx, &["p2".into()], 10)
        .unwrap()
        .is_empty());
}

fn tiles(page: &str) -> Vec<u64> {
    page.split("id=\"i")
        .skip(1)
        .filter_map(|s| s.split('"').next()?.parse().ok())
        .collect()
}

// Photos or videos only, in the timeline (its pages and jumps keep the
// filter), in search and on the map.
// rivet: verifies REQ-WEB-010
#[tokio::test]
async fn photos_or_videos_only() {
    let e = env();
    let app = web::router(e.state.clone());
    let (_, all) = get(&app, "/t/p1", false).await;
    assert!(tiles(&all).contains(&e.video));
    let (_, videos) = get(&app, "/t/p1?kind=video", false).await;
    assert_eq!(tiles(&videos), vec![e.video], "{videos}");
    assert!(
        videos.contains(r#"<a href="t/p1?kind=video" class="on" aria-current="true">Videos</a>"#),
        "{videos}"
    );
    assert!(
        videos.contains("?from=") && videos.contains("&amp;kind=video\""),
        "jump links keep it: {videos}"
    );
    let (_, photos) = get(&app, "/t/p1?kind=photo", false).await;
    let p = tiles(&photos);
    assert!(!p.is_empty() && !p.contains(&e.video), "{p:?}");

    let (_, hits) = get(&app, "/search?q=leuchtturm&kind=photo", false).await;
    assert!(!hits.contains(&format!("/{}", e.video)), "{hits}");
    let (_, hits) = get(&app, "/search?q=leuchtturm&kind=video", false).await;
    assert!(hits.contains(&format!("/{}?t=", e.video)), "{hits}");

    let (_, json) = get(&app, "/map/clusters?z=0&kind=video", false).await;
    assert!(
        json.contains("\"total\":1"),
        "the video has a position: {json}"
    );
    let (_, json) = get(&app, "/map/clusters?z=0&kind=photo", false).await;
    assert!(!json.contains(&format!("\"id\":{}", e.video)), "{json}");
}

// On this day: pictures from today's month and day in earlier years.
// rivet: verifies REQ-WEB-011
#[tokio::test]
async fn home_shows_this_day_in_earlier_years() {
    let e = env();
    let app = web::router(e.state.clone());
    let (_, page) = get(&app, "/", false).await;
    let otd = page
        .split(r#"<section class="otd">"#)
        .nth(1)
        .unwrap_or_else(|| panic!("no on-this-day section: {page}"));
    let year = chrono::Local::now()
        .format("%Y")
        .to_string()
        .parse::<i32>()
        .unwrap()
        - 1;
    assert!(
        otd.contains(&format!(r#"<span class="year">{year}</span>"#)),
        "{otd}"
    );
}
