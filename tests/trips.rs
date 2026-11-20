//! Trips (REQ-TRIP-001): days away from home, grouped and named by place.
//! Synthetic pictures at the synthetic places of tests/fixtures: home is
//! Ostdorf; Bergheim is about 90 km away, Westdorf about 40 km.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::{config, fixture, jpeg_with_exif, Dm};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

const OSTDORF: (Dm, Dm) = ((10, 31, 'N'), (20, 16, 'E'));
const BERGHEIM: (Dm, Dm) = ((11, 18, 'N'), (20, 6, 'E'));
const WESTDORF: (Dm, Dm) = ((10, 24, 'N'), (19, 54, 'E'));

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

// rivet: verifies REQ-TRIP-001
// rivet: verifies REQ-WEB-018
// rivet: verifies REQ-WEB-019
#[tokio::test]
async fn days_away_from_home_are_one_trip_named_by_place() {
    let fx = fixture();
    let dir = fx.root1.join("trips");
    std::fs::create_dir_all(&dir).unwrap();
    let shots: [(&str, Option<(Dm, Dm)>); 11] = [
        ("2021:06:01 10:00:00", Some(OSTDORF)),
        ("2021:06:08 10:00:00", Some(OSTDORF)),
        ("2021:06:20 10:00:00", Some(OSTDORF)),
        // Westdorf is near: no trip.
        ("2021:06:25 10:00:00", Some(WESTDORF)),
        ("2021:06:25 11:00:00", Some(WESTDORF)),
        ("2021:06:25 12:00:00", Some(WESTDORF)),
        // Bergheim, 3 and 5 July, a day without pictures between.
        ("2021:07:03 10:00:00", Some(BERGHEIM)),
        ("2021:07:03 18:00:00", Some(BERGHEIM)),
        ("2021:07:05 09:00:00", Some(BERGHEIM)),
        ("2021:07:05 09:30:00", None),
        ("2021:07:10 10:00:00", Some(OSTDORF)),
    ];
    for (i, (date, gps)) in shots.iter().enumerate() {
        std::fs::write(
            dir.join(format!("t{i:02}.jpg")),
            jpeg_with_exif(date, "+02:00", *gps, 40 + i as u8),
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
    let places = lichtbild::places::Places::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/places-cities.txt"),
        None,
        None,
    )
    .unwrap();
    let state = AppState::new(&cfg, idx.clone()).with_places(Some(places));
    let app = web::router(state.clone());
    let (s, page) = get(&app, "/trips").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page.matches("class=\"thing\"").count(), 1, "{page}");
    assert!(page.contains("<b>Bergheim"), "{page}");
    assert!(page.contains("<small>3–5 July 2021</small>"), "{page}");
    // Four items: the one without a position on 5 July counts too.
    assert!(page.contains("<i>4</i>"), "{page}");
    assert!(!page.contains("Westdorf"), "{page}");
    // The trip opens its own pictures, named, with a slideshow of them.
    let at = page
        .find("href=\"days/")
        .expect("a link to the trip's days")
        + 6;
    let link = page[at..at + page[at..].find('"').unwrap()].replace("&amp;", "&");
    assert!(
        link.starts_with("days/all?from=2021-07-03&to=2021-07-05&title=Bergheim"),
        "{link}"
    );
    let (s, trip) = get(&app, &format!("/{link}")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(trip.contains("<h1>Bergheim, ZZ</h1>"), "{trip}");
    assert_eq!(trip.matches("class=\"item").count(), 4, "{trip}");
    assert!(
        trip.contains("show/all?from=2021-07-03&amp;to=2021-07-05"),
        "{trip}"
    );
    let (_, show) = get(&app, "/show/all?from=2021-07-03&to=2021-07-05").await;
    assert_eq!(show.matches("<li data-src").count(), 4, "{show}");
    // One day, from its heading; and this day in every year.
    assert!(
        trip.contains("href=\"days/all?from=2021-07-05&amp;to=2021-07-05\""),
        "{trip}"
    );
    let (_, day) = get(&app, "/days/all?from=2021-07-05&to=2021-07-05").await;
    assert_eq!(day.matches("class=\"item").count(), 2, "{day}");
    assert!(day.contains("<h1>5 July 2021</h1>"), "{day}");
    // From a day to the days before and after that have pictures (4 July
    // has none), and any day by the date picker.
    assert!(
        day.contains(
            "<a rel=\"prev\" href=\"days/all?from=2021-07-03&amp;to=2021-07-03\">‹ 3 July 2021</a>"
        ),
        "{day}"
    );
    assert!(
        day.contains("<a rel=\"next\" href=\"days/all?from=2021-07-10&amp;to=2021-07-10\">10 July 2021 ›</a>"),
        "{day}"
    );
    assert!(day.contains("name=\"day\" value=\"2021-07-05\""), "{day}");
    let (s, picked) = get(&app, "/days/all?day=2021-07-05").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(picked.matches("class=\"item").count(), 2, "{picked}");
    assert!(picked.contains("<h1>5 July 2021</h1>"), "{picked}");
    let (s, _) = get(&app, "/days/all?day=5.7.2021").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, md) = get(&app, "/days/all?md=06-25").await;
    assert_eq!(md.matches("class=\"item").count(), 3, "{md}");
    let (s, _) = get(&app, "/days/all?from=2021-07-05&to=nonsense").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Only the viewer's items: p2 holds none of these, so no trips.
    let all = idx.all().unwrap();
    let p2: Vec<_> = all
        .iter()
        .filter(|(_, r)| r.label == "p2")
        .cloned()
        .collect();
    let pos = state.positions.get(&idx).unwrap();
    assert!(lichtbild::trips::find(&p2, &pos, state.places.as_deref()).is_empty());
    // All libraries: still the one trip.
    let trips = lichtbild::trips::find(&all, &pos, state.places.as_deref());
    assert_eq!(trips.len(), 1);
    assert_eq!(trips[0].places, ["Bergheim, ZZ"]);
}
