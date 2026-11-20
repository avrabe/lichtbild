//! Duplicates (REQ-DUP-001): exact copies and the same picture in another
//! file are shown once, as the best copy the viewer may see; a burst of
//! camera shots is not a duplicate. Synthetic pictures only.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::{picture, with_exif};
use lichtbild::config::{Limits, Settings};
use lichtbild::dups;
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

struct Env {
    _tmp: tempfile::TempDir,
    cfg: lichtbild::config::Config,
}

const GPS: Option<(common::Dm, common::Dm)> = Some(((10, 31, 'N'), (20, 16, 'E')));

/// p1: a camera photo, a second shot of its burst, its messenger copy
/// (half size, no EXIF), two flat black pictures, another photo.
/// p2: an exact copy of the camera photo.
fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for d in ["lib/p1/a", "lib/p1/w", "lib/p1/b", "lib/p2", "data"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    let w = |p: &str, b: &[u8]| std::fs::write(base.join(p), b).unwrap();
    let cam = with_exif(
        picture(640, 480, 10, 0.0),
        "2024:07:01 10:00:00",
        "+02:00",
        GPS,
    );
    w("lib/p1/a/IMG_20240701_100000.jpg", &cam);
    w(
        "lib/p1/a/IMG_20240701_100001.jpg",
        &with_exif(
            picture(640, 480, 10, 0.02),
            "2024:07:01 10:00:01",
            "+02:00",
            GPS,
        ),
    );
    w(
        "lib/p1/w/IMG-20240701-WA0001.jpg",
        &picture(320, 240, 10, 0.0),
    );
    w("lib/p1/b/black-1.jpg", &black(400, 300));
    w("lib/p1/b/black-2.jpg", &black(200, 150));
    w("lib/p1/a/other.jpg", &picture(640, 480, 200, 0.3));
    w("lib/p2/IMG_20240701_100000.jpg", &cam);
    // The same file twice in p1 too (a folder exported twice), later.
    std::thread::sleep(std::time::Duration::from_millis(20));
    w("lib/p1/w/IMG_20240701_100000 (1).jpg", &cam);
    let cfg = Settings {
        roots: vec![
            ("p1".into(), base.join("lib/p1")),
            ("p2".into(), base.join("lib/p2")),
        ],
        data_dir: Some(base.join("data")),
        workers: Some(2),
        limits: Limits {
            max_pixels: 4_000_000,
            ..Limits::default()
        },
        ..Settings::default()
    }
    .resolve()
    .unwrap();
    Env { _tmp: tmp, cfg }
}

fn black(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::new(w, h);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&img)
        .unwrap();
    out
}

fn id_of(idx: &Index, label: &str, rel: &str) -> u64 {
    idx.all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.label == label && r.rel == rel.as_bytes())
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("{label}:{rel} not indexed"))
}

struct Ids {
    cam: u64,
    burst: u64,
    wa: u64,
    copy: u64,
    black1: u64,
    black2: u64,
    other: u64,
    dup1: u64,
}

fn ids(idx: &Index) -> Ids {
    Ids {
        cam: id_of(idx, "p1", "a/IMG_20240701_100000.jpg"),
        burst: id_of(idx, "p1", "a/IMG_20240701_100001.jpg"),
        wa: id_of(idx, "p1", "w/IMG-20240701-WA0001.jpg"),
        copy: id_of(idx, "p2", "IMG_20240701_100000.jpg"),
        black1: id_of(idx, "p1", "b/black-1.jpg"),
        black2: id_of(idx, "p1", "b/black-2.jpg"),
        other: id_of(idx, "p1", "a/other.jpg"),
        dup1: id_of(idx, "p1", "w/IMG_20240701_100000 (1).jpg"),
    }
}

fn grouped(e: &Env) -> (Arc<Index>, Arc<dups::Dups>, dups::Report) {
    let idx = Arc::new(Index::open(&e.cfg.data_dir).unwrap());
    let rep = scan::run(&e.cfg, &idx, &Options::default()).unwrap();
    assert!(rep.reconciled, "{:?}", rep.problems);
    assert_eq!(
        dups::fingerprint_pass(&e.cfg.data_dir, &idx).unwrap(),
        6,
        "one fingerprint per content (the exact copies share one)"
    );
    // Nothing new on a second pass.
    assert_eq!(dups::fingerprint_pass(&e.cfg.data_dir, &idx).unwrap(), 0);
    let d = Arc::new(dups::Dups::default());
    let r = d.rebuild(&idx).unwrap();
    (idx, d, r)
}

