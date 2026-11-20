//! Series in the timeline (REQ-WEB-012): photos taken one after another
//! that look alike show as one tile with a count. Synthetic pictures, with
//! made-up vectors standing in for what the vision model would store.

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
    // The series ends after its last item.
    let s_at = page.find(&format!("id=\"s{third}\"")).unwrap();
    let close = s_at + page[s_at..].find("</a></span>").unwrap();
    assert!(page[s_at..close].contains(&format!("id=\"i{first}\"")));
}
