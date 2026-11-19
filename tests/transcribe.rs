//! Transcripts and search (REQ-MEDIA-004, REQ-SEARCH-001), on open media
//! only: 19.5 s of a public-domain LibriVox reading (Kafka, "Auf der
//! Galerie") and a synthetic tone and silence clip (tests/fixtures).
//!
//! The model is Whisper tiny q8_0, pinned by SHA-256 like base
//! (stt::MODELS); CI downloads it, see .github/workflows/ci.yml. Locally set
//! LICHTBILD_TEST_WHISPER_DIR to a directory with config.json,
//! tokenizer.json and model.gguf. Without it these tests fail, unless
//! LICHTBILD_SKIP_MODEL_TESTS=1 says so on purpose: a model test must not
//! pass by being skipped.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use lichtbild::config::{Limits, Settings, TranscribeFiles};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::search::Search;
use lichtbild::stt;
use lichtbild::web::{self, AppState};

const SPEECH: &[u8] = include_bytes!("fixtures/video/librivox-kafka-galerie-de.mp4");
const TONE: &[u8] = include_bytes!("fixtures/video/synthetic-tone-silence.mp4");
const EXE: &str = env!("CARGO_BIN_EXE_lichtbild");

/// The model directory, or None when the run says to skip on purpose.
fn model() -> Option<PathBuf> {
    match std::env::var_os("LICHTBILD_TEST_WHISPER_DIR") {
        Some(d) => Some(PathBuf::from(d)),
        None if std::env::var_os("LICHTBILD_SKIP_MODEL_TESTS").is_some() => {
            eprintln!("model tests skipped on purpose (LICHTBILD_SKIP_MODEL_TESTS)");
            None
        }
        None => panic!(
            "set LICHTBILD_TEST_WHISPER_DIR (Whisper tiny q8_0, see tests/transcribe.rs), \
             or LICHTBILD_SKIP_MODEL_TESTS=1 to skip on purpose"
        ),
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    cfg: lichtbild::config::Config,
}

/// p1 holds the speech clip and the tone clip; p2 holds a copy of the
/// speech clip under another name, to test scoping.
fn env(model_dir: &Path) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for d in ["lib/p1/v", "lib/p2", "data"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    std::fs::write(base.join("lib/p1/v/galerie.mp4"), SPEECH).unwrap();
    std::fs::write(base.join("lib/p1/v/tone.mp4"), TONE).unwrap();
    // A different file (one byte appended to the container's end), so it
    // gets its own hash and transcript.
    let mut other = SPEECH.to_vec();
    other.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    std::fs::write(base.join("lib/p2/zirkus.mp4"), other).unwrap();
    let cfg = Settings {
        roots: vec![
            ("p1".into(), base.join("lib/p1")),
            ("p2".into(), base.join("lib/p2")),
        ],
        data_dir: Some(base.join("data")),
        workers: Some(2),
        limits: Limits {
            max_pixels: 400_000,
            ..Limits::default()
        },
        transcribe: TranscribeFiles {
            model_dir: Some(model_dir.to_path_buf()),
            languages: vec!["de".into(), "en".into()],
        },
        ..Settings::default()
    }
    .resolve()
    .unwrap();
    Env { _tmp: tmp, cfg }
}

fn opts() -> Options {
    Options {
        keyframe_decoder: Some(EXE.into()),
        ..Options::default()
    }
}

fn words(t: &stt::Transcript) -> String {
    t.pieces
        .iter()
        .map(|p| p.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

// The child transcribes German speech with times, in German, and nothing
// past the end of the audio.
// rivet: verifies REQ-MEDIA-004
#[test]
fn speech_is_transcribed_in_its_language_with_times() {
    let Some(dir) = model() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("galerie.mp4");
    std::fs::write(&f, SPEECH).unwrap();
    let t = stt::run_child(
        Path::new(EXE),
        &f,
        &dir,
        &[],
        std::time::Duration::from_secs(300),
    )
    .unwrap();
    assert_eq!(t.state, "ok", "{t:?}");
    assert_eq!(t.model, "whisper-tiny-q80");
    assert!(
        (19.0..20.0).contains(&t.audio_seconds),
        "{}",
        t.audio_seconds
    );
    // Language detected per window, without a configured set.
    assert!(t.pieces.iter().all(|p| p.lang == "de"), "{t:?}");
    let w = words(&t);
    for expected in ["franz kafka", "publikum"] {
        assert!(w.contains(expected), "{expected:?} missing: {w}");
    }
    let mut last = 0.0f32;
    for p in &t.pieces {
        assert!(p.start >= last - 0.01, "times go back: {t:?}");
        assert!(
            p.start < t.audio_seconds && p.end <= t.audio_seconds + 0.01,
            "{t:?}"
        );
        last = p.start;
    }
}

// A tone and silence is no speech, and decoding it does not loop through
// every temperature (it took 22 s before the loop cut, 0.4 s after).
// rivet: verifies REQ-MEDIA-004
#[test]
fn tone_and_silence_are_no_speech() {
    let Some(dir) = model() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("tone.mp4");
    std::fs::write(&f, TONE).unwrap();
    let start = std::time::Instant::now();
    let t = stt::run_child(
        Path::new(EXE),
        &f,
        &dir,
        &[],
        std::time::Duration::from_secs(300),
    )
    .unwrap();
    assert_eq!(
        (t.state.as_str(), t.pieces.len()),
        ("no-speech", 0),
        "{t:?}"
    );
    // 0.4 s on an M4, 9.9 s on the CI runner; without the loop cut a tone
    // ran through every temperature (22 s on the M4).
    assert!(start.elapsed().as_secs() < 30, "{:?}", start.elapsed());
}

// A model whose hash is not pinned is refused when the config is read.
// rivet: verifies REQ-MEDIA-004
#[test]
fn an_unpinned_model_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::write(d.join("config.json"), "{}").unwrap();
    std::fs::write(d.join("tokenizer.json"), "{}").unwrap();
    std::fs::write(d.join("model.gguf"), b"not the pinned model").unwrap();
    let e = stt::check_model(d).unwrap_err().to_string();
    assert!(e.contains("unknown model"), "{e}");
    std::fs::remove_file(d.join("model.gguf")).unwrap();
    assert!(stt::check_model(d).is_err());
}

// The pass after a scan stores one transcript per content, does not redo
// it, and keeps a failure as a failure instead of stopping.
// rivet: verifies REQ-MEDIA-004
#[test]
fn the_pass_stores_transcripts_once_and_survives_failures() {
    let Some(dir) = model() else { return };
    let e = env(&dir);
    let idx = Index::open(&e.cfg.data_dir).unwrap();
    scan::run(&e.cfg, &idx, &opts()).unwrap();

    // A decoder that fails: every video is recorded as failed, none lost.
    let rep = stt::run_pass(&e.cfg, &idx, Path::new("/usr/bin/false"), None).unwrap();
    assert_eq!(
        (rep.transcribed, rep.no_speech, rep.failed),
        (0, 0, 3),
        "{rep:?}"
    );
    // A failure with this model is not retried by the next pass.
    let rep = stt::run_pass(&e.cfg, &idx, Path::new(EXE), None).unwrap();
    assert_eq!(rep.failed + rep.transcribed + rep.no_speech, 0, "{rep:?}");

    // Fresh index: the real child.
    let e = env(&dir);
    let idx = Index::open(&e.cfg.data_dir).unwrap();
    scan::run(&e.cfg, &idx, &opts()).unwrap();
    let rep = stt::run_pass(&e.cfg, &idx, Path::new(EXE), None).unwrap();
    assert_eq!(
        (rep.transcribed, rep.no_speech, rep.failed),
        (2, 1, 0),
        "{rep:?}"
    );
    let again = stt::run_pass(&e.cfg, &idx, Path::new(EXE), None).unwrap();
    assert_eq!(again, stt::PassReport::default());
    // A deadline in the past leaves the work for the next pass.
    let e2 = env(&dir);
    let idx2 = Index::open(&e2.cfg.data_dir).unwrap();
    scan::run(&e2.cfg, &idx2, &opts()).unwrap();
    let rep = stt::run_pass(
        &e2.cfg,
        &idx2,
        Path::new(EXE),
        Some(std::time::Instant::now()),
    )
    .unwrap();
    assert_eq!(rep.remaining, 3, "{rep:?}");
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
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn id_of(idx: &Index, rel: &str) -> u64 {
    idx.all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.rel == rel.as_bytes())
        .map(|(id, _)| id)
        .unwrap()
}

// Spoken words are found, scoped to the libraries asked for, and lead to
// the moment in the video; the viewer shows the transcript.
// rivet: verifies REQ-SEARCH-001
#[tokio::test]
async fn spoken_words_are_found_in_scope_and_open_the_video_there() {
    let Some(dir) = model() else { return };
    let e = env(&dir);
    let idx = Arc::new(Index::open(&e.cfg.data_dir).unwrap());
    scan::run(&e.cfg, &idx, &opts()).unwrap();
    stt::run_pass(&e.cfg, &idx, Path::new(EXE), None).unwrap();
    let state = AppState::new(&e.cfg, idx.clone());
    let docs = state.search.rebuild(&idx, None).unwrap();
    assert!(docs > 3, "{docs}");
    let gal = id_of(&idx, "v/galerie.mp4");
    let zir = id_of(&idx, "zirkus.mp4");

    // Scope inside the query: p1 only never returns p2's copy.
    let s: &Search = &state.search;
    let p1 = s.query("Publikum", &["p1".to_string()]).unwrap();
    assert_eq!(p1.iter().map(|h| h.id).collect::<Vec<_>>(), [gal], "{p1:?}");
    assert!(!p1[0].speech.is_empty());
    let both = s.query("Publikum", &["p1".into(), "p2".into()]).unwrap();
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(s.query("Publikum", &[]).unwrap().is_empty());
    // Names and dates are searchable too; folding finds "kafka" in "Kafka".
    assert!(s
        .query("zirkus", &["p2".into()])
        .unwrap()
        .iter()
        .any(|h| h.id == zir));

    let app = web::router(state);
    let (st, page) = get(&app, "/search?q=publikum&who=p1").await;
    assert_eq!(st, StatusCode::OK);
    let link = format!("href=\"v/p1/{gal}?t=");
    assert!(page.contains(&link), "{page}");
    assert!(!page.contains(&format!("/{zir}")), "out of scope: {page}");
    // A time opens the video there; the viewer shows the transcript.
    let start = page.split(&link).nth(1).unwrap().split('"').next().unwrap();
    let (st, viewer) = get(&app, &format!("/v/p1/{gal}?t={start}")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        viewer.contains(&format!("src=\"original/{gal}#t={start}\"")),
        "{viewer}"
    );
    assert!(viewer.contains("class=\"transcript\""), "{viewer}");
    assert!(viewer.contains("whisper-tiny-q80"), "{viewer}");
    // A video's transcript panel speaks of the video, not a picture.
    assert!(
        viewer.contains(r#"class="place">Show below the video</button>"#),
        "{viewer}"
    );
}
