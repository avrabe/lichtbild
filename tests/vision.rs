//! Looking at pictures: text in them (REQ-TEXT-001), things in them
//! (REQ-SEE-001) and what they show (REQ-SEE-002). Fixtures:
//! tests/fixtures/text/two-letters.png (synthetic, German text drawn for
//! lichtbild) and tests/fixtures/vision/cc0-cat-on-sofa.jpg (CC0, Wikimedia
//! Commons, metadata stripped).
//!
//! The models are pinned by SHA-256 (src/vision.rs); CI fetches them with
//! scripts/fetch-vision-models.sh. Locally set LICHTBILD_TEST_VISION_DIR to
//! that directory. Without it these tests fail, unless
//! LICHTBILD_SKIP_MODEL_TESTS=1 says so on purpose.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use lichtbild::config::{Limits, Settings};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::vision;
use lichtbild::web::{self, AppState};

const LETTERS: &[u8] = include_bytes!("fixtures/text/two-letters.png");
const CAT: &[u8] = include_bytes!("fixtures/vision/cc0-cat-on-sofa.jpg");
const EXE: &str = env!("CARGO_BIN_EXE_lichtbild");

fn model() -> Option<PathBuf> {
    match std::env::var_os("LICHTBILD_TEST_VISION_DIR") {
        Some(d) => Some(PathBuf::from(d)),
        None if std::env::var_os("LICHTBILD_SKIP_MODEL_TESTS").is_some() => {
            eprintln!("model tests skipped on purpose (LICHTBILD_SKIP_MODEL_TESTS)");
            None
        }
        None => panic!(
            "set LICHTBILD_TEST_VISION_DIR (see scripts/fetch-vision-models.sh), \
             or LICHTBILD_SKIP_MODEL_TESTS=1 to skip on purpose"
        ),
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    cfg: lichtbild::config::Config,
    base: PathBuf,
}

/// p1 holds the letters, the cat and a plain picture; p2 holds a copy of
/// the letters, to test scoping.
fn env(model_dir: Option<&Path>) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for d in ["lib/p1", "lib/p2", "data"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    std::fs::write(base.join("lib/p1/IMG_20240501_100000.png"), LETTERS).unwrap();
    std::fs::write(base.join("lib/p1/IMG_20240503_100000.jpg"), CAT).unwrap();
    let plain = image::RgbImage::from_fn(320, 240, |x, y| {
        image::Rgb([(x / 2) as u8, (y / 2) as u8, 90])
    });
    plain
        .save(base.join("lib/p1/IMG_20240502_100000.png"))
        .unwrap();
    std::fs::write(base.join("lib/p2/copy.png"), LETTERS).unwrap();
    let settings = Settings {
        roots: vec![
            ("p1".into(), base.join("lib/p1")),
            ("p2".into(), base.join("lib/p2")),
        ],
        data_dir: Some(base.join("data")),
        limits: Limits::default(),
        vision_dir: model_dir.map(Path::to_path_buf),
        ..Settings::default()
    };
    let cfg = settings.resolve().unwrap();
    Env {
        _tmp: tmp,
        cfg,
        base,
    }
}

async fn get(app: &axum::Router, uri: &str) -> String {
    let req = Request::builder()
        .uri(uri)
        .header("host", "localhost:8080")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{uri}");
    String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned()
}

fn id_of(idx: &Index, rel: &[u8]) -> u64 {
    idx.all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.label == "p1" && r.rel == rel)
        .unwrap()
        .0
}