// The exact copy and the messenger copy join the camera photo; the burst
// shot, the flat pictures and the other photo stay apart.
// rivet: verifies REQ-DUP-001
#[test]
fn copies_are_grouped_and_bursts_are_not() {
    let e = env();
    let (idx, d, r) = grouped(&e);
    let i = ids(&idx);
    assert_eq!(
        (r.groups, r.items, r.exact_pairs, r.near_pairs),
        (1, 4, 2, 1),
        "{r:?}"
    );
    let both = ["p1".to_string(), "p2".to_string()];
    let none = std::collections::HashSet::new();
    let mut g = d.copies(i.cam, &both);
    g.sort_unstable();
    let mut want = vec![i.cam, i.wa, i.copy, i.dup1];
    want.sort_unstable();
    assert_eq!(g, want);
    for alone in [i.burst, i.black1, i.black2, i.other] {
        assert!(d.copies(alone, &both).is_empty(), "{alone} grouped");
        assert_eq!(
            d.shown(alone, &both, &none),
            dups::Shown {
                shown: true,
                copies: 1
            }
        );
    }
    // The camera original beats its messenger copy.
    let p1 = ["p1".to_string()];
    assert!(d.shown(i.cam, &p1, &none).shown && !d.shown(i.wa, &p1, &none).shown);
    assert_eq!(d.shown(i.cam, &p1, &none).copies, 3);
    // Who sees only p2 sees its copy, and nothing of p1.
    let p2 = ["p2".to_string()];
    assert_eq!(
        d.shown(i.copy, &p2, &none),
        dups::Shown {
            shown: true,
            copies: 1
        }
    );
    assert_eq!(d.copies(i.copy, &p2), vec![i.copy]);
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("host", "localhost:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let s = res.status();
    let b = res.into_body().collect().await.unwrap().to_bytes();
    (s, String::from_utf8_lossy(&b).into_owned())
}

fn tiles(page: &str) -> Vec<u64> {
    page.split("id=\"i")
        .skip(1)
        .filter_map(|s| s.split('"').next()?.parse().ok())
        .collect()
}

// Timeline, search and viewer show one copy, scoped to the viewer.
// rivet: verifies REQ-DUP-001
#[tokio::test]
async fn the_timeline_search_and_viewer_show_one_copy() {
    let e = env();
    let (idx, d, _) = grouped(&e);
    let i = ids(&idx);
    let mut state = AppState::new(&e.cfg, idx.clone());
    state.dups = d;
    state.search.rebuild(&idx, None).unwrap();
    let app = web::router(state);

    let (_, page) = get(&app, "/t/p1").await;
    let t = tiles(&page);
    assert!(t.contains(&i.cam) && !t.contains(&i.wa), "{t:?}");
    assert!(t.contains(&i.burst) && t.contains(&i.other));
    assert_eq!(t.len(), 5, "{t:?}");
    assert!(page.contains("title=\"3 copies\">×3</span>"), "{page}");

    let (_, page) = get(&app, "/t/p2").await;
    assert_eq!(tiles(&page), vec![i.copy]);
    assert!(!page.contains("copies\">×"), "{page}");

    let (_, page) = get(&app, "/t/all").await;
    let t = tiles(&page);
    assert_eq!(
        t.iter()
            .filter(|x| [i.cam, i.wa, i.copy, i.dup1].contains(x))
            .count(),
        1,
        "{t:?}"
    );
    assert!(page.contains("title=\"4 copies\">×4</span>"), "{page}");

    // A search hit on the messenger copy shows the camera photo.
    let (_, page) = get(&app, "/search?q=WA0001&who=p1").await;
    assert!(page.contains(&format!("href=\"v/p1/{}\"", i.cam)), "{page}");
    assert!(!page.contains(&format!("v/p1/{}", i.wa)), "{page}");

    // The viewer lists the copies the viewer may see, and no others.
    let (s, page) = get(&app, &format!("/v/p1/{}", i.cam)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("class=\"copies\""), "{page}");
    assert!(page.contains("IMG-20240701-WA0001.jpg"), "{page}");
    assert!(!page.contains("p2 ·"), "out of scope: {page}");
}

async fn post(app: &axum::Router, uri: &str, back: &str, site: &str) -> (StatusCode, String) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("host", "localhost:8080")
                .header("sec-fetch-site", site)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("back={back}")))
                .unwrap(),
        )
        .await
        .unwrap();
    let s = res.status();
    let loc = res
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (s, loc)
}

