//! Video: container, metadata and poster frames from synthetic clips
//! (REQ-MEDIA-001). The clips are generated test patterns; see
//! tests/fixtures/README.md.

#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use lichtbild::config::{Limits, Settings};
use lichtbild::index::Index;
use lichtbild::scan::{self, Options};
use lichtbild::web::{self, AppState, ByteRange};

const H264: &[u8] = include_bytes!("fixtures/video/synthetic-h264-160x120.mp4");
const HEVC10_ROT90: &[u8] = include_bytes!("fixtures/video/synthetic-hevc10-rot90.mov");
const MPEG4: &[u8] = include_bytes!("fixtures/video/synthetic-mpeg4.mp4");
const TRUNCATED: &[u8] = include_bytes!("fixtures/video/truncated.mp4");

pub struct Env {
    _tmp: tempfile::TempDir,
    pub cfg: lichtbild::config::Config,
}

pub fn env() -> Env {
    env_with(400_000)
}

pub fn env_with(max_pixels: u64) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("lib/p1");
    let data = base.join("data");
    std::fs::create_dir_all(root.join("v")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let w = |p: &str, b: &[u8]| std::fs::write(root.join(p), b).unwrap();
    w("v/clip-h264.mp4", H264);
    w("v/IMG_0001.MOV", HEVC10_ROT90);
    w("v/old-mpeg4.mp4", MPEG4);
    w("v/truncated.mp4", TRUNCATED);
    let cfg = Settings {
        roots: vec![("p1".into(), root)],
        data_dir: Some(data),
        workers: Some(2),
        limits: Limits {
            max_pixels,
            ..Limits::default()
        },
        ..Settings::default()
    }
    .resolve()
    .unwrap();
    Env { _tmp: tmp, cfg }
}

/// Posters are decoded by the lichtbild binary in a child process; the
/// test harness itself is not that binary.
fn opts() -> Options {
    Options {
        keyframe_decoder: Some(env!("CARGO_BIN_EXE_lichtbild").into()),
        ..Options::default()
    }
}

fn record(idx: &Index, rel: &str) -> lichtbild::index::Record {
    idx.all()
        .unwrap()
        .into_iter()
        .map(|(_, r)| r)
        .find(|r| r.rel == rel.as_bytes())
        .unwrap_or_else(|| panic!("{rel} not indexed"))
}

/// Mean colour of each quadrant: top-left, top-right, bottom-left,
/// bottom-right.
fn quadrants(img: &image::RgbImage) -> [[u8; 3]; 4] {
    let (w, h) = (img.width(), img.height());
    let mut out = [[0u8; 3]; 4];
    for (q, (x0, y0)) in [(0, 0), (w / 2, 0), (0, h / 2), (w / 2, h / 2)]
        .into_iter()
        .enumerate()
    {
        let mut sum = [0u64; 3];
        let mut n = 0u64;
        for y in y0..y0 + h / 2 {
            for x in x0..x0 + w / 2 {
                let p = img.get_pixel(x, y);
                for c in 0..3 {
                    sum[c] += u64::from(p[c]);
                }
                n += 1;
            }
        }
        out[q] = sum.map(|s| (s / n) as u8);
    }
    out
}

fn preview(data: &Path, hash: &str) -> image::RgbImage {
    image::open(scan::preview_path(data, hash))
        .unwrap()
        .to_rgb8()
}

