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
        "/recent",
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

// A video's transcript plays along as captions: a track on the video, and
// WebVTT cues with times; a photo, or an unknown item, has none.
// rivet: verifies REQ-WEB-022
#[tokio::test]
async fn a_video_plays_with_its_transcript_as_captions() {
    let e = env();
    let app = web::router(e.state.clone());
    let (_, page) = get(&app, &format!("/v/p1/{}", e.video), false).await;
    assert!(
        page.contains(&format!(
            "<track kind=\"captions\" src=\"captions/{}\" srclang=\"de\" label=\"What is said\" default></video>",
            e.video
        )),
        "{page}"
    );
    let (s, vtt) = get(&app, &format!("/captions/{}", e.video), false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(vtt.starts_with("WEBVTT\n"), "{vtt}");
    assert!(
        vtt.contains("\n00:00:01.000 --> 00:00:02.000\nWir fahren heute zum Leuchtturm, und der Leuchtturm ist hoch.\n"),
        "{vtt}"
    );
    assert_eq!(vtt.matches(" --> ").count(), 2, "{vtt}");
    let photo = e
        .idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.video.is_none() && r.label == "p1")
        .unwrap()
        .0;
    assert_eq!(
        get(&app, &format!("/captions/{photo}"), false).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&app, "/captions/999999", false).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, page) = get(&app, &format!("/v/p1/{photo}"), false).await;
    assert!(!page.contains("<track"), "{page}");
}

