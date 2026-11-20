//! Faces in photos (REQ-FACE-001, REQ-FACE-002, DD-025): YuNet finds faces
//! and five landmarks each, every face is turned upright onto the ArcFace
//! 112×112 template, and AuraFace gives it a 512-d vector. Both run by rten
//! in a `faces` child process per photo, after the look pass, so a crash
//! or a hang stays in the child. Results are stored per content hash with
//! VERSION, so each photo is looked at for faces once.
//!
//! The two model files sit next to the other vision models
//! (LICHTBILD_VISION_DIR), pinned by SHA-256; without them there are no
//! faces and nothing else changes.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use image::RgbImage;
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use serde::{Deserialize, Serialize};

/// The model files and their hashes: YuNet 2026may (opencv_zoo 48b10fa5,
/// MIT) and AuraFace v1 (fal/AuraFace-v1 af6d057c, Apache-2.0).
pub const FILES: &[(&str, &str)] = &[
    (
        "faces-detect.onnx",
        "ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0",
    ),
    (
        "faces-vector.onnx",
        "a7933ea5330113b01c9b60351d8f4c33003f145d8470ac5f0e52ee2effe25c60",
    ),
];

/// The version of the face models and rules; a new one looks again.
pub const VERSION: &str = "faces-1";
/// Components of a face vector.
pub const DIM: usize = 512;
/// Kept from this detector score on.
pub const MIN_SCORE: f32 = 0.8;
/// Faces narrower than this (pixels of the working picture) are too small
/// to tell people apart and are not kept.
pub const MIN_SIDE: f32 = 36.0;
/// At most this many faces per photo (a class photo).
pub const MAX: usize = 60;
/// YuNet finds faces of about 10 to 300 pixels, so a photo is searched at
/// two sizes: close-ups at the small one, faces in a crowd at the large.
const SCALES: [u32; 2] = [640, 1280];
/// Wall clock per photo.
pub const TIME_BUDGET: Duration = Duration::from_secs(60);

/// One face in a photo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Face {
    /// x, y, width, height as shares of the upright picture.
    pub b: [f32; 4],
    pub score: f32,
    /// The face's vector (DIM signed bytes, embed::quantize).
    pub v: Vec<i8>,
}

/// What the child returns: the faces, and a small square JPEG of each
/// for the people pages (CROP pixels).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub faces: Vec<Face>,
    pub crops: Vec<Vec<u8>>,
}

/// The side of a face's picture, in pixels.
pub const CROP: u32 = 160;

/// Where a face's picture is kept: faces/<2>/<hash>-<n>.jpg.
pub fn crop_path(data_dir: &Path, hash: &str, n: u32) -> std::path::PathBuf {
    data_dir
        .join("faces")
        .join(&hash[..2.min(hash.len())])
        .join(format!("{hash}-{n}.jpg"))
}

/// A square around the face, half again its size, as a CROP-pixel JPEG.
fn crop(img: &RgbImage, b: &[f32; 4]) -> Result<Vec<u8>> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let side = (b[2].max(b[3]) * 1.5).min(w).min(h).max(1.0);
    let cx = b[0] + b[2] / 2.0;
    let cy = b[1] + b[3] / 2.0;
    let x = (cx - side / 2.0).clamp(0.0, w - side) as u32;
    let y = (cy - side / 2.0).clamp(0.0, h - side) as u32;
    let s = side as u32;
    let part = image::imageops::crop_imm(img, x, y, s.max(1), s.max(1)).to_image();
    let small = image::imageops::resize(&part, CROP, CROP, image::imageops::FilterType::Triangle);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82).encode_image(&small)?;
    Ok(out)
}