// The child reads the letters: every line, umlauts and € included, in
// reading order, with corners on the picture, as two documents; paper is
// no thing.
// rivet: verifies REQ-TEXT-001
#[test]
fn the_reader_finds_lines_blocks_and_documents() {
    let Some(m) = model() else { return };
    let e = env(None);
    let look = vision::run_child(
        Path::new(EXE),
        &e.base.join("lib/p1/IMG_20240501_100000.png"),
        &m,
        vision::TIME_BUDGET,
    )
    .unwrap();
    let t = look.text;
    assert_eq!(t.state, "ok");
    let texts: Vec<&str> = t.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "Leuchtturmfest",
            "Sonntag ab 11 Uhr am Hafen",
            "Würstchen, Kuchen und Musik",
            "Größe der Gruppe: zwölf Personen",
            "Quittung Nr. 4711",
            "Gesamtbetrag 23,50 €",
        ]
    );
    assert_eq!(t.documents(), 2, "two sheets: {:?}", t.docs);
    // The receipt's lines sit on the right half, the heading on the left.
    assert!(
        t.lines[4].quad[0] > 0.5 && t.lines[0].quad[2] < 0.5,
        "{:?}",
        t.lines
    );
    assert!(t
        .lines
        .iter()
        .all(|l| l.quad.iter().all(|v| (0.0..=1.0).contains(v))));
    assert!(
        look.objects.is_empty(),
        "paper is no thing: {:?}",
        look.objects
    );
    assert_eq!(look.vector.len(), lichtbild::embed::DIM);
}

// The cat photo: a cat with its box, no text, and a vector that a sentence
// about it matches, better than one about something else.
// rivet: verifies REQ-SEE-001
// rivet: verifies REQ-SEE-002
#[test]
fn things_and_what_a_picture_shows_are_found() {
    let Some(m) = model() else { return };
    let e = env(None);
    let look = vision::run_child(
        Path::new(EXE),
        &e.base.join("lib/p1/IMG_20240503_100000.jpg"),
        &m,
        vision::TIME_BUDGET,
    )
    .unwrap();
    assert_eq!(look.text.state, "none");
    let cat = look
        .objects
        .iter()
        .find(|o| o.name() == "Katze")
        .expect("a cat");
    assert!(cat.score >= 0.9, "{cat:?}");
    assert!(
        cat.bbox[0] < cat.bbox[2] && cat.bbox[1] < cat.bbox[3],
        "{cat:?}"
    );
    let enc = lichtbild::embed::TextEncoder::load(&m).unwrap();
    let dot = |q: &str| -> f32 {
        let v = enc.encode(q).unwrap();
        look.vector
            .iter()
            .zip(&v)
            .map(|(a, b)| *a as f32 * b)
            .sum::<f32>()
            / 127.0
    };
    let (yes, no) = (
        dot("eine Katze auf einem Sofa"),
        dot("ein Fahrrad auf der Straße"),
    );
    assert!(
        yes >= lichtbild::embed::TEXT_MIN && yes > no + 0.05,
        "{yes} vs {no}"
    );
}

