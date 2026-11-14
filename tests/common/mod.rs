//! Synthetic fixtures (REQ-PRIV-001): every image is generated here; the
//! coordinates and dates are made up.
#![allow(clippy::disallowed_methods)] // tests build fixture trees (DD-006)
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use exif::experimental::Writer;
use exif::{Field, In, Rational, Tag, Value};
use image::{ImageBuffer, Rgb};
use lichtbild::config::{Config, Limits, Settings};
use sha2::{Digest, Sha256};

pub fn jpeg(w: u32, h: u32, seed: u8) -> Vec<u8> {
    let img = ImageBuffer::from_fn(w, h, |x, y| {
        Rgb([seed, (x * 7) as u8 ^ seed, (y * 13) as u8])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&img)
        .unwrap();
    out
}

pub fn png(w: u32, h: u32, seed: u8) -> Vec<u8> {
    let img = ImageBuffer::from_fn(w, h, |x, y| Rgb([seed, x as u8, y as u8]));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn ascii(tag: Tag, s: &str) -> Field {
    Field {
        tag,
        ifd_num: In::PRIMARY,
        value: Value::Ascii(vec![s.as_bytes().to_vec()]),
    }
}

fn dms(deg: u32, min: u32, sec: u32) -> Value {
    Value::Rational(vec![
        Rational { num: deg, denom: 1 },
        Rational { num: min, denom: 1 },
        Rational { num: sec, denom: 1 },
    ])
}

/// Degrees, minutes, hemisphere letter.
pub type Dm = (u32, u32, char);

/// A JPEG with EXIF DateTimeOriginal, OffsetTimeOriginal and optionally GPS.
pub fn jpeg_with_exif(date: &str, offset: &str, gps: Option<(Dm, Dm)>, seed: u8) -> Vec<u8> {
    let mut fields = vec![
        ascii(Tag::DateTimeOriginal, date),
        ascii(Tag::OffsetTimeOriginal, offset),
        // A made-up camera, for the viewer's info panel.
        ascii(Tag::Make, "Lichtbild"),
        ascii(Tag::Model, "Testkamera 1"),
        Field {
            tag: Tag::ExposureTime,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![Rational { num: 1, denom: 120 }]),
        },
        Field {
            tag: Tag::FNumber,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![Rational { num: 18, denom: 10 }]),
        },
        Field {
            tag: Tag::PhotographicSensitivity,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![50]),
        },
        Field {
            tag: Tag::FocalLength,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![Rational { num: 42, denom: 10 }]),
        },
    ];
    if let Some(((la, lam, lar), (lo, lom, lor))) = gps {
        fields.push(ascii(Tag::GPSLatitudeRef, &lar.to_string()));
        fields.push(Field {
            tag: Tag::GPSLatitude,
            ifd_num: In::PRIMARY,
            value: dms(la, lam, 0),
        });
        fields.push(ascii(Tag::GPSLongitudeRef, &lor.to_string()));
        fields.push(Field {
            tag: Tag::GPSLongitude,
            ifd_num: In::PRIMARY,
            value: dms(lo, lom, 0),
        });
    }
    let mut w = Writer::new();
    for f in &fields {
        w.push_field(f);
    }
    let mut tiff = Cursor::new(Vec::new());
    w.write(&mut tiff, false).unwrap();
    let tiff = tiff.into_inner();
    let base = jpeg(64, 48, seed);
    let mut out = base[..2].to_vec(); // SOI
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
    out.extend_from_slice(b"Exif\0\0");
    out.extend_from_slice(&tiff);
    out.extend_from_slice(&base[2..]);
    out
}

pub struct Fixture {
    pub _tmp: tempfile::TempDir,
    pub root1: PathBuf,
    pub root2: PathBuf,
    pub outside: PathBuf,
    pub data: PathBuf,
}

pub const HEIC_640X480: &[u8] = include_bytes!("../fixtures/synthetic-640x480.heic");

