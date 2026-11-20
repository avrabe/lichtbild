//! Series in the timeline (REQ-WEB-012): photos taken one after another
//! that look alike show as one tile with a count; the slideshow
//! (REQ-WEB-013) shows one photo of each. Synthetic pictures, with made-up
//! vectors standing in for what the vision model would store.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::{config, fixture, jpeg};
use lichtbild::embed::{quantize, DIM};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

const VIDEO: &[u8] = include_bytes!("fixtures/video/synthetic-h264-160x120.mp4");

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String) {
    let r = Request::builder()
        .uri(uri)
        .header("host", "localhost:8080")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(r).await.unwrap();
    let s = res.status();
    let b = res.into_body().collect().await.unwrap().to_bytes();
    (s, String::from_utf8_lossy(&b).into_owned())
}

fn look(v: Vec<i8>) -> lichtbild::vision::Look {
    lichtbild::vision::Look {
        text: lichtbild::ocr::Text {
            state: "none".into(),
            model: "test".into(),
            lines: vec![],
            docs: vec![],
        },
        objects: vec![],
        vector: v,
    }
}

/// Replace the thumbnail of `hash`: a sharp checkerboard or a flat grey,
/// so which shot of a series is best is known (REQ-WEB-021).
fn thumb(data_dir: &std::path::Path, hash: &str, sharp: bool) {
    let img = image::GrayImage::from_fn(64, 48, |x, y| {
        image::Luma([if sharp && (x / 4 + y / 4) % 2 == 0 {
            60
        } else {
            140
        }])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
        .encode_image(&img)
        .unwrap();
    std::fs::write(scan::thumb_path(data_dir, hash), out).unwrap();
}

/// The tile of item `id`: its opening tag.
fn tile(page: &str, id: u64) -> &str {
    let at = page.find(&format!("id=\"i{id}\"")).expect("tile");
    let start = page[..at].rfind("<a ").unwrap();
    &page[start..at]
}

// rivet: verifies REQ-WEB-012
#[tokio::test]
async fn photos_taken_one_after_another_that_look_alike_are_one_series() {
    let fx = fixture();
    // The fixture holds a/IMG_20200102_030405.jpg; three more within
    // seconds that look the same, one six minutes later that looks the
    // same, and two at five o'clock taken five seconds apart that do not.
    let more = [
        ("IMG_20200102_030410.jpg", 31),
        ("IMG_20200102_030430.jpg", 32),
        ("IMG_20200102_031000.jpg", 33),
        ("IMG_20200102_050000.jpg", 34),
        ("IMG_20200102_050005.jpg", 35),
    ];
    for (name, seed) in more {
        std::fs::write(fx.root1.join("a").join(name), jpeg(40, 30, seed)).unwrap();
    }
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    assert!(
        scan::run(&cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );

    let a: Vec<f32> = (0..DIM).map(|i| (i as f32 * 0.37).sin()).collect();
    let b: Vec<f32> = (0..DIM).map(|i| (i as f32 * 1.91).cos()).collect();
    let id_of = |stamp: &str| {
        let (id, rec) = idx
            .all()
            .unwrap()
            .into_iter()
            .find(|(_, r)| r.taken_local == stamp)
            .unwrap_or_else(|| panic!("{stamp}"));
        (id, rec.hash)
    };
    let mut ids = Vec::new();
    let mut hashes = Vec::new();
    for (stamp, v) in [
        ("2020-01-02T03:04:05", &a),
        ("2020-01-02T03:04:10", &a),
        ("2020-01-02T03:04:30", &a),
        ("2020-01-02T03:10:00", &a),
        ("2020-01-02T05:00:00", &b),
        ("2020-01-02T05:00:05", &a),
    ] {
        let (id, hash) = id_of(stamp);
        idx.set_look(&hash, "test", &look(quantize(v))).unwrap();
        ids.push(id);
        hashes.push(hash);
    }
    // The newest of the series is the sharpest.
    for (i, h) in hashes.iter().enumerate() {
        thumb(&cfg.data_dir, h, i == 2);
    }
    let [first, second, third, later, other, after_other] = ids[..] else {
        unreachable!()
    };

    // Without vision there are no series.
    let mut state = AppState::new(&cfg, idx.clone());
    let app = web::router(state.clone());
    let (s, page) = get(&app, "/t/p1").await;
    assert_eq!(s, StatusCode::OK);
    assert!(!page.contains("class=\"series\""), "{page}");

    state.vision_on = true;
    state.space.rebuild(&idx).unwrap();
    let app = web::router(state);
    let (_, page) = get(&app, "/t/p1").await;
    // Newest first: the series is led by the third and holds three.
    assert!(
        page.contains(&format!("<span class=\"series\" id=\"s{third}\">")),
        "{page}"
    );
    let lead = &page[page.find(&format!("id=\"s{third}\"")).unwrap()..];
    let lead = &lead[..lead.find("</a>").unwrap()];
    assert!(lead.contains("aria-expanded=\"false\""), "{lead}");
    assert!(lead.contains(">3</span>"), "{lead}");
    for id in [second, first] {
        assert!(tile(&page, id).contains(" later\""), "{id}: {page}");
    }
    // Six minutes later, a different look, or a different look before:
    // each alone.
    for id in [later, other, after_other, third] {
        assert!(!tile(&page, id).contains(" later\""), "{id}: {page}");
    }
    assert_eq!(page.matches("class=\"series\"").count(), 1, "{page}");
    // The series ends after its last item, with the offer to keep the best.
    let s_at = page.find(&format!("id=\"s{third}\"")).unwrap();
    let close = s_at + page[s_at..].find("</a><button").unwrap();
    assert!(page[s_at..close].contains(&format!("id=\"i{first}\"")));
    assert!(
        page.contains(&format!("class=\"keepbest\" data-ids=\"{second},{first}\" data-best=\"{third}\">Keep the best, hide the other 2</button></span>")),
        "{page}"
    );
}

// The sharpest shot leads its series, the others after it in their order,
// and the series offers to keep it and hide the others; a favourite leads
// whatever its sharpness.
// rivet: verifies REQ-WEB-021
#[tokio::test]
async fn the_best_shot_leads_its_series() {
    let fx = fixture();
    for (name, seed) in [
        ("IMG_20200102_030410.jpg", 31),
        ("IMG_20200102_030430.jpg", 32),
    ] {
        std::fs::write(fx.root1.join("a").join(name), jpeg(40, 30, seed)).unwrap();
    }
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    assert!(
        scan::run(&cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    let a: Vec<f32> = (0..DIM).map(|i| (i as f32 * 0.37).sin()).collect();
    let all = idx.all().unwrap();
    let at = |stamp: &str| {
        let (id, r) = all.iter().find(|(_, r)| r.taken_local == stamp).unwrap();
        (*id, r.hash.clone())
    };
    let (first, second, third) = (
        at("2020-01-02T03:04:05"),
        at("2020-01-02T03:04:10"),
        at("2020-01-02T03:04:30"),
    );
    for (_, h) in [&first, &second, &third] {
        idx.set_look(h, "test", &look(quantize(&a))).unwrap();
    }
    // The oldest is the sharp one.
    thumb(&cfg.data_dir, &first.1, true);
    thumb(&cfg.data_dir, &second.1, false);
    thumb(&cfg.data_dir, &third.1, false);
    let mut state = AppState::new(&cfg, idx.clone());
    state.vision_on = true;
    state.space.rebuild(&idx).unwrap();
    let app = web::router(state.clone());
    let order = |page: &str| -> Vec<u64> {
        page.split("id=\"i")
            .skip(1)
            .filter_map(|s| s.split('"').next()?.parse().ok())
            .collect()
    };
    let (s, page) = get(&app, "/t/p1").await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        page.contains(&format!("<span class=\"series\" id=\"s{}\">", first.0)),
        "{page}"
    );
    let ids = order(&page);
    let pos = |id: u64| ids.iter().position(|&x| x == id).unwrap();
    assert!(
        pos(first.0) < pos(third.0) && pos(third.0) < pos(second.0),
        "{ids:?}"
    );
    assert!(
        page.contains(&format!(
            "data-ids=\"{},{}\" data-best=\"{}\"",
            third.0, second.0, first.0
        )),
        "{page}"
    );
    // The same on the day's own page.
    let (_, day) = get(&app, "/days/p1?day=2020-01-02").await;
    assert!(
        day.contains(&format!("<span class=\"series\" id=\"s{}\">", first.0)),
        "{day}"
    );

    // A favourite leads, though blurred.
    idx.set_fav("operator", second.0, true).unwrap();
    let (_, page) = get(&app, "/t/p1").await;
    assert!(
        page.contains(&format!("<span class=\"series\" id=\"s{}\">", second.0)),
        "{page}"
    );
}

// rivet: verifies REQ-WEB-013
#[tokio::test]
async fn a_slideshow_shows_one_photo_of_each_series_from_where_it_starts() {
    let fx = fixture();
    for (name, seed) in [
        ("IMG_20200102_030410.jpg", 31),
        ("IMG_20200102_030430.jpg", 32),
        ("IMG_20200102_031000.jpg", 33),
    ] {
        std::fs::write(fx.root1.join("a").join(name), jpeg(40, 30, seed)).unwrap();
    }
    std::fs::write(fx.root1.join("a/clip.mp4"), VIDEO).unwrap();
    let cfg = config(&fx, &fx.data);
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    let opts = Options {
        keyframe_decoder: Some(env!("CARGO_BIN_EXE_lichtbild").into()),
        ..Options::default()
    };
    assert!(scan::run(&cfg, &idx, &opts).unwrap().reconciled);
    let a: Vec<f32> = (0..DIM).map(|i| (i as f32 * 0.37).sin()).collect();
    let all = idx.all().unwrap();
    let id_at = |stamp: &str| all.iter().find(|(_, r)| r.taken_local == stamp).unwrap();
    for stamp in [
        "2020-01-02T03:04:05",
        "2020-01-02T03:04:10",
        "2020-01-02T03:04:30",
        "2020-01-02T03:10:00",
    ] {
        idx.set_look(&id_at(stamp).1.hash, "test", &look(quantize(&a)))
            .unwrap();
    }
    let (first, second, third, later) = (
        id_at("2020-01-02T03:04:05").0,
        id_at("2020-01-02T03:04:10").0,
        id_at("2020-01-02T03:04:30").0,
        id_at("2020-01-02T03:10:00").0,
    );
    let video = all.iter().find(|(_, r)| r.video.is_some()).unwrap().0;
    let p2 = all.iter().find(|(_, r)| r.label == "p2").unwrap().0;
    let mut state = AppState::new(&cfg, idx.clone());
    state.vision_on = true;
    state.space.rebuild(&idx).unwrap();
    let app = web::router(state);

    let slides = |page: &str| -> Vec<u64> {
        page.split("data-href=\"v/p1/")
            .skip(1)
            .filter_map(|s| s.split('"').next()?.parse().ok())
            .collect()
    };
    let (s, page) = get(&app, "/show/p1").await;
    assert_eq!(s, StatusCode::OK);
    let all_slides = slides(&page);
    assert!(
        all_slides.contains(&third) && all_slides.contains(&later),
        "{page}"
    );
    for id in [second, first, video] {
        assert!(!all_slides.contains(&id), "{id}: {page}");
    }
    assert!(
        page.contains("data-when=\"2 January 2020, 03:04\""),
        "{page}"
    );
    // From a photo on: it comes first, then older ones.
    let (_, page) = get(&app, &format!("/show/p1?at={later}")).await;
    let from = slides(&page);
    assert_eq!(from.first(), Some(&later), "{page}");
    assert_eq!(from.get(1), Some(&third), "{page}");
    // A photo of another timeline starts nothing.
    let (_, page) = get(&app, &format!("/show/p1?at={p2}")).await;
    assert!(
        slides(&page).is_empty() && page.contains("No photos to show"),
        "{page}"
    );
    // The viewer offers it from the photo shown.
    let (_, page) = get(&app, &format!("/v/p1/{later}")).await;
    assert!(
        page.contains(&format!("href=\"show/p1?at={later}\"")),
        "{page}"
    );
}