// Hiding the shown copy shows the next one; "keep this one" chooses the
// copy; the hidden list shows them again; another site cannot hide.
// rivet: verifies REQ-DUP-002
#[tokio::test]
async fn hide_and_keep_choose_what_is_shown() {
    let e = env();
    let (idx, d, _) = grouped(&e);
    let i = ids(&idx);
    let mut state = AppState::new(&e.cfg, idx.clone());
    state.dups = d;
    let app = web::router(state);
    let shown = |page: &str| tiles(page);

    // Another site's form changes nothing.
    let (s, _) = post(&app, &format!("/hide/{}", i.cam), "t/p1", "cross-site").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(idx.hidden("operator").unwrap().is_empty());

    let (s, loc) = post(&app, &format!("/hide/{}", i.cam), "t/p1", "same-origin").await;
    assert!(s.is_redirection(), "{s}");
    assert_eq!(loc, "/t/p1");
    let (_, page) = get(&app, "/t/p1").await;
    let t = shown(&page);
    assert!(!t.contains(&i.cam), "{t:?}");
    assert!(t.contains(&i.dup1), "the next copy shows: {t:?}");
    // The hidden item is listed, and shows again.
    let (_, page) = get(&app, "/hidden").await;
    assert!(
        page.contains(&format!("action=\"unhide/{}\"", i.cam)),
        "{page}"
    );
    post(&app, &format!("/unhide/{}", i.cam), "hidden", "same-origin").await;
    assert!(shown(&get(&app, "/t/p1").await.1).contains(&i.cam));

    // Keep the messenger copy: it shows, the others are hidden.
    post(
        &app,
        &format!("/keep/{}", i.wa),
        &format!("v/p1/{}", i.wa),
        "same-origin",
    )
    .await;
    let t = shown(&get(&app, "/t/p1").await.1);
    assert!(
        t.contains(&i.wa) && !t.contains(&i.cam) && !t.contains(&i.dup1),
        "{t:?}"
    );
    // The viewer offers to show a hidden item again.
    let (_, page) = get(&app, &format!("/v/p1/{}", i.cam)).await;
    assert!(
        page.contains(&format!("action=\"unhide/{}\"", i.cam)),
        "{page}"
    );
    // A "back" to another site goes nowhere else.
    let (_, loc) = post(
        &app,
        &format!("/hide/{}", i.other),
        "https://evil.example/",
        "same-origin",
    )
    .await;
    assert!(loc.starts_with('/') && !loc.contains("evil"), "{loc}");
}

// The cleanup list names exact duplicate files in one's own library, and
// the space they take; it changes nothing.
// rivet: verifies REQ-DUP-003
#[tokio::test]
async fn the_cleanup_list_names_exact_duplicate_files() {
    let e = env();
    let (idx, d, _) = grouped(&e);
    let mut state = AppState::new(&e.cfg, idx.clone());
    state.dups = d;
    let app = web::router(state);
    let (s, page) = get(&app, "/duplicates").await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("1 group of identical files"), "{page}");
    assert!(page.contains("1 extra copy"), "{page}");
    assert!(page.contains("w/IMG_20240701_100000 (1).jpg"), "{page}");
    // The messenger copy is not the same file and is not listed.
    assert!(!page.contains("WA0001"), "{page}");
    let (s, csv) = get(&app, "/duplicates.csv").await;
    assert_eq!(s, StatusCode::OK);
    let rows: Vec<&str> = csv.lines().collect();
    assert_eq!(rows[0], "library,keep,duplicate,bytes");
    assert_eq!(rows.len(), 2, "{csv}");
    assert!(rows[1].starts_with("\"p1\","), "{csv}");
    assert!(
        rows[1].contains("\"w/IMG_20240701_100000 (1).jpg\""),
        "{csv}"
    );
}

// The hidden list is ordered by date or name, narrowed by a word, and
// several are shown again at once.
// rivet: verifies REQ-WEB-020
#[tokio::test]
async fn the_hidden_list_sorts_narrows_and_shows_several_again() {
    let e = env();
    let (idx, d, _) = grouped(&e);
    let i = ids(&idx);
    for id in [i.black1, i.black2, i.other, i.burst] {
        idx.set_hidden("operator", id, true).unwrap();
    }
    let mut state = AppState::new(&e.cfg, idx.clone());
    state.dups = d;
    let app = web::router(state);
    let order = |page: &str| -> Vec<u64> {
        page.match_indices("action=\"unhide/")
            .map(|(at, m)| {
                let rest = &page[at + m.len()..];
                rest[..rest.find('"').unwrap()].parse().unwrap()
            })
            .collect()
    };

    let (s, page) = get(&app, "/hidden?sort=name").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        order(&page),
        vec![i.black1, i.black2, i.burst, i.other],
        "{page}"
    );
    assert!(page.contains("<option value=\"name\" selected>"), "{page}");
    assert!(
        page.contains("title=\"a/other.jpg\">other.jpg</small>"),
        "{page}"
    );
    let new = order(&get(&app, "/hidden").await.1);
    let mut old = order(&get(&app, "/hidden?sort=old").await.1);
    assert_eq!(new.len(), 4);
    old.reverse();
    assert_eq!(new, old);

    // A word narrows the list, and the way back keeps it.
    let (_, page) = get(&app, "/hidden?q=Black&sort=name").await;
    assert_eq!(order(&page), vec![i.black1, i.black2], "{page}");
    assert!(page.contains("4 hidden by you"), "{page}");
    assert!(page.contains("2 with “black”"), "{page}");
    assert!(
        page.contains("name=\"back\" value=\"hidden?sort=name&amp;q=black\""),
        "{page}"
    );

    // Two chosen are shown again at once; the others stay hidden.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/unhide-many")
                .header("host", "localhost:8080")
                .header("sec-fetch-site", "same-origin")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "ids={}%2C{}&back=hidden%3Fsort%3Dname%26q%3Dblack",
                    i.black1, i.black2
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_redirection());
    assert_eq!(res.headers()["location"], "/hidden?sort=name&q=black");
    let left = idx.hidden("operator").unwrap();
    assert!(!left.contains(&i.black1) && !left.contains(&i.black2));
    assert!(left.contains(&i.other) && left.contains(&i.burst));
}