/// Root p1 holds 14 entries: 5 that index (one HEIC) and 9 that are skipped,
/// one per reason. Root p2 holds one JPEG.
pub fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root1 = base.join("lib/p1");
    let root2 = base.join("lib/p2");
    let outside = base.join("outside");
    let data = base.join("data");
    for d in [&root1.join("a"), &root1.join("b"), &root2, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }
    let w = |p: &Path, b: &[u8]| std::fs::write(p, b).unwrap();
    w(
        &root1.join("a/IMG_exif.jpg"),
        &jpeg_with_exif(
            "2021:06:15 10:11:12",
            "+02:00",
            Some(((10, 30, 'N'), (20, 15, 'E'))),
            1,
        ),
    );
    w(&root1.join("a/IMG_20200102_030405.jpg"), &jpeg(40, 30, 2));
    w(&root1.join("a/plain.png"), &png(32, 24, 3));
    w(&root1.join("b/clip.mp4"), b"\0\0\0\x18ftypmp42synthetic");
    // Synthetic HEIC made with macOS sips from a generated gradient
    // (tests/fixtures/README.md); 640x480, stored as a 2x1 tile grid.
    w(&root1.join("b/IMG_20190704_102030.HEIC"), HEIC_640X480);
    w(
        &root1.join("b/broken.heic"),
        b"\0\0\0\x18ftypheicnot really heic",
    );
    w(&root1.join("b/notes.txt"), b"synthetic");
    w(&root1.join(".DS_Store"), b"synthetic");
    w(&root1.join("b/broken.jpg"), b"\xFF\xD8not really a jpeg");
    w(&root1.join("b/huge.png"), &png(700, 700, 4)); // over the test pixel limit
    w(&outside.join("secret.jpg"), &jpeg(8, 8, 5));
    std::os::unix::fs::symlink(outside.join("secret.jpg"), root1.join("link_out.jpg")).unwrap();
    std::os::unix::fs::symlink(root1.join("a/plain.png"), root1.join("link_in.png")).unwrap();
    std::os::unix::fs::symlink(root1.join("a"), root1.join("link_dir")).unwrap();
    std::os::unix::fs::symlink(root1.join("gone.jpg"), root1.join("broken_link.jpg")).unwrap();
    w(&root2.join("IMG_20220303_101010.jpg"), &jpeg(20, 20, 6));
    Fixture {
        _tmp: tmp,
        root1,
        root2,
        outside,
        data,
    }
}

/// The deployment creates the data directory; tests do the same.
pub fn config(fx: &Fixture, data: &Path) -> Config {
    std::fs::create_dir_all(data).unwrap();
    Settings {
        roots: vec![
            ("p1".into(), fx.root1.clone()),
            ("p2".into(), fx.root2.clone()),
        ],
        data_dir: Some(data.to_path_buf()),
        workers: Some(2),
        limits: Limits {
            max_pixels: 400_000,
            ..Limits::default()
        },
        ..Settings::default()
    }
    .resolve()
    .unwrap()
}

/// Everything about a tree that a write could change: listing, type, size,
/// mode, mtime, content hash, link target.
pub fn snapshot(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let m = std::fs::symlink_metadata(&d).unwrap();
        out.insert(
            d.strip_prefix(root).unwrap().to_path_buf(),
            format!(
                "dir mode={:o} mtime={}",
                m.mode(),
                m.mtime_nsec() + m.mtime() * 1_000_000_000
            ),
        );
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            if m.is_dir() {
                stack.push(p);
            } else if m.file_type().is_symlink() {
                out.insert(
                    rel,
                    format!("link -> {}", std::fs::read_link(&p).unwrap().display()),
                );
            } else {
                let h = Sha256::digest(std::fs::read(&p).unwrap());
                out.insert(
                    rel,
                    format!(
                        "file size={} mode={:o} mtime={} sha256={:x}",
                        m.len(),
                        m.mode(),
                        m.mtime() * 1_000_000_000 + m.mtime_nsec(),
                        h
                    ),
                );
            }
        }
    }
    out
}

pub fn set_readonly(root: &Path, ro: bool) {
    let mut all = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                stack.push(p.clone());
            }
            if !m.file_type().is_symlink() {
                all.push(p);
            }
        }
    }
    all.push(root.to_path_buf());
    for p in all {
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        let mode = perm.mode();
        perm.set_mode(if ro { mode & !0o222 } else { mode | 0o200 });
        std::fs::set_permissions(&p, perm).unwrap();
    }
}

/// Open the index, scan once, close the index.
pub fn scan_once(cfg: &Config, opts: &lichtbild::scan::Options) -> lichtbild::scan::Report {
    let idx = lichtbild::index::Index::open(&cfg.data_dir).unwrap();
    lichtbild::scan::run(cfg, &idx, opts).unwrap()
}

/// Every record as comparable text, keyed by root label and path (ids left
/// out: they depend on worker order).
pub fn projection(data: &Path) -> BTreeMap<String, String> {
    let idx = lichtbild::index::Index::open(data).unwrap();
    idx.all()
        .unwrap()
        .into_iter()
        .map(|(_, r)| {
            let key = format!("{}:{}", r.label, String::from_utf8_lossy(&r.rel));
            let val = format!(
                "{}|{}|{}|{}|{}|{}|{:?}|{}|{:?}|{:?}|{}|{}",
                r.size,
                r.mtime_ns,
                r.hash,
                r.width,
                r.height,
                r.taken_local,
                r.taken_offset,
                r.date_source,
                r.lat,
                r.lon,
                r.thumb,
                r.missing as u8,
            );
            (key, val)
        })
        .collect()
}