// rivet: verifies REQ-MEDIA-001
#[test]
fn videos_are_indexed_with_poster_length_date_and_place() {
    let e = env();
    let idx = Index::open(&e.cfg.data_dir).unwrap();
    let report = scan::run(&e.cfg, &idx, &opts()).unwrap();
    let r = &report.roots[0];
    assert!(report.reconciled, "{:?}", report.problems);
    assert_eq!((r.found, r.indexed, r.skipped), (4, 3, 1));
    assert_eq!(
        r.skipped_by_reason.get("decode error (mp4)"),
        Some(&1),
        "{:?}",
        r.skipped_by_reason
    );
    assert_eq!(r.thumbnails.generated, 2);
    assert_eq!(r.thumbnails.previews, 2);
    let codecs: Vec<_> = r
        .videos_by_codec
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert_eq!(codecs, [("h264", 1), ("hevc", 1), ("mp4v", 1)]);
    assert!(report
        .to_text()
        .contains("  videos: h264 1, hevc 1, mp4v 1\n"));
    assert_eq!(
        r.thumbnails.failed.get("decode-error (mp4v)"),
        Some(&1),
        "{:?}",
        r.thumbnails.failed
    );

    // H.264, no rotation, mvhd time (UTC), Android-style ©xyz position.
    let h = record(&idx, "v/clip-h264.mp4");
    let v = h.video.clone().unwrap();
    assert_eq!((v.codec.as_str(), h.width, h.height), ("h264", 160, 120));
    assert!((900..=1100).contains(&v.duration_ms), "{}", v.duration_ms);
    assert_eq!(
        (h.taken_local.as_str(), h.taken_offset.as_deref()),
        ("2024-05-06T07:08:09", Some("+00:00"))
    );
    assert_eq!(h.date_source, "video-utc");
    assert_eq!((h.lat, h.lon), (Some(10.52), Some(20.27)));
    assert_eq!((h.thumb.as_str(), h.preview), ("ok", true));

    // 10-bit HEVC stored landscape with the clockwise 90-degree matrix of
    // an iPhone portrait clip (ffprobe: rotation=-90), Apple keys.
    let m = record(&idx, "v/IMG_0001.MOV");
    let v = m.video.clone().unwrap();
    assert_eq!((v.codec.as_str(), m.width, m.height), ("hevc", 144, 176));
    assert_eq!(
        (m.taken_local.as_str(), m.taken_offset.as_deref()),
        ("2024-07-08T21:30:00", Some("+02:00"))
    );
    assert_eq!(m.date_source, "video");
    assert_eq!((m.lat, m.lon), (Some(11.3), Some(20.1)));

    // A codec lichtbild cannot decode: indexed, playable, no poster.
    let o = record(&idx, "v/old-mpeg4.mp4");
    assert_eq!(o.video.unwrap().codec, "mp4v");
    assert_eq!((o.thumb.as_str(), o.preview), ("decode-error", false));

    let ph = preview(&e.cfg.data_dir, &h.hash);
    let pm = preview(&e.cfg.data_dir, &m.hash);
    assert_eq!(ph.dimensions(), (160, 120));
    assert_eq!(pm.dimensions(), (144, 176));
    // Quadrant colours of the same first frame as ffmpeg 9.0.1 decodes and
    // rotates it (`ffmpeg -i <clip> -frames:v 1 -pix_fmt rgb24`); a wrong
    // rotation, plane order or colour matrix moves them by far more.
    close(
        quadrants(&ph),
        [[103, 98, 9], [83, 98, 216], [175, 162, 3], [94, 74, 242]],
    );
    close(
        quadrants(&pm),
        [[169, 164, 4], [107, 110, 10], [90, 79, 240], [85, 94, 230]],
    );
}

fn close(got: [[u8; 3]; 4], want: [[u8; 3]; 4]) {
    for (g, w) in got.iter().flatten().zip(want.iter().flatten()) {
        assert!(g.abs_diff(*w) <= 6, "got {got:?}, ffmpeg {want:?}");
    }
}

// A rescan reads nothing again, and the video without a poster is still
// counted under its codec, so the kill criterion sees every scan.
// rivet: verifies REQ-MEDIA-001
#[test]
fn rescan_keeps_videos_and_counts_failed_posters_by_codec() {
    let e = env();
    let idx = Index::open(&e.cfg.data_dir).unwrap();
    scan::run(&e.cfg, &idx, &opts()).unwrap();
    let again = scan::run(&e.cfg, &idx, &opts()).unwrap();
    let r = &again.roots[0];
    assert!(again.reconciled, "{:?}", again.problems);
    assert_eq!((r.indexed, r.unchanged, r.new, r.updated), (3, 3, 0, 0));
    assert_eq!(r.thumbnails.generated, 0);
    assert_eq!(r.thumbnails.previews, 2);
    // Unchanged videos are counted too, so every scan has the denominator.
    assert_eq!(r.videos_by_codec.values().sum::<u64>(), 3);
    assert_eq!(
        r.thumbnails.failed.get("decode-error (mp4v)"),
        Some(&1),
        "{:?}",
        r.thumbnails.failed
    );
}

async fn get(
    app: &axum::Router,
    uri: &str,
    range: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(uri).header("host", "localhost:8080");
    if let Some(r) = range {
        req = req.header("range", r);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, body)
}

fn id_of(idx: &Index, rel: &str) -> u64 {
    idx.all()
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.rel == rel.as_bytes())
        .map(|(id, _)| id)
        .unwrap()
}

