//! Placing pictures without a recorded position (REQ-PLACE-002, DD-022):
//! estimates from the neighbours in time, and positions set by hand.
//! Synthetic pictures at the synthetic places of tests/fixtures.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::{config, fixture, jpeg_with_exif};
use lichtbild::config::MapFiles;
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: &str,
    site: &str,
) -> (StatusCode, String) {
    let mut r = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:8080")
        .header("sec-fetch-site", site);
    if method == "POST" {
        r = r.header("content-type", "application/x-www-form-urlencoded");
    }
    let res = app
        .clone()
        .oneshot(r.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let s = res.status();
    let b = res.into_body().collect().await.unwrap().to_bytes();
    (s, String::from_utf8_lossy(&b).into_owned())
}

async fn get(app: &axum::Router, uri: &str) -> String {
    let (s, b) = call(app, "GET", uri, "", "same-origin").await;
    assert_eq!(s, StatusCode::OK, "{uri}: {b}");
    b
}

fn synthetic_places() -> lichtbild::places::Places {
    let f = |n: &str| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(n)
    };
    lichtbild::places::Places::load(&f("places-cities.txt"), None, None).unwrap()
}

struct Env {
    _fx: common::Fixture,
    _map: tempfile::TempDir,
    idx: Arc<Index>,
    state: AppState,
}

impl Env {
    fn id(&self, label: &str, rel: &str) -> u64 {
        self.idx
            .all()
            .unwrap()
            .into_iter()
            .find(|(_, r)| r.label == label && r.rel == rel.as_bytes())
            .unwrap_or_else(|| panic!("{label}/{rel}"))
            .0
    }
}

