//! Text in pictures (REQ-TEXT-001), on a synthetic fixture only:
//! tests/fixtures/text/two-letters.png, two sheets with German text drawn
//! for lichtbild.
//!
//! The models are PaddleOCR's, pinned by SHA-256 (src/ocr.rs); CI fetches
//! them with scripts/fetch-ocr-model.sh. Locally set LICHTBILD_TEST_OCR_DIR
//! to a directory with det.onnx, rec.onnx and dict.txt. Without it these
//! tests fail, unless LICHTBILD_SKIP_MODEL_TESTS=1 says so on purpose.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use lichtbild::config::{Limits, Settings};
use lichtbild::index::Index;
use lichtbild::ocr;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

const LETTERS: &[u8] = include_bytes!("fixtures/text/two-letters.png");
const EXE: &str = env!("CARGO_BIN_EXE_lichtbild");

fn model() -> Option<PathBuf> {
    match std::env::var_os("LICHTBILD_TEST_OCR_DIR") {
        Some(d) => Some(PathBuf::from(d)),
        None if std::env::var_os("LICHTBILD_SKIP_MODEL_TESTS").is_some() => {
            eprintln!("model tests skipped on purpose (LICHTBILD_SKIP_MODEL_TESTS)");
            None
        }
        None => panic!(
            "set LICHTBILD_TEST_OCR_DIR (see scripts/fetch-ocr-model.sh), \
             or LICHTBILD_SKIP_MODEL_TESTS=1 to skip on purpose"
        ),
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    cfg: lichtbild::config::Config,
    base: PathBuf,
}

/// p1 holds the letters and a plain picture without text; p2 holds a copy
/// of the letters, to test scoping.
fn env(model_dir: Option<&Path>) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for d in ["lib/p1", "lib/p2", "data"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    std::fs::write(base.join("lib/p1/IMG_20240501_100000.png"), LETTERS).unwrap();
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
        ocr_dir: model_dir.map(Path::to_path_buf),
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

// The child reads the fixture: every line, umlauts and € included, in
// reading order, with corners on the picture, as two documents.
// rivet: verifies REQ-TEXT-001
#[test]
fn the_reader_finds_lines_blocks_and_documents() {
    let Some(m) = model() else { return };
    let e = env(None);
    let t = ocr::run_child(
        Path::new(EXE),
        &e.base.join("lib/p1/IMG_20240501_100000.png"),
        &m,
        ocr::TIME_BUDGET,
    )
    .unwrap();
    assert_eq!(t.state, "ok");
    assert_eq!(t.model, ocr::MODEL);
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
}

// A pass reads each picture once (by content), stores what it found,
// and search, the viewer and the status page show it, scoped to the
// viewer's libraries.
// rivet: verifies REQ-TEXT-001
#[tokio::test]
async fn read_text_is_stored_searched_and_shown() {
    let Some(m) = model() else { return };
    let e = env(Some(&m));
    let idx = Arc::new(Index::open(&e.cfg.data_dir).unwrap());
    assert!(
        scan::run(&e.cfg, &idx, &Options::default())
            .unwrap()
            .reconciled
    );
    let rep = ocr::run_pass(&e.cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    // The letters once (p1 and p2 share the content), the plain picture.
    assert_eq!(
        (rep.with_text, rep.without, rep.failed),
        (1, 1, 0),
        "{rep:?}"
    );
    let again = ocr::run_pass(&e.cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    assert_eq!(again, ocr::PassReport::default(), "nothing read twice");

    let state = AppState::new(&e.cfg, idx.clone());
    state.search.rebuild(&idx, None).unwrap();
    let app = web::router(state.clone());
    let id = idx
        .all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.label == "p1" && r.rel == b"IMG_20240501_100000.png")
        .unwrap()
        .0;

    let hits = get(&app, "/search?q=quittung").await;
    assert!(
        hits.contains(&format!("href=\"v/all/{id}?q=quittung\"")),
        "{hits}"
    );
    assert!(
        hits.contains("in the picture:</span> Quittung Nr. 4711"),
        "{hits}"
    );

    let page = get(&app, &format!("/v/p1/{id}?q=quittung")).await;
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
    assert!(
        page.contains("<span>Würstchen, Kuchen und Musik</span>"),
        "{page}"
    );
    let page = get(&app, &format!("/v/p1/{id}")).await;
    assert!(
        page.contains(r#"<details class="textp">"#),
        "closed without a search"
    );

    let status = get(&app, "/status").await;
    assert!(
        status.contains("2 of 2 photos read, 1 with text"),
        "{status}"
    );
}

// Without a model nothing is read; a wrong model file stops the start.
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
    let rep = ocr::run_pass(&e.cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    assert_eq!(rep, ocr::PassReport::default());

    let fake = e.base.join("fake-model");
    std::fs::create_dir_all(&fake).unwrap();
    for f in ["det.onnx", "rec.onnx", "dict.txt"] {
        std::fs::write(fake.join(f), b"not a model").unwrap();
    }
    let err = ocr::check_model(&fake).unwrap_err().to_string();
    assert!(err.contains("unknown model file"), "{err}");
}