// The timeline marks videos with their length; the viewer plays the
// original with the poster; the original seeks by range requests.
// rivet: verifies REQ-MEDIA-001
#[tokio::test]
async fn videos_play_with_poster_and_seek_by_range() {
    let e = env();
    let idx = Arc::new(Index::open(&e.cfg.data_dir).unwrap());
    scan::run(&e.cfg, &idx, &opts()).unwrap();
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let places = lichtbild::places::Places::load(
        &fx.join("places-cities.txt"),
        Some(&fx.join("places-admin1.txt")),
        Some(&fx.join("places-countries.txt")),
    )
    .unwrap();
    let app = web::router(AppState::new(&e.cfg, idx.clone()).with_places(Some(places)));
    let mov = id_of(&idx, "v/IMG_0001.MOV");
    let old = id_of(&idx, "v/old-mpeg4.mp4");

    let (st, _, body) = get(&app, "/t/p1", None).await;
    let page = String::from_utf8(body).unwrap();
    assert_eq!(st, StatusCode::OK);
    assert!(
        page.contains(&format!("class=\"item video\" id=\"i{mov}\"")),
        "{page}"
    );
    assert!(page.contains("<span class=\"dur\">0:01</span>"), "{page}");
    assert!(
        page.contains("<span class=\"nothumb\">video</span>"),
        "{page}"
    );

    let (st, _, body) = get(&app, &format!("/v/p1/{mov}"), None).await;
    let page = String::from_utf8(body).unwrap();
    assert_eq!(st, StatusCode::OK);
    assert!(page.contains(&format!("src=\"original/{mov}\"")), "{page}");
    assert!(
        page.contains(&format!("poster=\"preview/{mov}?h=")),
        "{page}"
    );
    assert!(
        page.contains("<dt>Length</dt><dd>0:01 <span class=\"dim\">· HEVC</span>"),
        "{page}"
    );
    assert!(page.contains("near Bergheim"), "{page}");
    // The newer neighbour (the MPEG-4 clip, dated by file time) has no
    // poster: the viewer must not prefetch the whole video instead.
    assert!(
        !page.contains(&format!("href=\"original/{old}\"")),
        "{page}"
    );
    // No poster: the video still plays, without a poster attribute.
    let (_, _, body) = get(&app, &format!("/v/p1/{old}"), None).await;
    let page = String::from_utf8(body).unwrap();
    assert!(
        page.contains(&format!("src=\"original/{old}\"></video>")),
        "{page}"
    );

    let len = HEVC10_ROT90.len() as u64;
    let (st, h, body) = get(&app, &format!("/original/{mov}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["content-type"], "video/quicktime");
    assert_eq!(h["accept-ranges"], "bytes");
    assert_eq!(body, HEVC10_ROT90);
    let (st, h, body) = get(&app, &format!("/original/{mov}"), Some("bytes=100-199")).await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);
    assert_eq!(h["content-range"], format!("bytes 100-199/{len}").as_str());
    assert_eq!(body, &HEVC10_ROT90[100..200]);
    let (st, _, body) = get(&app, &format!("/original/{mov}"), Some("bytes=-10")).await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, &HEVC10_ROT90[HEVC10_ROT90.len() - 10..]);
    let (st, h, _) = get(
        &app,
        &format!("/original/{mov}"),
        Some(&format!("bytes={len}-")),
    )
    .await;
    assert_eq!(st, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(h["content-range"], format!("bytes */{len}").as_str());
}

#[test]
fn range_headers() {
    let r = |h: &str| web::byte_range(Some(h), 1000);
    assert_eq!(web::byte_range(None, 1000), ByteRange::Whole);
    assert_eq!(r("bytes=0-0"), ByteRange::Part(0, 0));
    assert_eq!(r("bytes=10-"), ByteRange::Part(10, 999));
    assert_eq!(r("bytes=990-5000"), ByteRange::Part(990, 999));
    assert_eq!(r("bytes=-100"), ByteRange::Part(900, 999));
    assert_eq!(r("bytes=-5000"), ByteRange::Part(0, 999));
    assert_eq!(r("bytes=1000-"), ByteRange::Unsatisfiable);
    assert_eq!(r("bytes=-0"), ByteRange::Unsatisfiable);
    assert_eq!(r("bytes=0-1,5-6"), ByteRange::Whole);
    assert_eq!(r("bytes=5-1"), ByteRange::Whole);
    assert_eq!(r("bytes=x-1"), ByteRange::Whole);
    assert_eq!(r("items=0-1"), ByteRange::Whole);
    assert_eq!(r("bytes=+1-2"), ByteRange::Whole);
}

// A video larger than the pixel limit is skipped before any frame is
// decoded (SSC-005).
// rivet: verifies REQ-MEDIA-001
#[test]
fn videos_over_the_pixel_limit_are_skipped() {
    let e = env_with(10_000);
    let idx = Index::open(&e.cfg.data_dir).unwrap();
    let report = scan::run(&e.cfg, &idx, &opts()).unwrap();
    let r = &report.roots[0];
    assert!(report.reconciled, "{:?}", report.problems);
    assert_eq!((r.indexed, r.skipped), (0, 4));
    assert_eq!(
        r.skipped_by_reason.get("over size limit"),
        Some(&3),
        "{:?}",
        r.skipped_by_reason
    );
}

// A decoder that crashes costs the poster only: the videos stay indexed
// and playable, counted under their codec (DD-016).
// rivet: verifies REQ-MEDIA-001
#[test]
fn a_failing_decoder_costs_only_the_poster() {
    let e = env();
    let idx = Index::open(&e.cfg.data_dir).unwrap();
    let opts = Options {
        keyframe_decoder: Some("/usr/bin/false".into()),
        ..Options::default()
    };
    let report = scan::run(&e.cfg, &idx, &opts).unwrap();
    let r = &report.roots[0];
    assert!(report.reconciled, "{:?}", report.problems);
    assert_eq!((r.indexed, r.skipped), (3, 1));
    assert_eq!(r.thumbnails.generated, 0);
    for codec in ["h264", "hevc", "mp4v"] {
        assert_eq!(
            r.thumbnails.failed.get(&format!("decode-error ({codec})")),
            Some(&1),
            "{:?}",
            r.thumbnails.failed
        );
    }
}

// A photo and the short clip under its name are one Live Photo: the clip
// leaves the timeline and plays from the photo's viewer; without its
// photo it is a video of its own again.
// rivet: verifies REQ-MEDIA-005
#[tokio::test]
async fn a_live_photo_is_one_item_with_its_clip() {
    let e = env();
    let root = e.cfg.roots[0].path.clone();
    let photo = root.join("v/IMG_0001.JPG");
    image::RgbImage::from_pixel(64, 48, image::Rgb([200, 120, 40]))
        .save(&photo)
        .unwrap();
    let idx = Arc::new(Index::open(&e.cfg.data_dir).unwrap());
    let report = scan::run(&e.cfg, &idx, &opts()).unwrap();
    assert!(report.reconciled, "{:?}", report.problems);
    assert_eq!(report.live_photos, 1);
    assert!(report.to_text().contains("live photos: 1"));
    let (p, c) = (id_of(&idx, "v/IMG_0001.JPG"), id_of(&idx, "v/IMG_0001.MOV"));
    assert_eq!(record(&idx, "v/IMG_0001.JPG").live, Some(c));
    assert_eq!(record(&idx, "v/IMG_0001.MOV").live_of, Some(p));
    // The longer clips stay videos of their own.
    assert_eq!(record(&idx, "v/clip-h264.mp4").live_of, None);

    let app = web::router(AppState::new(&e.cfg, idx.clone()));
    let (_, _, body) = get(&app, "/t/p1", None).await;
    let page = String::from_utf8(body).unwrap();
    assert!(page.contains(&format!("id=\"i{p}\"")), "{page}");
    assert!(
        !page.contains(&format!("id=\"i{c}\"")),
        "the clip is no item of its own"
    );
    assert!(page.contains("class=\"livetag\""));
    let (_, _, body) = get(&app, &format!("/v/p1/{p}"), None).await;
    let viewer = String::from_utf8(body).unwrap();
    assert!(
        viewer.contains(&format!("src=\"original/{c}\"")),
        "{viewer}"
    );
    assert!(viewer.contains("class=\"livebtn\""));
    let (st, _, _) = get(&app, &format!("/original/{c}"), Some("bytes=0-99")).await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);

    // A rescan keeps the pair; the photo gone, the clip is a video again.
    assert_eq!(scan::run(&e.cfg, &idx, &opts()).unwrap().live_photos, 1);
    std::fs::remove_file(&photo).unwrap();
    assert_eq!(scan::run(&e.cfg, &idx, &opts()).unwrap().live_photos, 0);
    assert_eq!(record(&idx, "v/IMG_0001.MOV").live_of, None);
    let app = web::router(AppState::new(&e.cfg, idx.clone()));
    let (_, _, body) = get(&app, "/t/p1", None).await;
    assert!(String::from_utf8(body)
        .unwrap()
        .contains(&format!("id=\"i{c}\"")));
}