/// The common fixture (p1's a/IMG_exif.jpg was taken 2021-06-15 10:11:12
/// at 10.5°N 20.25°E, beside the synthetic Ostdorf), plus pictures that
/// record no position: one 9 minutes later in p1, one a minute later in
/// p2, and two in the afternoon in p1. And a synthetic map file.
fn env() -> Env {
    let fx = fixture();
    let w = |p: std::path::PathBuf, date: &str, seed: u8| {
        std::fs::write(p, jpeg_with_exif(date, "+02:00", None, seed)).unwrap()
    };
    w(fx.root1.join("a/near.jpg"), "2021:06:15 10:20:12", 11);
    w(fx.root2.join("other.jpg"), "2021:06:15 10:12:12", 12);
    w(fx.root1.join("b/later.jpg"), "2021:06:15 15:00:00", 13);
    w(fx.root1.join("b/later2.jpg"), "2021:06:15 15:05:00", 14);
    let map = tempfile::tempdir().unwrap();
    let pm = map.path().join("world.pmtiles");
    let mut b = b"PMTiles\x03".to_vec();
    b.extend((8..40_000usize).map(|i| (i * 31 % 251) as u8));
    std::fs::write(&pm, b).unwrap();
    let mut cfg = config(&fx, &fx.data);
    cfg.map = lichtbild::map::check_files(&MapFiles {
        pmtiles: Some(pm),
        assets: None,
    })
    .unwrap();
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    assert!(
        scan::run(&cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    let state = AppState::new(&cfg, idx.clone()).with_places(Some(synthetic_places()));
    state.search.rebuild(&idx, state.places.as_deref()).unwrap();
    Env {
        _fx: fx,
        _map: map,
        idx,
        state,
    }
}

// A picture minutes after one with a position is shown there, marked as
// estimated, on the map, in the viewer, in search and on the status page;
// one in another library is not.
// rivet: verifies REQ-PLACE-002
#[tokio::test]
async fn neighbours_in_time_place_a_picture_as_estimated() {
    let e = env();
    let app = web::router(e.state.clone());
    let near = e.id("p1", "a/near.jpg");
    let other = e.id("p2", "other.jpg");

    let json = get(&app, "/map/clusters?z=18&bbox=20,10,21,11").await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let items: Vec<&serde_json::Value> = v["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["items"].as_array().cloned().unwrap_or_default())
        .collect::<Vec<_>>()
        .leak()
        .iter()
        .collect();
    let est = items.iter().find(|i| i["id"] == near).expect("on the map");
    assert_eq!(est["est_m"], 100 + 9 * 60, "{est}");
    assert!(items.iter().all(|i| i["id"] != other), "{json}");
    let exif = e.id("p1", "a/IMG_exif.jpg");
    let rec = items.iter().find(|i| i["id"] == exif).unwrap();
    assert!(
        rec.get("est_m").is_none(),
        "a recorded one is not estimated"
    );

    let page = get(&app, &format!("/v/p1/{near}")).await;
    assert!(page.contains("about near Ostdorf"), "{page}");
    assert!(
        page.contains("estimated, within 640 m: from a picture 9 minutes away"),
        "{page}"
    );
    assert!(page.contains(&format!(
        r#"<a href="map?place={near}">Place it yourself</a>"#
    )));
    let page = get(&app, &format!("/v/p2/{other}")).await;
    assert!(page.contains("no position recorded"), "{page}");

    let hits = get(&app, "/search?q=ostdorf").await;
    assert!(hits.contains(&format!("/{near}\"")), "{hits}");
    // The result names the estimated place, as the recorded one does.
    let tile = &hits[hits.find(&format!("/{near}\"")).unwrap()..];
    let tile = &tile[..tile.find("</div></div>").unwrap()];
    assert!(tile.contains(" · Ostdorf"), "{tile}");

    let status = get(&app, "/status").await;
    assert!(
        status.contains(
            "1 recorded, 1 estimated (0 between two pictures, 1 from one nearby), 0 set by hand"
        ),
        "{status}"
    );
}

// Placing by hand: only from this site, only a picture without a recorded
// position, only valid coordinates; it places its neighbours too, is
// found by the place's name, and can be forgotten again.
// rivet: verifies REQ-PLACE-002
#[tokio::test]
async fn a_picture_placed_by_hand_places_its_neighbours() {
    let e = env();
    let app = web::router(e.state.clone());
    let later = e.id("p1", "b/later.jpg");
    let later2 = e.id("p1", "b/later2.jpg");
    let exif = e.id("p1", "a/IMG_exif.jpg");
    let at_bergheim = "lat=11.30&lon=20.10";

    let page = get(&app, &format!("/map?place={later}")).await;
    assert!(
        page.contains(r#"<form class="placebar" id="placeform""#),
        "{page}"
    );
    let (s, _) = call(
        &app,
        "GET",
        &format!("/map?place={exif}"),
        "",
        "same-origin",
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "its file records a position");

    let post = |uri: String, body: &'static str, site: &'static str| {
        let app = app.clone();
        async move { call(&app, "POST", &uri, body, site).await.0 }
    };
    assert_eq!(
        post(format!("/place/{later}"), at_bergheim, "cross-site").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(format!("/place/{exif}"), at_bergheim, "same-origin").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(format!("/place/{later}"), "lat=95&lon=20", "same-origin").await,
        StatusCode::BAD_REQUEST
    );
    assert!(e.idx.placed().unwrap().is_empty());

    assert_eq!(
        post(format!("/place/{later}"), at_bergheim, "same-origin").await,
        StatusCode::SEE_OTHER
    );
    let page = get(&app, &format!("/v/p1/{later}")).await;
    assert!(page.contains("near Bergheim"), "{page}");
    assert!(page.contains("set by hand"), "{page}");
    assert!(
        page.contains(&format!(r#"action="unplace/{later}""#)),
        "{page}"
    );
    let page = get(&app, &format!("/v/p1/{later2}")).await;
    assert!(page.contains("from a picture 5 minutes away"), "{page}");

    e.state
        .search
        .rebuild(&e.idx, e.state.places.as_deref())
        .unwrap();
    let hits = get(&app, "/search?q=bergheim").await;
    assert!(hits.contains(&format!("/{later}\"")), "{hits}");
    assert!(hits.contains(&format!("/{later2}\"")), "{hits}");

    assert_eq!(
        post(format!("/unplace/{later}"), "", "same-origin").await,
        StatusCode::SEE_OTHER
    );
    let page = get(&app, &format!("/v/p1/{later2}")).await;
    assert!(page.contains("no position recorded"), "{page}");
}
