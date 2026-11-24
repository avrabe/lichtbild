//! Faces (REQ-FACE-001, REQ-FACE-002): the faces child finds them, and
//! the faces of one person are grouped. Fixtures, all public domain (NASA
//! photographs, works of the US government), scaled down with no metadata
//! kept: two portraits of one person (pd-portrait-a1, a2), one of another
//! (pd-portrait-b1), and a group photo with both of them among eight
//! (pd-group); sources in tests/fixtures/README.md.
//!
//! The models are pinned by SHA-256 (src/faces.rs); CI fetches them with
//! scripts/fetch-vision-models.sh. Locally set LICHTBILD_TEST_VISION_DIR.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use lichtbild::config::{Limits, Settings};
use lichtbild::faces;
use lichtbild::index::Index;
use lichtbild::people::Who;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState};

const A1: &[u8] = include_bytes!("fixtures/faces/pd-portrait-a1.jpg");
const A2: &[u8] = include_bytes!("fixtures/faces/pd-portrait-a2.jpg");
const B1: &[u8] = include_bytes!("fixtures/faces/pd-portrait-b1.jpg");
const GROUP: &[u8] = include_bytes!("fixtures/faces/pd-group.jpg");
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

fn found(m: &Path, pic: &[u8]) -> faces::Found {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("pic.jpg");
    std::fs::write(&f, pic).unwrap();
    lichtbild::vision::run_child_as(Path::new(EXE), "faces", &f, m, faces::TIME_BUDGET).unwrap()
}

// One face in each portrait, eight in the group; boxes on the picture,
// vectors of the model's size, a small picture of each.
// rivet: verifies REQ-FACE-001
#[test]
fn the_child_finds_every_face_with_its_vector() {
    let Some(m) = model() else { return };
    assert!(faces::present(&m).unwrap());
    for (pic, n) in [(A1, 1), (A2, 1), (B1, 1), (GROUP, 8)] {
        let f = found(&m, pic);
        assert_eq!(f.faces.len(), n);
        assert_eq!(f.crops.len(), n);
        for face in &f.faces {
            assert_eq!(face.v.len(), faces::DIM);
            assert!(face.score >= faces::MIN_SCORE);
            assert!(
                face.b.iter().all(|v| (0.0..=1.0).contains(v)),
                "{:?}",
                face.b
            );
        }
        for c in &f.crops {
            let img = image::load_from_memory(c).unwrap();
            assert_eq!((img.width(), img.height()), (faces::CROP, faces::CROP));
        }
    }
}

// The same person's portraits match each other better than anyone else's,
// and each is found again among the eight faces of the group photo.
// rivet: verifies REQ-FACE-002
#[test]
fn one_persons_faces_match_and_others_do_not() {
    let Some(m) = model() else { return };
    let v = |pic| found(&m, pic).faces.remove(0).v;
    let (a1, a2, b1) = (v(A1), v(A2), v(B1));
    let same = faces::cos(&a1, &a2);
    let other = faces::cos(&a1, &b1).max(faces::cos(&a2, &b1));
    assert!(same >= lichtbild::people::LINK, "same person {same}");
    assert!(other < lichtbild::people::LINK, "other person {other}");
    let group = found(&m, GROUP).faces;
    for (who, mine) in [("a", &a1), ("b", &b1)] {
        let mut sims: Vec<f32> = group.iter().map(|g| faces::cos(mine, &g.v)).collect();
        sims.sort_by(|x, y| y.total_cmp(x));
        assert!(
            sims[0] >= lichtbild::people::LINK,
            "{who} in the group: {sims:?}"
        );
        assert!(
            sims[1] < lichtbild::people::LINK,
            "{who} only once: {sims:?}"
        );
    }
}