// The viewer's script ships what has no server side: every panel over the
// photo can move below it, and a correction is saved in place, keeping the
// other rows. (This checks the served script, not a browser run.)
// rivet: verifies REQ-WEB-023
#[tokio::test]
async fn the_viewer_script_moves_every_panel_below_and_saves_in_place() {
    let e = env();
    let app = web::router(e.state.clone());
    let (s, js) = get(&app, "/static/lichtbild.js", false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        js.contains(r#"viewer.querySelectorAll(".vbar .tpanel, .vbar .xpanel, .vbar .kpanel, .vbar dl.meta")"#),
        "every panel kind moves below"
    );
    assert!(js.contains(r#"var KINDS = { "fix/thing": 1, "fix/text": 1, "fix/off": 1 };"#));
    assert!(
        js.contains("var now = document.importNode(mine, true);\n          row.replaceWith(now);")
    );
    // A refused correction says why, not only a red line (REQ-FIX-003).
    assert!(js.contains("why.className = \"why\";"));
    assert!(js.contains("throw new Error(t || \"\")"));
    // On a phone the photo moves above an open sheet instead of staying
    // behind it (owner report on 0.9.0).
    let (_, css) = get(&app, "/static/lichtbild.css", false).await;
    assert!(
        css.contains(".viewer:has(.vbar details[open] > :is(.tpanel, .xpanel, .kpanel, .meta)) .stage {\n    flex: none; height: calc(42dvh - 56px);"),
        "the photo above the sheet"
    );
    let (_, page) = get(&app, &format!("/v/p1/{}", e.video), false).await;
    assert!(
        page.contains("<section class=\"under\" hidden></section>"),
        "{page}"
    );
    assert!(page.contains("<dl class=\"meta\">"), "{page}");
}

// Day or night: every page loads the theme script before its styles (so a
// choice does not flash), the menu has the switch, and the styles hold a
// night and a day that override the system's.
// rivet: verifies REQ-WEB-024
#[tokio::test]
async fn day_or_night_is_the_persons_choice() {
    let e = env();
    let app = web::router(e.state.clone());
    for uri in ["/", "/status", &format!("/v/p1/{}", e.video)] {
        let (_, page) = get(&app, uri, false).await;
        let theme = page.find("<script src=\"static/theme.js?v=").expect(uri);
        let css = page
            .find("<link rel=\"stylesheet\" href=\"static/lichtbild.css")
            .expect(uri);
        assert!(theme < css, "{uri}: theme after styles");
        assert!(
            !page[theme..theme + 60].contains("defer"),
            "{uri}: must run before drawing"
        );
        assert!(
            page.contains(
                "<button type=\"button\" class=\"theme\" data-now=\"auto\"><b>Day or night</b>"
            ),
            "{uri}"
        );
    }
    let (s, js) = get(&app, "/static/theme.js", false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(js.contains(r#"if (t === "day" || t === "night") document.documentElement.setAttribute("data-theme", t);"#));
    let (_, css) = get(&app, "/static/lichtbild.css", false).await;
    assert!(
        css.contains(":root:not([data-theme=\"day\"]) {"),
        "the system's night yields to day"
    );
    assert!(
        css.contains(":root[data-theme=\"night\"] {\n  --paper: #16140f;"),
        "night by choice"
    );
}

// Every answer is timed by its route, never its ids: the answer says how
// long it took (Server-Timing) and the status page shows the last hour's
// pages and pictures; slow ones are named by route.
// rivet: verifies REQ-WEB-025
#[tokio::test]
async fn answers_are_timed_by_route() {
    let e = env();
    let app = web::router(e.state.clone());
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v/p1/{}", e.video))
                .header("host", "localhost:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let st = r.headers()["server-timing"].to_str().unwrap().to_string();
    assert!(st.starts_with("app;dur="), "{st}");
    get(&app, &format!("/thumb/{}", e.video), false).await;
    get(&app, "/static/lichtbild.css", false).await;
    e.state
        .timings
        .record("/search", std::time::Duration::from_millis(2400));
    let (_, list) = get(&app, "/status", true).await;
    let at = list.find("<dt>Answers</dt><dd>").expect("answers row");
    let row = &list[at..at + list[at..].find("</dd>").unwrap()];
    // The viewer and the search; the status page is still being answered.
    assert!(row.contains("2 pages: half within"), "{row}");
    assert!(
        row.contains("slower than a second: <code>/search</code> 2.4 s (1×)"),
        "{row}"
    );
    assert!(row.contains("1 picture: half within"), "{row}");
    assert!(!row.contains(&format!("/{}", e.video)), "an id in {row}");
}

// A slideshow of the chosen pictures: only those, only photos, only of the
// timeline's libraries; the selection bar offers it.
// rivet: verifies REQ-WEB-026
// rivet: verifies REQ-WEB-027
#[tokio::test]
async fn a_slideshow_of_the_chosen_pictures() {
    let e = env();
    let app = web::router(e.state.clone());
    let all = e.idx.all().unwrap();
    let photo = |l: &str| {
        all.iter()
            .find(|(_, r)| r.label == l && r.video.is_none() && !r.missing && r.thumb == "ok")
            .unwrap()
            .0
    };
    let (p1, p2) = (photo("p1"), photo("p2"));
    let slides = |page: &str| -> Vec<u64> {
        page.split("<li data-src")
            .skip(1)
            .filter_map(|s| {
                let at = s.find("data-href=\"v/")? + 13;
                let rest = &s[at..];
                let rest = &rest[rest.find('/')? + 1..];
                rest[..rest.find('"')?].parse().ok()
            })
            .collect()
    };
    let (s, page) = get(&app, &format!("/show/p1?ids={p1},{p2},{}", e.video), false).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(slides(&page), vec![p1], "{page}");
    let (_, page) = get(&app, &format!("/show/all?ids={p2}%2C{p1}"), false).await;
    let mut both = slides(&page);
    both.sort();
    let mut want = vec![p1, p2];
    want.sort();
    assert_eq!(both, want, "{page}");
    let (_, page) = get(&app, "/show/p1?ids=nonsense", false).await;
    assert!(slides(&page).is_empty(), "{page}");
    let (_, js) = get(&app, "/static/lichtbild.js", false).await;
    assert!(js.contains(
        r#"window.location.href = "show/" + who + "?ids=" + encodeURIComponent(list.join(","));"#
    ));
    // On the TV (AirPlay, the owner's choice): the slideshow says how to
    // mirror the screen on Apple devices; a video offers AirPlay itself.
    let (_, page) = get(&app, &format!("/show/p1?ids={p1}"), false).await;
    assert!(
        page.contains(r#"class="stv" hidden"#) && page.contains("Screen Mirroring"),
        "{page}"
    );
    assert!(js.contains("var apple = /iPad|iPhone|Macintosh/.test(navigator.userAgent);"));
    let (_, page) = get(&app, &format!("/v/all/{}", e.video), false).await;
    assert!(page.contains(r#"x-webkit-airplay="allow""#), "{page}");
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

async fn get_navigating(app: &axum::Router, uri: &str) -> (StatusCode, String, String) {
    let r = Request::builder()
        .uri(uri)
        .header("host", "localhost:8080")
        .header("sec-fetch-mode", "navigate");
    let res = app
        .clone()
        .oneshot(r.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let s = res.status();
    let loc = res
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let b = res.into_body().collect().await.unwrap().to_bytes();
    (s, String::from_utf8_lossy(&b).into_owned(), loc)
}

// Review before v0.11.0: a refusal reached by a link is a page with a way
// back (scripts still get the bare reason), empty answers lead on, and the
// photo's keys and focus behave.
// rivet: verifies REQ-WEB-028
// rivet: verifies REQ-WEB-015
#[tokio::test]
async fn refusals_and_empty_answers_say_what_to_do_next() {
    let e = env();
    let app = web::router(e.state.clone());
    // A refusal: a page for a browser that navigated, the bare text for scripts.
    let (s, page, _) = get_navigating(&app, "/days/p1?from=gestern").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(page.contains("<h1>That did not work</h1>"), "{page}");
    assert!(page.contains("<p>Which days?</p>"), "{page}");
    assert!(page.contains("class=\"goback\""), "{page}");
    assert!(page.contains("<header>"), "{page}");
    let (s, bare) = get(&app, "/days/p1?from=gestern", false).await;
    assert_eq!((s, bare.as_str()), (StatusCode::BAD_REQUEST, "which days?"));
    let (s, page, _) = get_navigating(&app, "/t/nobody").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(page.contains("<h1>Not found</h1>"), "{page}");
    // Days without days: the timeline.
    let (s, _, loc) = get_navigating(&app, "/days/p1").await;
    assert!(s.is_redirection(), "{s}");
    assert_eq!(loc, "/t/p1");
    // A search without results is not a dead end.
    let (_, page) = get(&app, "/search?q=xyzzyqq", false).await;
    assert!(page.contains("0 results for"), "{page}");
    assert!(
        page.contains("See what there is: <a href=\"words\">Words</a>"),
        "{page}"
    );
    assert!(!page.contains("try fewer words"), "{page}");
    let (_, page) = get(&app, "/search?q=xyzzyqq+leuchtturm", false).await;
    assert!(
        page.contains("Every word must match; try fewer words."),
        "{page}"
    );
    // Words say "once" and "1 picture or video", not "1 times".
    let (_, page) = get(&app, "/words", false).await;
    assert!(
        page.contains("title=\"in 1 picture or video, 2 times\""),
        "{page}"
    );
    // Keys: Escape closes what is open before leaving the photo; a list box
    // is not a shortcut; the faces panel comes back; focus stays on a row.
    let (_, js) = get(&app, "/static/lichtbild.js", false).await;
    assert!(js.contains("if (document.querySelector(\"dialog[open]\")) return;\n      var open = document.querySelectorAll(\"details[open]\");\n      if (open.length) {\n        open.forEach(function (d) { d.open = false; });"));
    assert!(js.contains("e.target.closest(\"input, textarea, select, .suggest\")"));
    assert!(js.contains("var KEY = \"lichtbild.faces\";"));
    assert!(js.contains("var first = now.querySelector(\"button, input, a[href]\");"));
    assert!(js.contains("a.goback"));
    // On a phone the info sheet scrolls.
    let (_, css) = get(&app, "/static/lichtbild.css", false).await;
    assert!(css.contains("max-height: 60vh; max-height: calc(58dvh - 20px); overflow-y: auto; overscroll-behavior: contain;"));
}

// Recently added lists what the scans brought in under the day it was
// indexed, the newest day first; the menu and the home page lead there.
// rivet: verifies REQ-WEB-029
#[tokio::test]
async fn recently_added_shows_the_newest_files_first() {
    let e = env();
    let app = web::router(e.state.clone());
    let (s, page) = get(&app, "/recent", false).await;
    assert_eq!(s, StatusCode::OK);
    let today = chrono::Utc::now();
    let added = format!("Added {} ", today.format("%-d"));
    assert!(page.contains(&added), "{page}");
    assert!(page.contains(&today.format("%Y").to_string()));
    let (_, home) = get(&app, "/", false).await;
    assert!(home.contains("href=\"recent\""));

    // A file found by a later scan comes first, though taken long ago.
    let fx_root = e.state.roots.get("p1").unwrap().clone();
    std::fs::write(fx_root.join("a/IMG_19990101_101010.jpg"), jpeg(24, 16, 42)).unwrap();
    let cfg = config(&e._fx, &e._fx.data);
    scan::run(&cfg, &e.idx, &Options::default()).unwrap();
    let new = e
        .idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.rel == b"a/IMG_19990101_101010.jpg")
        .map(|(id, r)| {
            assert!(r.added > 0);
            id
        })
        .unwrap();
    let app = web::router(e.state.clone());
    let (_, page) = get(&app, "/recent", false).await;
    let labels = vec!["p1".to_string(), "p2".to_string()];
    let newest = e.idx.recent(&labels, 1, |_, _| true).unwrap();
    assert_eq!(newest[0].0.id, new, "the newest file first");
    // Indexed today like the rest, it sits in today's group, by date taken.
    let group = page.split("<section class=\"added\">").nth(1).unwrap();
    assert!(group.contains(&format!("id=\"i{new}\"")), "{group}");
    assert!(group.contains("1999"));
}

// A birthday is a real date with a year in the past, or month and day.
// rivet: verifies REQ-FACE-007
#[test]
fn birthdays_are_real_dates() {
    use lichtbild::web::parse_birthday as b;
    assert_eq!(b(""), Some(None));
    assert_eq!(b("03-04"), Some(Some("03-04".into())));
    assert_eq!(b("02-29"), Some(Some("02-29".into())));
    assert_eq!(b(" 1990-03-04 "), Some(Some("1990-03-04".into())));
    for bad in [
        "13-01",
        "02-30",
        "1990-02-30",
        "3000-01-01",
        "1700-01-01",
        "0304",
        "x",
        "1990/03/04",
    ] {
        assert_eq!(b(bad), None, "{bad}");
    }
}

// Photos that look blurry or nearly black are offered to put away, worst
// first; sharp ones and hidden ones are not; nothing hides by itself.
// rivet: verifies REQ-WEB-030
#[tokio::test]
async fn blurry_and_dark_photos_are_offered_for_review() {
    let e = env();
    let root = e.state.roots.get("p1").unwrap().clone();
    let save = |name: &str, f: &dyn Fn(u32, u32) -> u8| {
        image::GrayImage::from_fn(200, 150, |x, y| image::Luma([f(x, y)]))
            .save(root.join(name))
            .unwrap();
    };
    save("a/IMG_20230101_100000.jpg", &|_, _| 128);
    save("a/IMG_20230102_100000.jpg", &|x, y| ((x + y) % 3) as u8);
    save("a/IMG_20230103_100000.jpg", &|x, y| {
        if (x / 6 + y / 6) % 2 == 0 {
            40
        } else {
            210
        }
    });
    let cfg = config(&e._fx, &e._fx.data);
    scan::run(&cfg, &e.idx, &Options::default()).unwrap();
    let n = lichtbild::best::quality_pass(&cfg.data_dir, &e.idx).unwrap();
    assert!(n >= 3, "{n}");
    assert_eq!(
        lichtbild::best::quality_pass(&cfg.data_dir, &e.idx).unwrap(),
        0,
        "once"
    );
    let id = |rel: &str| {
        e.idx
            .all()
            .unwrap()
            .into_iter()
            .find(|(_, r)| r.rel == rel.as_bytes())
            .unwrap()
            .0
    };
    let (flat, dark, sharp) = (
        id("a/IMG_20230101_100000.jpg"),
        id("a/IMG_20230102_100000.jpg"),
        id("a/IMG_20230103_100000.jpg"),
    );
    let app = web::router(e.state.clone());
    let (s, page) = get(&app, "/review", false).await;
    assert_eq!(s, StatusCode::OK);
    let section = |title: &str| {
        page.split("<section class=\"added\">")
            .find(|p| p.starts_with(&format!("<h2>{title} ")))
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        section("Blurry").contains(&format!("id=\"i{flat}\"")),
        "{page}"
    );
    assert!(
        section("Too dark").contains(&format!("id=\"i{dark}\"")),
        "{page}"
    );
    assert!(!page.contains(&format!("id=\"i{sharp}\"")));
    let (_, menu) = get(&app, "/", false).await;
    assert!(menu.contains("href=\"review\""));

    // Hidden for this person (the operator, without login): off the list.
    e.idx.set_hidden("operator", flat, true).unwrap();
    let (_, page) = get(&app, "/review", false).await;
    assert!(!page.contains(&format!("id=\"i{flat}\"")));
}

// Many pictures close together in time are an event: listed on Events
// and at the top of the days they fall on; a few pictures are not.
// rivet: verifies REQ-TRIP-002
#[tokio::test]
async fn many_pictures_close_together_are_an_event() {
    let e = env();
    let root = e.state.roots.get("p1").unwrap().clone();
    std::fs::create_dir_all(root.join("party")).unwrap();
    for i in 0..14u8 {
        let date = format!("2023:08:12 19:{:02}:00", i * 4);
        std::fs::write(
            root.join(format!("party/IMG_{i:02}.jpg")),
            common::jpeg_with_exif(&date, "+02:00", None, 100 + i),
        )
        .unwrap();
    }
    let cfg = config(&e._fx, &e._fx.data);
    scan::run(&cfg, &e.idx, &Options::default()).unwrap();
    e.state.dups.rebuild(&e.idx).unwrap();
    let app = web::router(e.state.clone());
    let (s, page) = get(&app, "/events", false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("<i>14</i>"), "{page}");
    assert!(
        page.contains("days/all?from=2023-08-12&amp;to=2023-08-12"),
        "{page}"
    );
    assert!(page.contains("19:00–19:52"), "{page}");
    // The fixture's few other pictures make no event.
    assert_eq!(page.matches("class=\"thing\"").count(), 1, "{page}");
    let (_, days) = get(&app, "/days/all?from=2023-08-11&to=2023-08-13", false).await;
    assert!(days.contains("Events on these days"), "{days}");
    let (_, one) = get(&app, "/days/all?from=2023-08-12&to=2023-08-12", false).await;
    assert!(
        !one.contains("Events on these days"),
        "a single day is its own event"
    );
    let (_, home) = get(&app, "/", false).await;
    assert!(home.contains("href=\"events\""));
}
