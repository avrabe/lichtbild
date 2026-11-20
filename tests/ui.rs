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
// rivet: verifies REQ-WEB-015
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
        "/things",
        "/trips",
        "/show/p1",
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
        for href in [
            "./",
            "map",
            "things",
            "trips",
            "words",
            "favourites",
            "albums",
            "hidden",
            "duplicates",
            "status",
        ] {
            assert!(
                header.contains(&format!(r#"<a href="{href}">"#)),
                "{uri}: {href}"
            );
        }
        // Keys and gestures are one tap away everywhere (REQ-WEB-015).
        assert!(header.contains(r#"class="keyhelp""#), "{uri}");
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

// The steps say what runs now, what is done and what waits: faces wait
// for the photos to be looked at, with time left, and the People page says
// so instead of "no faces".
// rivet: verifies REQ-WEB-009
#[tokio::test]
async fn the_status_page_shows_the_steps_and_what_waits() {
    let mut e = env();
    e.state.vision_on = true;
    e.state.faces_on = true;
    let st = e.state.status.clone();
    let app = web::router(e.state.clone());
    st.set_phase(Phase::Looking {
        since: std::time::SystemTime::now() - std::time::Duration::from_secs(41 * 60),
        done: 856,
        total: 14635,
        report: Default::default(),
    });
    let (_, list) = get(&app, "/status", true).await;
    assert!(
        list.contains("Step 3 of 4: looking at photos, 856 of 14635 in this pass, about 10 h 5"),
        "{list}"
    );
    assert!(
        list.contains("<li class=\"run\"><b>Looking at photos</b><i>now</i>"),
        "{list}"
    );
    assert!(
        list.contains("<li class=\"wait\"><b>Faces</b><i>waits</i>"),
        "{list}"
    );
    assert!(
        list.contains("Starts when every photo has been looked at."),
        "{list}"
    );
    assert!(
        list.contains("<li class=\"done\"><b>Transcripts</b>")
            || list.contains("<li class=\"off\"><b>Transcripts</b>"),
        "{list}"
    );
    let (_, people) = get(&app, "/people", false).await;
    assert!(
        people.contains("Faces are looked for once every photo has been looked at; that pass is at 856 of 14635."),
        "{people}"
    );
    assert!(!people.contains("not in the vision directory"), "{people}");

    st.set_phase(Phase::Faces {
        since: std::time::SystemTime::now(),
        done: 10,
        total: 100,
        report: Default::default(),
    });
    let (_, list) = get(&app, "/status", true).await;
    assert!(
        list.contains("Step 4 of 4: looking for faces, 10 of 100 photos"),
        "{list}"
    );
    assert!(
        list.contains("<li class=\"run\"><b>Faces</b><i>now</i>"),
        "{list}"
    );
    let (_, people) = get(&app, "/people", false).await;
    assert!(
        people.contains("Looking for faces now: 10 of 100 photos."),
        "{people}"
    );
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
    assert_eq!((lt.items, lt.times), (1, 2));
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

fn arrow(page: &str, rel: &str) -> Option<u64> {
    let at = page.find(&format!(r#"rel="{rel}" href="v/"#))?;
    let rest = &page[at..];
    let href = rest.split("href=\"").nth(1)?.split('"').next()?;
    href.split('/').nth(2)?.split('?').next()?.parse().ok()
}

// With a filter, the date chooser offers only months that have such
// items, and the viewer opened from the timeline keeps the filter: its
// arrows step only through such items, and "Timeline" returns to the
// filtered timeline. Its arrows also skip what the person hid.
// rivet: verifies REQ-WEB-010
#[tokio::test]
async fn the_filter_holds_in_the_chooser_and_the_viewer() {
    let e = env();
    let app = web::router(e.state.clone());
    let month = e.idx.locate(e.video).unwrap().unwrap().taken_local[..7].to_string();

    let (_, page) = get(&app, "/t/p1?kind=video", false).await;
    // The rail and the menu both list the months.
    let months: std::collections::BTreeSet<&str> = page
        .split("data-ym=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .collect();
    assert_eq!(
        months.into_iter().collect::<Vec<_>>(),
        vec![month.as_str()],
        "{page}"
    );
    assert!(
        page.contains(&format!(r#"href="v/p1/{}?kind=video""#, e.video)),
        "{page}"
    );

    let (_, page) = get(&app, &format!("/v/p1/{}?kind=video", e.video), false).await;
    assert!(
        page.contains("&amp;kind=video#i"),
        "back to the filtered timeline: {page}"
    );
    assert_eq!(
        (arrow(&page, "prev"), arrow(&page, "next")),
        (None, None),
        "{page}"
    );

    // Unfiltered, the video has a photo beside it; hidden, it is skipped.
    let (_, page) = get(&app, &format!("/v/p1/{}", e.video), false).await;
    let (prev, next) = (arrow(&page, "prev"), arrow(&page, "next"));
    let beside = prev.or(next).expect("a neighbour");
    e.idx.set_hidden("operator", beside, true).unwrap();
    let (_, page) = get(&app, &format!("/v/p1/{}", e.video), false).await;
    assert_ne!(arrow(&page, "prev"), Some(beside), "{page}");
    assert_ne!(arrow(&page, "next"), Some(beside), "{page}");
}

// Suggestions as the search box is typed: things (in German and English),
// dates, words said; the earlier words kept, with nothing that finds
// nothing; only what the viewer's libraries hold; common words and single
// letters suggest nothing.
// rivet: verifies REQ-SEARCH-002
#[tokio::test]
async fn the_search_box_suggests_what_the_libraries_hold() {
    let e = env();
    let (_, photo) = e
        .idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.label == "p1" && r.video.is_none() && r.taken_local.starts_with("2020"))
        .unwrap();
    let look = lichtbild::vision::Look {
        text: lichtbild::ocr::Text {
            state: "none".into(),
            model: "test".into(),
            lines: vec![],
            docs: vec![],
        },
        objects: vec![lichtbild::objects::Object {
            kind: 15,
            score: 0.9,
            bbox: [0.1, 0.1, 0.6, 0.8],
        }],
        vector: vec![],
    };
    e.idx.set_look(&photo.hash, "test", &look).unwrap();
    e.state.search.rebuild(&e.idx, None).unwrap();
    let app = web::router(e.state.clone());

    let (s, l) = get(&app, "/suggest?q=Leu", true).await;
    assert_eq!(s, StatusCode::OK);
    assert!(l.contains(r#"href="search?q=leuchtturm""#), "{l}");
    for q in ["kat", "Cat", "katzen"] {
        let (_, l) = get(&app, &format!("/suggest?q={q}"), true).await;
        assert!(l.contains(r#"href="search?q=Katze""#), "{q}: {l}");
        assert!(l.contains("cat · 1"), "{q}: {l}");
    }
    let (_, l) = get(&app, "/suggest?q=jul", true).await;
    assert!(l.contains(r#"href="search?q=Juli%202019""#), "{l}");
    // The earlier words stay, and only what still finds something is
    // offered: the cat was photographed in 2020, not in 2019 or 2022.
    let (_, l) = get(&app, "/suggest?q=katze%2020", true).await;
    assert!(l.contains(r#"href="search?q=katze%202020""#), "{l}");
    assert!(l.contains("katze <b>2020</b>"), "{l}");
    assert!(!l.contains("2019") && !l.contains("2022"), "{l}");
    let (_, l) = get(&app, "/suggest?q=20", true).await;
    assert!(l.contains("2019") && l.contains("2022"), "{l}");
    // And it runs: the suggested query finds the photo.
    let (_, hits) = get(&app, "/search?q=katze%202020", false).await;
    assert!(
        hits.contains(&format!("/{}\"", photo_id(&e, &photo.hash))),
        "{hits}"
    );
    for q in ["l", "abe", "katze%20"] {
        let (_, l) = get(&app, &format!("/suggest?q={q}"), true).await;
        assert!(!l.contains("<a"), "{q}: {l}");
    }
    // Only what the viewer's libraries hold.
    let p1 = e.state.search.suggest("leu", &["p1".into()], 8);
    assert_eq!(p1.len(), 1);
    assert_eq!(p1[0].items, 1);
    assert!(e.state.search.suggest("leu", &["p2".into()], 8).is_empty());
    assert!(e.state.search.suggest("kat", &["p2".into()], 8).is_empty());
}

fn photo_id(e: &Env, hash: &str) -> u64 {
    e.idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.hash == hash)
        .unwrap()
        .0
}

// Results that differ offer terms to narrow them down, each a link to the
// query with the term added; the photo/video filter is kept.
// rivet: verifies REQ-SEARCH-003
#[tokio::test]
async fn search_results_offer_terms_that_narrow_them_down() {
    let e = env();
    let app = web::router(e.state.clone());
    let (_, page) = get(&app, "/search?q=img", false).await;
    assert!(page.contains("<nav class=\"narrow\""), "{page}");
    assert!(page.contains("href=\"search?q=img%202020\""), "{page}");
    let narrowed = page.split("href=\"search?q=img%202020\"").nth(1).unwrap();
    assert!(narrowed.starts_with("><span aria-hidden=\"true\">◷</span> 2020 <span class=\"n\">"));
    let (_, page) = get(&app, "/search?q=img&kind=photo", false).await;
    assert!(
        page.contains("href=\"search?q=img%202020&amp;kind=photo\""),
        "{page}"
    );
    // Narrowed to one year: that year is not offered again.
    let (_, page) = get(&app, "/search?q=img%202020", false).await;
    assert!(!page.contains("search?q=img%202020%202020"), "{page}");
}

// Dates are found by season and holiday, in German and English, and the
// box suggests them.
// rivet: verifies REQ-SEARCH-004
#[tokio::test]
async fn pictures_are_found_by_season_and_holiday() {
    let e = env();
    let app = web::router(e.state.clone());
    for q in ["winter", "Winter%202020"] {
        let (_, hits) = get(&app, &format!("/search?q={q}"), false).await;
        assert!(hits.contains("2020-01-02 03:04:05"), "{q}: {hits}");
        assert!(!hits.contains("2019-07-04"), "{q}: {hits}");
    }
    let (_, hits) = get(&app, "/search?q=sommer", false).await;
    assert!(hits.contains("2019-07-04"), "{hits}");
    let (_, l) = get(&app, "/suggest?q=som", true).await;
    assert!(l.contains(r#"href="search?q=Sommer%202019""#), "{l}");
}

// A chip finds what it promises: text read in pictures holds no year, so
// a search that matches only that text offers no year that would find
// nothing.
// rivet: verifies REQ-SEARCH-003
#[tokio::test]
async fn narrowing_never_offers_a_dead_end() {
    let e = env();
    let photos: Vec<_> = e
        .idx
        .all()
        .unwrap()
        .into_iter()
        .filter(|(_, r)| r.video.is_none() && r.thumb == "ok")
        .collect();
    assert!(photos.len() >= 4, "{}", photos.len());
    let years: std::collections::HashSet<_> = photos
        .iter()
        .map(|(_, r)| r.taken_local[..4].to_string())
        .collect();
    assert!(years.len() >= 2, "{years:?}");
    for (_, r) in &photos {
        let look = lichtbild::vision::Look {
            text: lichtbild::ocr::Text {
                state: "ok".into(),
                model: "test".into(),
                lines: vec![lichtbild::ocr::Line {
                    quad: [0.1, 0.1, 0.5, 0.1, 0.5, 0.2, 0.1, 0.2],
                    block: 0,
                    text: "Geburtstag".into(),
                    conf: 0.95,
                }],
                docs: vec![0],
            },
            objects: vec![],
            vector: vec![],
        };
        e.idx.set_look(&r.hash, "test", &look).unwrap();
    }
    e.state.search.rebuild(&e.idx, None).unwrap();
    let app = web::router(e.state.clone());
    let (_, page) = get(&app, "/search?q=geburtstag", false).await;
    assert!(page.contains("results for"), "{page}");
    assert!(!page.contains("<nav class=\"narrow\""), "{page}");
}