// The pass stores faces and pictures of them; grouping puts the portraits
// of one person and their face in the group photo together; a name given
// on the web goes to all three and is found by search; the viewer names
// the faces of a photo.
// rivet: verifies REQ-FACE-002
// rivet: verifies REQ-FACE-003
// rivet: verifies REQ-FACE-006
#[tokio::test]
async fn the_pass_groups_people_and_a_name_finds_them() {
    let Some(m) = model() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for d in ["lib/p1", "data"] {
        std::fs::create_dir_all(base.join(d)).unwrap();
    }
    for (name, pic) in [
        ("IMG_20240501_100000.jpg", A1),
        ("IMG_20240502_100000.jpg", A2),
        ("IMG_20240503_100000.jpg", B1),
        ("IMG_20240504_100000.jpg", GROUP),
    ] {
        std::fs::write(base.join("lib/p1").join(name), pic).unwrap();
    }
    let cfg = Settings {
        roots: vec![("p1".into(), base.join("lib/p1"))],
        data_dir: Some(base.join("data")),
        limits: Limits::default(),
        vision_dir: Some(m.clone()),
        ..Settings::default()
    }
    .resolve()
    .unwrap();
    let idx = Arc::new(Index::open(&cfg.data_dir).unwrap());
    scan::run(&cfg, &idx, &Options::default()).unwrap();
    let rep = faces::run_pass(&cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    assert_eq!((rep.looked, rep.faces, rep.failed), (4, 11, 0));
    // Done once: a second pass has nothing left.
    let again = faces::run_pass(&cfg, &idx, Path::new(EXE), None, &|_, _, _| {}).unwrap();
    assert_eq!(again.looked, 0);

    let state = AppState::new(&cfg, idx.clone());
    state.search.rebuild(&idx, None).unwrap();
    // Before the faces are read after a start, People says they are on
    // their way instead of "No faces yet" (REQ-FACE-006).
    let early = web::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/people")
                .header("host", "localhost:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let early = early.into_body().collect().await.unwrap().to_bytes();
    let early = String::from_utf8_lossy(&early);
    assert!(early.contains("Faces are still loading"), "{early}");
    assert!(!early.contains("No faces yet"));
    assert_eq!(state.people.rebuild(&idx).unwrap(), 11);
    let id = |n: &str| {
        idx.all()
            .unwrap()
            .into_iter()
            .find(|(_, r)| r.rel == n.as_bytes())
            .map(|(i, r)| (i, r.hash))
            .unwrap()
    };
    let (a1, a2, b1, group) = (
        id("IMG_20240501_100000.jpg"),
        id("IMG_20240502_100000.jpg"),
        id("IMG_20240503_100000.jpg"),
        id("IMG_20240504_100000.jpg"),
    );
    let who = |h: &str| state.people.in_photo(h)[0].who;
    assert_eq!(who(&a1.1), who(&a2.1));
    assert_ne!(who(&a1.1), who(&b1.1));
    let in_group: Vec<Who> = state
        .people
        .in_photo(&group.1)
        .iter()
        .map(|f| f.who)
        .collect();
    assert!(in_group.contains(&who(&a1.1)), "a in the group photo");
    assert!(in_group.contains(&who(&b1.1)), "b in the group photo");
    assert!(faces::crop_path(&cfg.data_dir, &a1.1, 0).exists());

    let app = web::router(state.clone());
    let send = |method: &str, uri: &str, body: &str| {
        let mut r = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "localhost:8080");
        if method == "POST" {
            r = r
                .header("sec-fetch-site", "same-origin")
                .header("content-type", "application/x-www-form-urlencoded");
        }
        let req = r.body(Body::from(body.to_string())).unwrap();
        let app = app.clone();
        async move {
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let loc = res
                .headers()
                .get("location")
                .map(|l| l.to_str().unwrap().to_string());
            let b = res.into_body().collect().await.unwrap().to_bytes();
            (status, loc, String::from_utf8_lossy(&b).into_owned())
        }
    };
    // Three photos of a: offered for a name, with their faces.
    let (s, _, page) = send("GET", "/people", "").await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains("Who is this?"), "{page}");
    let (s, _, _) = send("GET", &format!("/face/{}/0", a1.0), "").await;
    assert_eq!(s, StatusCode::OK);
    // Named from a's group page: all three photos are hers.
    let (s, _, _) = send("GET", &format!("/people/{}/0", a1.0), "").await;
    assert_eq!(s, StatusCode::OK);
    let (s, loc, _) = send(
        "POST",
        "/people/name",
        &format!("item={}&face=0&name=Astra", a1.0),
    )
    .await;
    assert!(s.is_redirection(), "{s}");
    let p = loc.unwrap();
    let (s, _, page) = send("GET", &p, "").await;
    assert_eq!(s, StatusCode::OK);
    for (i, _) in [&a1, &a2, &group] {
        assert!(page.contains(&format!("thumb/{i}?")), "{i} on {p}");
    }
    assert!(!page.contains(&format!("thumb/{}?", b1.0)));
    // Search: the name alone opens her page; with a word, it narrows.
    let (s, loc, _) = send("GET", "/search?q=astra", "").await;
    assert!(s.is_redirection());
    assert_eq!(loc.unwrap(), p);
    // The viewer of the group photo names her among the faces, and offers
    // the others a name.
    let (_, _, v) = send("GET", &format!("/v/p1/{}", group.0), "").await;
    assert!(
        v.contains("details class=\"faces\"") && v.contains("☺ 8"),
        "{v}"
    );
    assert!(
        v.contains(">Astra</a>") && v.contains("Who is this?"),
        "{v}"
    );
}