// A pass looks at each picture once (by content) and stores what it found;
// search, the viewer and the status page show it, scoped to the viewer's
// libraries.
// rivet: verifies REQ-TEXT-001
// rivet: verifies REQ-SEE-001
// rivet: verifies REQ-SEE-002
#[tokio::test]
async fn what_was_seen_is_stored_searched_and_shown() {
    let Some(m) = model() else { return };
    let e = env(Some(&m));
    let idx = Arc::new(Index::open(&e.cfg.data_dir).unwrap());
    assert!(
        scan::run(&e.cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    let rep = vision::run_pass(&e.cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    // The letters once (p1 and p2 share the content), the cat, the plain
    // picture.
    assert_eq!(
        (rep.looked, rep.with_text, rep.with_things, rep.failed),
        (3, 1, 1, 0),
        "{rep:?}"
    );
    let again = vision::run_pass(&e.cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    assert_eq!(
        again,
        vision::PassReport::default(),
        "nothing looked at twice"
    );

    let state = AppState::new(&e.cfg, idx.clone());
    state.search.rebuild(&idx, None).unwrap();
    assert_eq!(state.space.rebuild(&idx).unwrap(), 3);
    let app = web::router(state.clone());
    let letters = id_of(&idx, b"IMG_20240501_100000.png");
    let cat = id_of(&idx, b"IMG_20240503_100000.jpg");

    // Text: found, shown with the line marked, copyable.
    let hits = get(&app, "/search?q=quittung").await;
    assert!(
        hits.contains(&format!("href=\"v/all/{letters}?q=quittung\"")),
        "{hits}"
    );
    assert!(
        hits.contains("in the picture:</span> Quittung Nr. 4711"),
        "{hits}"
    );
    let page = get(&app, &format!("/v/p1/{letters}?q=quittung")).await;
    assert!(page.contains(r#"<details class="textp" open>"#), "{page}");
    assert!(page.contains("2 separate documents"), "{page}");
    assert!(
        page.contains(r#"<span class="hit">Quittung Nr. 4711</span>"#),
        "{page}"
    );
    assert!(
        page.contains(r#"<a class="hit" href="search?q=Quittung%20Nr.%204711" data-quad=""#),
        "{page}"
    );
    assert!(page.contains(r#"<div class="ptools" hidden><button type="button" class="copy">Copy</button><button type="button" class="place">Show below the picture</button>"#));
    let page = get(&app, &format!("/v/p1/{letters}")).await;
    assert!(
        page.contains(r#"<details class="textp">"#),
        "closed without a search"
    );
    // Words in pictures join the cloud, like words said in videos.
    let words = get(&app, "/words").await;
    assert!(
        words.contains(r#"href="search?q=leuchtturmfest""#),
        "{words}"
    );

    // Things: found by name (German or English), shown as chips and boxes.
    for q in ["katze", "cat", "Katzen"] {
        let hits = get(&app, &format!("/search?q={q}")).await;
        // Among the exact hits, not only among the look-alikes.
        let exact = hits.split(r#"<section class="looks">"#).next().unwrap();
        assert!(
            exact.contains(&format!("href=\"v/all/{cat}")),
            "{q}: {hits}"
        );
    }
    let page = get(&app, &format!("/v/p1/{cat}")).await;
    assert!(
        page.contains(r#"<a class="chip" href="search?q=Katze">Katze</a>"#),
        "{page}"
    );
    assert!(
        page.contains(r#"<div class="boxlayer thinglayer" data-for="things" hidden"#),
        "{page}"
    );
    // What it shows: a sentence finds the cat, under "look like", and not
    // the letters.
    let hits = get(&app, "/search?q=ein%20schwarzes%20Tier%20schl%C3%A4ft").await;
    let looks = hits
        .split(r#"<section class="looks">"#)
        .nth(1)
        .unwrap_or_default();
    assert!(looks.contains(&format!("href=\"v/all/{cat}\"")), "{hits}");
    assert!(
        !looks.contains(&format!("href=\"v/all/{letters}\"")),
        "{hits}"
    );

    // The Things page: a tile per kind, its count, a search.
    let things = get(&app, "/things").await;
    assert!(
        things.contains(&format!(
            r#"<a class="thing" href="search?q=Katze"><img src="thumb/{cat}?h="#
        )),
        "{things}"
    );
    assert!(things.contains("<b>Katzen</b><i>1</i>"), "{things}");

    let status = get(&app, "/status").await;
    assert!(
        status.contains("3 of 3 photos looked at: 1 with text, 1 with things"),
        "{status}"
    );
}

// Without models nothing is looked at; a wrong model file stops the start.
// rivet: verifies REQ-TEXT-001
#[test]
fn no_model_reads_nothing_and_a_wrong_one_is_refused() {
    let e = env(None);
    let idx = Index::open(&e.cfg.data_dir).unwrap();
    assert!(
        scan::run(&e.cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    let rep = vision::run_pass(&e.cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    assert_eq!(rep, vision::PassReport::default());

    let fake = e.base.join("fake-model");
    std::fs::create_dir_all(&fake).unwrap();
    for (f, _, _) in vision::FILES {
        std::fs::write(fake.join(f), b"not a model").unwrap();
    }
    let err = vision::check(&fake).unwrap_err().to_string();
    assert!(err.contains("unknown model file"), "{err}");
}