/// Whether the face models are there: Ok(true) with both files pinned,
/// Ok(false) with neither, an error with one missing or a wrong one.
pub fn present(dir: &Path) -> Result<bool> {
    let mut found = 0;
    for (name, sum) in FILES {
        let p = dir.join(name);
        if !p.exists() {
            continue;
        }
        let h = sha256_hex(&std::fs::read(&p).with_context(|| format!("{}", p.display()))?);
        if h != *sum {
            bail!("{}: unknown model file (sha256 {h})", p.display());
        }
        found += 1;
    }
    match found {
        0 => Ok(false),
        n if n == FILES.len() => Ok(true),
        _ => bail!("{}: only some of the face model files", dir.display()),
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

fn read_pinned(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let (_, sum) = FILES
        .iter()
        .find(|(n, _)| *n == name)
        .context("not a face model file")?;
    let p = dir.join(name);
    let data = std::fs::read(&p).with_context(|| format!("face model: {}", p.display()))?;
    if sha256_hex(&data) != *sum {
        bail!("{}: unknown model file", p.display());
    }
    Ok(data)
}

/// A face as the detector sees it, in picture pixels.
#[derive(Debug, Clone)]
struct Hit {
    score: f32,
    b: [f32; 4],
    k: [[f32; 2]; 5],
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = (a[0] + a[2]).min(b[0] + b[2]);
    let y2 = (a[1] + a[3]).min(b[1] + b[3]);
    let i = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    i / (a[2] * a[3] + b[2] * b[3] - i).max(1e-9)
}

pub struct Finder {
    detect: rten::Model,
    /// Loaded on the first face: most photos have none, and the vector
    /// model is 250 MB to read and check.
    vector: std::sync::OnceLock<rten::Model>,
    dir: std::path::PathBuf,
}

impl Finder {
    pub fn load(dir: &Path) -> Result<Finder> {
        Ok(Finder {
            detect: rten::Model::load(read_pinned(dir, "faces-detect.onnx")?)?,
            vector: std::sync::OnceLock::new(),
            dir: dir.to_path_buf(),
        })
    }

    fn vector_model(&self) -> Result<&rten::Model> {
        if let Some(m) = self.vector.get() {
            return Ok(m);
        }
        let m = rten::Model::load(read_pinned(&self.dir, "faces-vector.onnx")?)?;
        Ok(self.vector.get_or_init(|| m))
    }

    /// YuNet at one size: the picture scaled so its longer side is `side`,
    /// padded to a multiple of 32, BGR 0..255.
    fn detect_at(&self, img: &RgbImage, side: u32) -> Result<Vec<Hit>> {
        let (w, h) = img.dimensions();
        let s = (side as f32 / w.max(h) as f32).min(1.0);
        let (rw, rh) = (
            ((w as f32 * s).round() as u32).max(1),
            ((h as f32 * s).round() as u32).max(1),
        );
        let r = image::imageops::resize(img, rw, rh, image::imageops::FilterType::Triangle);
        let (pw, ph) = (rw.div_ceil(32) * 32, rh.div_ceil(32) * 32);
        let mut t = NdTensor::<f32, 4>::zeros([1, 3, ph as usize, pw as usize]);
        for (x, y, p) in r.enumerate_pixels() {
            t[[0, 0, y as usize, x as usize]] = p[2] as f32;
            t[[0, 1, y as usize, x as usize]] = p[1] as f32;
            t[[0, 2, y as usize, x as usize]] = p[0] as f32;
        }
        let m = &self.detect;
        let names = [
            "cls_8", "cls_16", "cls_32", "obj_8", "obj_16", "obj_32", "bbox_8", "bbox_16",
            "bbox_32", "kps_8", "kps_16", "kps_32",
        ];
        let ids: Vec<_> = names
            .iter()
            .map(|n| m.node_id(n))
            .collect::<Result<_, _>>()?;
        let input = *m.input_ids().first().context("detector input")?;
        let out = m.run(vec![(input, t.view().into())], &ids, None)?;
        let v: Vec<Vec<f32>> = out
            .into_iter()
            .map(|o| -> Result<Vec<f32>> {
                let t: Tensor<f32> = o.try_into()?;
                Ok(t.to_vec())
            })
            .collect::<Result<_>>()?;
        let mut faces = Vec::new();
        for (i, stride) in [8usize, 16, 32].iter().enumerate() {
            let (cols, rows) = (pw as usize / stride, ph as usize / stride);
            let (cls, obj, bb, kp) = (&v[i], &v[3 + i], &v[6 + i], &v[9 + i]);
            if cls.len() < rows * cols || bb.len() < rows * cols * 4 || kp.len() < rows * cols * 10
            {
                bail!("detector output size");
            }
            let st = *stride as f32;
            for r_ in 0..rows {
                for c in 0..cols {
                    let idx = r_ * cols + c;
                    let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                    if score < MIN_SCORE {
                        continue;
                    }
                    let cx = (c as f32 + bb[idx * 4]) * st;
                    let cy = (r_ as f32 + bb[idx * 4 + 1]) * st;
                    let bw = bb[idx * 4 + 2].exp() * st;
                    let bh = bb[idx * 4 + 3].exp() * st;
                    let mut k = [[0f32; 2]; 5];
                    for (n, kn) in k.iter_mut().enumerate() {
                        *kn = [
                            (kp[idx * 10 + 2 * n] + c as f32) * st / s,
                            (kp[idx * 10 + 2 * n + 1] + r_ as f32) * st / s,
                        ];
                    }
                    faces.push(Hit {
                        score,
                        b: [(cx - bw / 2.0) / s, (cy - bh / 2.0) / s, bw / s, bh / s],
                        k,
                    });
                }
            }
        }
        Ok(faces)
    }

    /// The faces in a picture, best first, each with its vector and
    /// picture.
    pub fn find(&self, img: &RgbImage) -> Result<Found> {
        let (w, h) = img.dimensions();
        let mut all = Vec::new();
        for side in SCALES {
            all.extend(self.detect_at(img, side)?);
            if w.max(h) <= side {
                break;
            }
        }
        all.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut keep: Vec<Hit> = Vec::new();
        for f in all {
            if f.b[2] >= MIN_SIDE && keep.iter().all(|k| iou(&k.b, &f.b) < 0.3) {
                keep.push(f);
            }
            if keep.len() == MAX {
                break;
            }
        }
        let mut out = Vec::with_capacity(keep.len());
        let mut crops = Vec::with_capacity(keep.len());
        for f in keep {
            let v = self.vector_of(img, &f)?;
            crops.push(crop(img, &f.b)?);
            out.push(Face {
                b: [
                    (f.b[0] / w as f32).clamp(0.0, 1.0),
                    (f.b[1] / h as f32).clamp(0.0, 1.0),
                    (f.b[2] / w as f32).clamp(0.0, 1.0),
                    (f.b[3] / h as f32).clamp(0.0, 1.0),
                ],
                score: f.score,
                v,
            });
        }
        Ok(Found { faces: out, crops })
    }

    fn vector_of(&self, img: &RgbImage, f: &Hit) -> Result<Vec<i8>> {
        let x = align(img, &f.k).map(|p| (p - 127.5) / 127.5);
        let m = self.vector_model()?;
        let input = *m.input_ids().first().context("vector input")?;
        let output = *m.output_ids().first().context("vector output")?;
        let out: Tensor<f32> = m
            .run(vec![(input, x.view().into())], &[output], None)?
            .into_iter()
            .next()
            .context("vector")?
            .try_into()?;
        let v = out.to_vec();
        if v.len() != DIM {
            bail!("face vector of {} components", v.len());
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        let unit: Vec<f32> = v.iter().map(|x| x / n).collect();
        Ok(crate::embed::quantize(&unit))
    }
}

/// Where ArcFace-style models expect the eyes, nose tip and mouth corners
/// in a 112×112 face.
const TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// The similarity transform (rotation, scale, shift; Umeyama without
/// reflection) taking `src` closest to `dst`: [a, -b, tx, b, a, ty].
pub fn similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [f32; 6] {
    let (mut sx, mut sy, mut dx, mut dy) = (0.0, 0.0, 0.0, 0.0);
    for i in 0..5 {
        sx += src[i][0];
        sy += src[i][1];
        dx += dst[i][0];
        dy += dst[i][1];
    }
    let (sx, sy, dx, dy) = (sx / 5.0, sy / 5.0, dx / 5.0, dy / 5.0);
    let (mut a, mut b, mut var) = (0.0, 0.0, 0.0);
    for i in 0..5 {
        let (px, py) = (src[i][0] - sx, src[i][1] - sy);
        let (qx, qy) = (dst[i][0] - dx, dst[i][1] - dy);
        a += px * qx + py * qy;
        b += px * qy - py * qx;
        var += px * px + py * py;
    }
    let var = var.max(1e-9);
    let (c, s) = (a / var, b / var);
    [c, -s, dx - (c * sx - s * sy), s, c, dy - (s * sx + c * sy)]
}

/// The face cut out upright onto the template, RGB 0..255, NCHW.
fn align(img: &RgbImage, k: &[[f32; 2]; 5]) -> NdTensor<f32, 4> {
    let m = similarity(k, &TEMPLATE);
    let (c, s, tx, ty) = (m[0], m[3], m[2], m[5]);
    let d = (c * c + s * s).max(1e-12);
    let (w, h) = (img.width() as f32, img.height() as f32);
    let mut t = NdTensor::<f32, 4>::zeros([1, 3, 112, 112]);
    for y in 0..112 {
        for x in 0..112 {
            let (u, v) = (x as f32 - tx, y as f32 - ty);
            let sx = (c * u + s * v) / d;
            let sy = (-s * u + c * v) / d;
            if !(0.0..=w - 1.0).contains(&sx) || !(0.0..=h - 1.0).contains(&sy) {
                continue;
            }
            let (x0, y0) = (sx.floor() as u32, sy.floor() as u32);
            let (x1, y1) = (
                (x0 + 1).min(img.width() - 1),
                (y0 + 1).min(img.height() - 1),
            );
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            for ch in 0..3 {
                let p = |xx, yy| img.get_pixel(xx, yy)[ch] as f32;
                t[[0, ch, y, x]] = p(x0, y0) * (1.0 - fx) * (1.0 - fy)
                    + p(x1, y0) * fx * (1.0 - fy)
                    + p(x0, y1) * (1.0 - fx) * fy
                    + p(x1, y1) * fx * fy;
            }
        }
    }
    t
}

/// Cosine of two face vectors.
pub fn cos(a: &[i8], b: &[i8]) -> f32 {
    let dot: i32 = a.iter().zip(b).map(|(x, y)| *x as i32 * *y as i32).sum();
    let n = |v: &[i8]| {
        v.iter()
            .map(|x| (*x as i32 * *x as i32) as f32)
            .sum::<f32>()
            .sqrt()
            .max(1e-9)
    };
    dot as f32 / (n(a) * n(b))
}

/// The `faces` child: `lichtbild faces <model-dir> <file>`.
pub fn child_main(args: &[&str]) -> Result<()> {
    let [model_dir, file] = args else {
        bail!("usage: faces <model-dir> <file>");
    };
    crate::vision::limit_self(TIME_BUDGET.as_secs() * 4);
    let img = crate::vision::load_picture(Path::new(file), 256 << 20)?;
    let faces = Finder::load(Path::new(model_dir))?.find(&img)?;
    serde_json::to_writer(std::io::stdout().lock(), &faces)?;
    Ok(())
}

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct PassReport {
    pub looked: u64,
    pub faces: u64,
    pub failed: u64,
    pub remaining: u64,
}

impl PassReport {
    pub fn to_text(&self) -> String {
        format!(
            "looked for faces in {} photos: {} faces, {} failed, {} left for the next pass",
            self.looked, self.faces, self.failed, self.remaining
        )
    }
}

/// Look for faces in the photos not yet looked at with VERSION, newest
/// first, one child at a time, until `deadline`. Every result, failures
/// included, is stored under the photo's content hash, so the pass resumes.
pub fn run_pass(
    cfg: &crate::config::Config,
    idx: &crate::index::Index,
    exe: &Path,
    deadline: Option<Instant>,
    progress: &dyn Fn(u64, u64, &PassReport),
) -> Result<PassReport> {
    let mut rep = PassReport::default();
    let Some(dir) = &cfg.vision_dir else {
        return Ok(rep);
    };
    if !present(dir)? {
        return Ok(rep);
    }
    let todo = idx.photos_for_faces(VERSION)?;
    let total = todo.len() as u64;
    if total > 0 {
        tracing::info!("looking for faces in {total} photos");
    }
    let started = Instant::now();
    let mut said = Instant::now();
    for (n, (_, rec)) in todo.into_iter().enumerate() {
        if n > 0 && said.elapsed() >= Duration::from_secs(600) {
            said = Instant::now();
            tracing::info!(
                "looked for faces in {n} of {total} photos ({} faces, {} failed) in {} min",
                rep.faces,
                rep.failed,
                started.elapsed().as_secs() / 60
            );
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            rep.remaining = total - n as u64;
            break;
        }
        let Some(root) = cfg.roots.iter().find(|r| r.label == rec.label) else {
            continue;
        };
        let rel = crate::index::rel_from_bytes(&rec.rel);
        // SSC-004: the stored path must still resolve inside its root.
        let Ok(path) = crate::source::resolve(&root.path, &rel) else {
            continue;
        };
        match crate::vision::run_child_as::<Found>(exe, "faces", &path, dir, TIME_BUDGET) {
            Ok(found) => {
                rep.looked += 1;
                rep.faces += found.faces.len() as u64;
                for (n, jpeg) in found.crops.iter().enumerate() {
                    let p = crop_path(&cfg.data_dir, &rec.hash, n as u32);
                    crate::datadir::write_atomic(&p, jpeg)?;
                }
                idx.set_faces(&rec.hash, VERSION, &found.faces)?;
            }
            Err(e) => {
                rep.failed += 1;
                idx.set_faces_failed(&rec.hash, VERSION, &crate::vision::short_reason(&e))?;
            }
        }
        progress(n as u64 + 1, total, &rep);
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_maps_onto_itself() {
        let m = similarity(&TEMPLATE, &TEMPLATE);
        assert!((m[0] - 1.0).abs() < 1e-4 && m[3].abs() < 1e-4);
        assert!(m[2].abs() < 1e-3 && m[5].abs() < 1e-3);
    }

    #[test]
    fn a_turned_and_shrunk_face_is_turned_back() {
        // The template turned by 30°, halved and moved.
        let (c, s) = (
            30f32.to_radians().cos() * 0.5,
            30f32.to_radians().sin() * 0.5,
        );
        let mut src = TEMPLATE;
        for p in src.iter_mut() {
            *p = [c * p[0] - s * p[1] + 200.0, s * p[0] + c * p[1] + 50.0];
        }
        let m = similarity(&src, &TEMPLATE);
        for (p, q) in src.iter().zip(TEMPLATE.iter()) {
            let x = m[0] * p[0] + m[1] * p[1] + m[2];
            let y = m[3] * p[0] + m[4] * p[1] + m[5];
            assert!(
                (x - q[0]).abs() < 0.01 && (y - q[1]).abs() < 0.01,
                "{x},{y} vs {q:?}"
            );
        }
    }

    #[test]
    fn face_models_are_all_there_or_none() {
        let d = tempfile::tempdir().unwrap();
        assert!(!present(d.path()).unwrap());
        crate::datadir::write_atomic(&d.path().join("faces-detect.onnx"), b"not the model")
            .unwrap();
        let e = present(d.path()).unwrap_err().to_string();
        assert!(e.contains("unknown model file"), "{e}");
    }

    #[test]
    fn overlap_of_boxes() {
        assert!((iou(&[0.0, 0.0, 10.0, 10.0], &[0.0, 0.0, 10.0, 10.0]) - 1.0).abs() < 1e-6);
        assert_eq!(iou(&[0.0, 0.0, 10.0, 10.0], &[20.0, 20.0, 5.0, 5.0]), 0.0);
    }
}