// Photos go to a child in batches; a picture that cannot be read fails
// alone, and a child that takes too long is killed and the rest go to a
// new one, each failing on its own.
// rivet: verifies REQ-FACE-002
#[test]
fn a_bad_picture_or_a_hang_fails_alone_in_a_batch() {
    let Some(m) = model() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let files: Vec<PathBuf> = [
        ("a.jpg", A1),
        ("broken.jpg", &b"not a picture"[..]),
        ("b.jpg", B1),
    ]
    .iter()
    .map(|(n, d)| {
        let p = tmp.path().join(n);
        std::fs::write(&p, d).unwrap();
        p
    })
    .collect();
    let mut got: Vec<(usize, Result<usize, String>)> = Vec::new();
    lichtbild::vision::run_batch::<faces::Found>(
        Path::new(EXE),
        "faces",
        &m,
        &files,
        faces::TIME_BUDGET,
        &mut |i, r| {
            got.push((i, r.map(|f| f.faces.len()).map_err(|e| e.to_string())));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0], (0, Ok(1)));
    assert!(
        matches!(&got[1], (1, Err(e)) if e.contains("decode")),
        "{got:?}"
    );
    assert_eq!(got[2], (2, Ok(1)));
    // A path that is not UTF-8 fails alone and costs no other photo.
    use std::os::unix::ffi::OsStrExt;
    let odd = tmp.path().join(std::ffi::OsStr::from_bytes(b"\xff.jpg"));
    let mixed = vec![files[0].clone(), odd, files[2].clone()];
    let mut got = Vec::new();
    lichtbild::vision::run_batch::<faces::Found>(
        Path::new(EXE),
        "faces",
        &m,
        &mixed,
        faces::TIME_BUDGET,
        &mut |i, r| {
            got.push((i, r.map(|f| f.faces.len()).map_err(|e| e.to_string())));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        got,
        vec![
            (0, Ok(1)),
            (1, Err("path not UTF-8".to_string())),
            (2, Ok(1))
        ]
    );
    // No time at all: every picture times out, each in its own child.
    let mut late = Vec::new();
    lichtbild::vision::run_batch::<faces::Found>(
        Path::new(EXE),
        "faces",
        &m,
        &files,
        std::time::Duration::from_millis(1),
        &mut |i, r| {
            late.push((i, r.err().map(|e| e.to_string())));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(late.len(), 3, "{late:?}");
    for (i, (n, e)) in late.iter().enumerate() {
        assert_eq!(*n, i);
        // The broken one may fail before its time is up.
        let want: &[&str] = if i == 1 {
            &["timeout", "decode"]
        } else {
            &["timeout"]
        };
        assert!(want.contains(&e.as_deref().unwrap_or("")), "{late:?}");
    }
}
