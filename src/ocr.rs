//! Text in pictures (REQ-TEXT-001, DD-023): find and read the text in
//! photos, in the house, with PaddleOCR models run by rten (pure Rust).
//!
//! A child process (`lichtbild read-text`) reads one picture, like the
//! transcriber: a picture that crashes or hangs the reader cannot take the
//! server with it. It returns lines of text with their corners on the
//! picture as shown (oriented), as fractions of its width and height,
//! grouped into blocks (paragraphs) and blocks into documents (a photo of
//! two letters on a table holds two). The result is stored per content
//! hash, so each picture is read once per model.
//!
//! The engine, measured in spikes/ocr-bench: PP-OCRv3 mobile detection,
//! PP-OCRv5 Latin recognition (ä ö ü ß included). A tilted page is
//! straightened before reading, and line crops are padded by 2.5 times
//! the DB "unclip" distance, which keeps the dots of the umlauts.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use image::{Rgb, RgbImage};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use serde::{Deserialize, Serialize};

/// The model files, pinned by SHA-256 (spikes/ocr-bench/SHA256SUMS.models):
/// from huggingface.co/monkt/paddleocr-onnx at revision 7b02d0a3,
/// PaddleOCR models, Apache-2.0.
pub const FILES: &[(&str, &str)] = &[
    (
        "det.onnx",
        "ee40e80071ba3a320d4efda75f3e22047a7d049e9bf7bcaaf9daea23fc21b935",
    ),
    (
        "rec.onnx",
        "614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409",
    ),
    (
        "dict.txt",
        "3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff",
    ),
];

/// The name stored with each result; a new model reads everything again.
pub const MODEL: &str = "ppocr-v3det-v5latin";

/// Pictures are read at most this large (longer side, in pixels).
pub const WORK_SIDE: u32 = 2400;
/// Detection runs at most this large (longer side).
const DET_SIDE: f32 = 960.0;
/// Crop padding: the DB unclip ratio for axis-aligned boxes.
const UNCLIP: f32 = 2.5;
/// Pages tilted more than this are straightened and detected again.
const DESKEW_DEG: f32 = 0.7;
/// Wall clock per picture; the Pi 5 needs about 1.5 s for a tilted page.
pub const TIME_BUDGET: Duration = Duration::from_secs(90);

/// One line of text: its corners (top-left, top-right, bottom-right,
/// bottom-left) as fractions of the picture's width and height, its
/// block, and the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub quad: [f32; 8],
    pub block: u16,
    pub text: String,
    /// The recognizer's mean certainty over the characters it read (0..1).
    #[serde(default)]
    pub conf: f32,
}

/// What the child returns, and what the index stores per content hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text {
    /// "ok", "none" (no text found) or "failed (<reason>)".
    pub state: String,
    pub model: String,
    /// Lines in reading order: block by block, top to bottom.
    pub lines: Vec<Line>,
    /// For each block, the document it belongs to.
    pub docs: Vec<u16>,
}

impl Text {
    pub fn failed(reason: &str) -> Text {
        Text {
            state: format!("failed ({reason})"),
            model: MODEL.into(),
            lines: Vec::new(),
            docs: Vec::new(),
        }
    }

    /// How many separate documents (groups of blocks) the picture holds.
    pub fn documents(&self) -> usize {
        self.docs.iter().map(|d| *d as usize + 1).max().unwrap_or(0)
    }

    /// All text for search: a line per line, an empty line between
    /// blocks.
    pub fn plain(&self) -> String {
        let mut s = String::new();
        let mut last = None;
        for l in &self.lines {
            if !s.is_empty() {
                s.push_str(if last == Some(l.block) { "\n" } else { "\n\n" });
            }
            s.push_str(&l.text);
            last = Some(l.block);
        }
        s
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}

/// Check the model directory: every file present with its pinned hash.
pub fn check_model(dir: &Path) -> Result<()> {
    for (name, sum) in FILES {
        let p = dir.join(name);
        let data = std::fs::read(&p).with_context(|| format!("ocr model: {}", p.display()))?;
        let h = sha256_hex(&data);
        if h != *sum {
            bail!("{}: unknown model file (sha256 {h})", p.display());
        }
    }
    Ok(())
}

/// The loaded models.
pub struct Engine {
    det: rten::Model,
    rec: rten::Model,
    dict: Vec<char>,
}

impl Engine {
    /// Load and verify (each file once, hashed in memory).
    pub fn load(dir: &Path) -> Result<Engine> {
        let read = |name: &str| -> Result<Vec<u8>> {
            let data = std::fs::read(dir.join(name)).with_context(|| name.to_string())?;
            let sum = FILES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s);
            if sum != Some(sha256_hex(&data).as_str()) {
                bail!("{name}: unknown model file");
            }
            Ok(data)
        };
        let det = rten::Model::load(read("det.onnx")?).context("det.onnx")?;
        let rec = rten::Model::load(read("rec.onnx")?).context("rec.onnx")?;
        let mut dict: Vec<char> = String::from_utf8(read("dict.txt")?)?
            .lines()
            .filter_map(|l| l.chars().next())
            .collect();
        dict.push(' ');
        Ok(Engine { det, rec, dict })
    }

    /// Read the text in `img` (already oriented).
    pub fn read(&self, img: &RgbImage) -> Result<Text> {
        let (w, h) = img.dimensions();
        let (boxes, skew) = self.detect(img)?;
        let (work, boxes, angle) = if skew.abs() > DESKEW_DEG {
            let straight = rotate(img, -skew.to_radians());
            let (b, _) = self.detect(&straight)?;
            (straight, b, skew.to_radians())
        } else {
            (img.clone(), boxes, 0.0)
        };
        let mut lines = Vec::new();
        for b in boxes {
            let (text, conf) = self.recognize(&work, b)?;
            let text = text.trim().to_string();
            if std::env::var_os("LICHTBILD_OCR_DEBUG").is_some() {
                eprintln!("{conf:.3} {text}");
            }
            if !keep(&text, conf) {
                continue;
            }
            // Corners back onto the picture as shown.
            let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
            let (s, c) = angle.sin_cos();
            let mut quad = [0f32; 8];
            let corners = [
                (b.0 as f32, b.1 as f32),
                (b.2 as f32 + 1.0, b.1 as f32),
                (b.2 as f32 + 1.0, b.3 as f32 + 1.0),
                (b.0 as f32, b.3 as f32 + 1.0),
            ];
            for (i, (x, y)) in corners.into_iter().enumerate() {
                let (dx, dy) = (x - cx, y - cy);
                quad[2 * i] = ((cx + dx * c - dy * s) / w as f32).clamp(0.0, 1.0);
                quad[2 * i + 1] = ((cy + dx * s + dy * c) / h as f32).clamp(0.0, 1.0);
            }
            lines.push(Line {
                quad,
                block: 0,
                text,
                conf,
            });
        }
        let docs = layout(&mut lines);
        Ok(Text {
            state: if lines.is_empty() { "none" } else { "ok" }.into(),
            model: MODEL.into(),
            lines,
            docs,
        })
    }

    /// DB detection: line boxes (x0, y0, x1, y1) in `img` pixels, and the
    /// text's skew in degrees: the median orientation of the wide regions,
    /// from their second moments.
    fn detect(&self, img: &RgbImage) -> Result<(Vec<LineBox>, f32)> {
        let (w, h) = img.dimensions();
        let scale = (DET_SIDE / w.max(h) as f32).min(1.0);
        let rw = ((w as f32 * scale / 32.0).round().max(1.0) * 32.0) as u32;
        let rh = ((h as f32 * scale / 32.0).round().max(1.0) * 32.0) as u32;
        let small = image::imageops::resize(img, rw, rh, image::imageops::FilterType::Triangle);
        let (mean, std) = ([0.485f32, 0.456, 0.406], [0.229f32, 0.224, 0.225]);
        let mut t = NdTensor::<f32, 4>::zeros([1, 3, rh as usize, rw as usize]);
        for (x, y, p) in small.enumerate_pixels() {
            // PaddleOCR's models take BGR.
            let bgr = [p[2], p[1], p[0]];
            for c in 0..3 {
                t[[0, c, y as usize, x as usize]] = (bgr[c] as f32 / 255.0 - mean[c]) / std[c];
            }
        }
        let out: Tensor<f32> = self.det.run_one(t.view().into(), None)?.try_into()?;
        let (rw, rh) = (rw as usize, rh as usize);
        let prob = out.into_shape([rh, rw]);
        let regions = regions(rw, rh, |x, y| prob[[y, x]]);
        let mut angles: Vec<f32> = regions
            .iter()
            .filter(|r| r.n >= 50 && r.x1 - r.x0 >= 40)
            .map(|r| r.angle_deg())
            .collect();
        angles.sort_by(f32::total_cmp);
        let skew = angles.get(angles.len() / 2).copied().unwrap_or(0.0);
        let mut out = Vec::new();
        for r in regions {
            if r.n < 10 || r.sum / r.n as f32 <= 0.6 {
                continue;
            }
            let (bw, bh) = ((r.x1 - r.x0 + 1) as f32, (r.y1 - r.y0 + 1) as f32);
            let d = bw * bh * UNCLIP / (2.0 * (bw + bh));
            let f = |v: f32, max: u32| (v / scale).clamp(0.0, max as f32 - 1.0) as u32;
            out.push((
                f(r.x0 as f32 - d, w),
                f(r.y0 as f32 - d, h),
                f(r.x1 as f32 + d, w),
                f(r.y1 as f32 + d, h),
            ));
        }
        Ok((out, skew))
    }

    /// The text in box `b` and the mean probability of its characters.
    fn recognize(&self, img: &RgbImage, b: (u32, u32, u32, u32)) -> Result<(String, f32)> {
        let (bw, bh) = (b.2 - b.0 + 1, b.3 - b.1 + 1);
        let crop = image::imageops::crop_imm(img, b.0, b.1, bw, bh).to_image();
        let h = 48usize;
        let w = ((bw as f32 * h as f32 / bh as f32).ceil() as usize).clamp(16, 3200);
        let r = image::imageops::resize(
            &crop,
            w as u32,
            h as u32,
            image::imageops::FilterType::Triangle,
        );
        let mut t = NdTensor::<f32, 4>::zeros([1, 3, h, w]);
        for (x, y, p) in r.enumerate_pixels() {
            let bgr = [p[2], p[1], p[0]];
            for c in 0..3 {
                t[[0, c, y as usize, x as usize]] = (bgr[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
        let out: Tensor<f32> = self.rec.run_one(t.view().into(), None)?.try_into()?;
        let shape = out.shape().to_vec();
        let [_, steps, classes] = shape[..] else {
            bail!("recognition output {shape:?}");
        };
        let out = out.into_shape([steps, classes]);
        let mut s = String::new();
        let (mut sum, mut n) = (0f32, 0u32);
        let mut last = 0usize;
        for i in 0..steps {
            let mut best = 0;
            for c in 1..classes {
                if out[[i, c]] > out[[i, best]] {
                    best = c;
                }
            }
            if best != 0 && best != last {
                if let Some(ch) = self.dict.get(best - 1) {
                    s.push(*ch);
                    sum += out[[i, best]];
                    n += 1;
                }
            }
            last = best;
        }
        Ok((s, if n == 0 { 0.0 } else { sum / n as f32 }))
    }
}

/// Whether a recognized line is text worth keeping: at least two letters
/// or digits, and read with certainty. Textures (leaves, gravel, noise)
/// give short lines of low certainty.
fn keep(text: &str, conf: f32) -> bool {
    let alnum = text.chars().filter(|c| c.is_alphanumeric()).count();
    alnum >= 2 && conf >= MIN_CONF && (alnum >= 4 || conf >= MIN_CONF_SHORT)
}

/// Lowest mean certainty of a kept line, and of a line of 2–3 characters.
/// Measured on the synthetic demo set: texture junk 0.10–0.70, real lines
/// 0.966–1.000 (DD-023).
const MIN_CONF: f32 = 0.80;
const MIN_CONF_SHORT: f32 = 0.90;

/// A line's box in picture pixels: x0, y0, x1, y1 (inclusive).
type LineBox = (u32, u32, u32, u32);

/// A connected region of the probability map above 0.3.
struct Region {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    n: usize,
    sum: f32,
    /// Sums of x, y, xx, yy, xy.
    m: [f64; 5],
}

impl Region {
    fn angle_deg(&self) -> f32 {
        let n = self.n as f64;
        let (cx, cy) = (self.m[0] / n, self.m[1] / n);
        let mu20 = self.m[2] / n - cx * cx;
        let mu02 = self.m[3] / n - cy * cy;
        let mu11 = self.m[4] / n - cx * cy;
        (0.5 * (2.0 * mu11).atan2(mu20 - mu02)).to_degrees() as f32
    }
}

/// 8-connected regions of the pixels where `p(x, y) > 0.3`.
fn regions(w: usize, h: usize, p: impl Fn(usize, usize) -> f32) -> Vec<Region> {
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for sy in 0..h {
        for sx in 0..w {
            if seen[sy * w + sx] || p(sx, sy) <= 0.3 {
                continue;
            }
            let mut r = Region {
                x0: sx,
                y0: sy,
                x1: sx,
                y1: sy,
                n: 0,
                sum: 0.0,
                m: [0.0; 5],
            };
            seen[sy * w + sx] = true;
            stack.push((sx, sy));
            while let Some((x, y)) = stack.pop() {
                let v = p(x, y);
                r.n += 1;
                r.sum += v;
                r.x0 = r.x0.min(x);
                r.y0 = r.y0.min(y);
                r.x1 = r.x1.max(x);
                r.y1 = r.y1.max(y);
                let (fx, fy) = (x as f64, y as f64);
                r.m[0] += fx;
                r.m[1] += fy;
                r.m[2] += fx * fx;
                r.m[3] += fy * fy;
                r.m[4] += fx * fy;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                            continue;
                        }
                        let (nx, ny) = (nx as usize, ny as usize);
                        if !seen[ny * w + nx] && p(nx, ny) > 0.3 {
                            seen[ny * w + nx] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            out.push(r);
        }
    }
    out
}

/// `img` rotated by `rad` about its centre (bilinear), same size, edges
/// repeated.
fn rotate(img: &RgbImage, rad: f32) -> RgbImage {
    let (w, h) = img.dimensions();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (s, c) = rad.sin_cos();
    RgbImage::from_fn(w, h, |x, y| {
        // The source of each output pixel: rotate back.
        let (dx, dy) = (x as f32 - cx, y as f32 - cy);
        let sx = (cx + dx * c + dy * s).clamp(0.0, w as f32 - 1.001);
        let sy = (cy - dx * s + dy * c).clamp(0.0, h as f32 - 1.001);
        let (x0, y0) = (sx.floor() as u32, sy.floor() as u32);
        let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
        let px = |x, y| img.get_pixel(x, y).0;
        let (a, b, cc, d) = (
            px(x0, y0),
            px(x0 + 1, y0),
            px(x0, y0 + 1),
            px(x0 + 1, y0 + 1),
        );
        let mut o = [0u8; 3];
        for i in 0..3 {
            let top = a[i] as f32 * (1.0 - fx) + b[i] as f32 * fx;
            let bot = cc[i] as f32 * (1.0 - fx) + d[i] as f32 * fx;
            o[i] = (top * (1.0 - fy) + bot * fy).round() as u8;
        }
        Rgb(o)
    })
}

/// A line's axis-aligned bounds (x0, y0, x1, y1), in picture fractions.
fn bounds(l: &Line) -> (f32, f32, f32, f32) {
    let xs = [l.quad[0], l.quad[2], l.quad[4], l.quad[6]];
    let ys = [l.quad[1], l.quad[3], l.quad[5], l.quad[7]];
    let min = |v: [f32; 4]| v.into_iter().fold(f32::MAX, f32::min);
    let max = |v: [f32; 4]| v.into_iter().fold(f32::MIN, f32::max);
    (min(xs), min(ys), max(xs), max(ys))
}

fn find(p: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while p[r] != r {
        r = p[r];
    }
    let mut i = i;
    while p[i] != r {
        let n = p[i];
        p[i] = r;
        i = n;
    }
    r
}

/// Group lines into blocks (lines of a paragraph: overlapping columns, at
/// most 1.2 line heights apart) and blocks into documents (blocks whose
/// surroundings, three line heights wide, touch); order the lines block by
/// block, top to bottom, left to right. Sets each line's block and returns
/// each block's document.
pub fn layout(lines: &mut Vec<Line>) -> Vec<u16> {
    let n = lines.len();
    if n == 0 {
        return Vec::new();
    }
    let b: Vec<_> = lines.iter().map(bounds).collect();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in i + 1..n {
            let (a, c) = (b[i], b[j]);
            let lh = (a.3 - a.1).min(c.3 - c.1);
            let gap = (c.1 - a.3).max(a.1 - c.3);
            let overlap = a.2.min(c.2) - a.0.max(c.0);
            let narrower = (a.2 - a.0).min(c.2 - c.0);
            if gap <= 1.2 * lh && overlap > 0.3 * narrower {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    // Blocks, in order of their top edge, then left edge.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut root_of = std::collections::HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        let g = *root_of.entry(r).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }
    let bb = |g: &Vec<usize>| {
        g.iter()
            .fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN, 0f32), |acc, &i| {
                (
                    acc.0.min(b[i].0),
                    acc.1.min(b[i].1),
                    acc.2.max(b[i].2),
                    acc.3.max(b[i].3),
                    acc.4.max(b[i].3 - b[i].1),
                )
            })
    };
    groups.sort_by(|x, y| {
        let (a, c) = (bb(x), bb(y));
        (a.1, a.0)
            .partial_cmp(&(c.1, c.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Documents: blocks near each other.
    let k = groups.len();
    let boxes: Vec<_> = groups.iter().map(bb).collect();
    let mut dp: Vec<usize> = (0..k).collect();
    for i in 0..k {
        for j in i + 1..k {
            let (a, c) = (boxes[i], boxes[j]);
            let m = 3.0 * a.4.max(c.4);
            if a.0 - m <= c.2 && c.0 - m <= a.2 && a.1 - m <= c.3 && c.1 - m <= a.3 {
                let (ri, rj) = (find(&mut dp, i), find(&mut dp, j));
                dp[ri] = rj;
            }
        }
    }
    let mut doc_ids = std::collections::HashMap::new();
    let mut docs = Vec::with_capacity(k);
    for i in 0..k {
        let r = find(&mut dp, i);
        let next = doc_ids.len() as u16;
        docs.push(*doc_ids.entry(r).or_insert(next));
    }
    let mut ordered = Vec::with_capacity(n);
    for (bi, g) in groups.iter().enumerate() {
        let mut g = g.clone();
        g.sort_by(|&x, &y| {
            (b[x].1, b[x].0)
                .partial_cmp(&(b[y].1, b[y].0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for i in g {
            let mut l = lines[i].clone();
            l.block = bi as u16;
            ordered.push(l);
        }
    }
    *lines = ordered;
    docs
}

/// Decode a picture for reading: oriented, at most WORK_SIDE on its
/// longer side.
pub fn load_picture(path: &Path, max_bytes: u64) -> Result<RgbImage> {
    let crate::media::Class::Media(format) = crate::media::classify(path) else {
        bail!("not a picture");
    };
    if format.is_video() {
        bail!("not a picture");
    }
    let mut data = Vec::new();
    File::open(path)?.take(max_bytes).read_to_end(&mut data)?;
    let img =
        crate::media::decode(&data, format, 100_000_000).map_err(|_| anyhow::anyhow!("decode"))?;
    let img = if img.width().max(img.height()) > WORK_SIDE {
        img.resize(WORK_SIDE, WORK_SIDE, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok(img.to_rgb8())
}

/// Run `exe read-text` on one picture, killed after `timeout`.
pub fn run_child(exe: &Path, file: &Path, model_dir: &Path, timeout: Duration) -> Result<Text> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .arg("read-text")
        .arg(model_dir)
        .arg(file)
        .env_clear()
        .env("RAYON_NUM_THREADS", "4")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start read-text")?;
    let mut stdout = child.stdout.take().context("stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        (&mut stdout)
            .take(16 << 20)
            .read_to_end(&mut out)
            .map(|_| out)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().map_err(|_| anyhow::anyhow!("reader"))??;
    match status {
        None => bail!("timeout"),
        Some(s) if !s.success() => bail!("exit {}", s.code().unwrap_or(-1)),
        Some(_) => Ok(serde_json::from_slice(&out).context("child output")?),
    }
}

/// The `read-text` child: `lichtbild read-text <model-dir> <file>`.
pub fn child_main(args: &[&str]) -> Result<()> {
    let [model_dir, file] = args else {
        bail!("usage: read-text <model-dir> <file>");
    };
    limit_self(TIME_BUDGET.as_secs() * 4);
    let img = load_picture(Path::new(file), 256 << 20)?;
    let engine = Engine::load(Path::new(model_dir))?;
    let t = engine.read(&img)?;
    serde_json::to_writer(std::io::stdout().lock(), &t)?;
    Ok(())
}

fn limit_self(cpu_seconds: u64) {
    let set = |res, v: u64| {
        let lim = libc::rlimit {
            rlim_cur: v as libc::rlim_t,
            rlim_max: v as libc::rlim_t,
        };
        // SAFETY: setrlimit(2) on this process with a valid struct.
        unsafe { libc::setrlimit(res, &lim) };
    };
    set(libc::RLIMIT_CPU, cpu_seconds);
    // About 0.9 GB measured on the Pi 5 at 2400 px; room for 100 MP decodes.
    #[cfg(target_os = "linux")]
    set(libc::RLIMIT_AS, 3 << 30);
}

/// What one reading pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct PassReport {
    pub with_text: u64,
    pub without: u64,
    pub failed: u64,
    /// Left for the next pass because the deadline came.
    pub remaining: u64,
}

impl PassReport {
    pub fn to_text(&self) -> String {
        format!(
            "text in pictures: {} with text, {} without, {} failed, {} left for the next pass",
            self.with_text, self.without, self.failed, self.remaining
        )
    }
}

/// Read the photos that have no result from this model yet, newest first,
/// one child at a time, until `deadline`. Every result, failures included,
/// is stored under the photo's content hash, so the pass resumes.
pub fn run_pass(
    cfg: &crate::config::Config,
    idx: &crate::index::Index,
    exe: &Path,
    deadline: Option<Instant>,
    progress: &dyn Fn(u64, u64, &PassReport),
) -> Result<PassReport> {
    let mut rep = PassReport::default();
    let Some(dir) = &cfg.ocr_dir else {
        return Ok(rep);
    };
    let todo = idx.photos_to_read(MODEL)?;
    let total = todo.len() as u64;
    if total > 0 {
        tracing::info!("text in pictures: {total} photos to read");
    }
    let started = Instant::now();
    let mut said = Instant::now();
    for (n, (_, rec)) in todo.into_iter().enumerate() {
        if n > 0 && said.elapsed() >= Duration::from_secs(600) {
            said = Instant::now();
            tracing::info!(
                "text in pictures: {n} of {total} read ({} with text, {} failed) in {} min",
                rep.with_text,
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
        let t = run_child(exe, &path, dir, TIME_BUDGET).unwrap_or_else(|e| {
            let s = e.to_string();
            let s: String = s
                .split(':')
                .next()
                .unwrap_or("error")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
                .take(40)
                .collect();
            Text::failed(s.trim())
        });
        match t.state.as_str() {
            "ok" => rep.with_text += 1,
            "none" => rep.without += 1,
            _ => rep.failed += 1,
        }
        idx.set_text(&rec.hash, &t)?;
        progress(n as u64 + 1, total, &rep);
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x0: f32, y0: f32, x1: f32, y1: f32, t: &str) -> Line {
        Line {
            quad: [x0, y0, x1, y0, x1, y1, x0, y1],
            block: 0,
            text: t.into(),
            conf: 1.0,
        }
    }

    #[test]
    fn lines_group_into_blocks_and_documents() {
        // Two letters side by side, far apart; the left one has a heading
        // and a paragraph of two lines.
        let mut lines = vec![
            line(0.05, 0.30, 0.40, 0.33, "zweiter absatz"),
            line(0.60, 0.10, 0.95, 0.13, "anderer brief"),
            line(0.05, 0.10, 0.30, 0.14, "Titel"),
            line(0.05, 0.26, 0.42, 0.29, "erster absatz"),
            line(0.60, 0.14, 0.90, 0.17, "zweite zeile"),
        ];
        let docs = layout(&mut lines);
        let texts: Vec<(&str, u16)> = lines.iter().map(|l| (l.text.as_str(), l.block)).collect();
        assert_eq!(
            texts,
            vec![
                ("Titel", 0),
                ("anderer brief", 1),
                ("zweite zeile", 1),
                ("erster absatz", 2),
                ("zweiter absatz", 2),
            ]
        );
        // Title and paragraph are one letter; the right one another.
        assert_eq!(docs, vec![0, 1, 0]);
        let t = Text {
            state: "ok".into(),
            model: MODEL.into(),
            lines,
            docs,
        };
        assert_eq!(t.documents(), 2);
        assert!(t
            .plain()
            .starts_with("Titel\n\nanderer brief\nzweite zeile\n\n"));
    }

    #[test]
    fn texture_junk_is_dropped_and_real_lines_kept() {
        // Certainties as measured on the demo set (DD-023).
        assert!(!keep("la", 0.44));
        assert!(!keep("veeey", 0.48));
        assert!(!keep("sem", 0.70));
        assert!(!keep("ab", 0.85), "short lines need more certainty");
        assert!(!keep("€", 0.99), "fewer than two letters or digits");
        assert!(keep("ab", 0.95));
        assert!(keep("Apfelschorle 0,4 l 3,10 €", 0.966));
        assert!(keep("Sahne", 0.82));
    }

    #[test]
    fn rotation_keeps_the_centre_and_turns_the_corners() {
        let img = RgbImage::from_fn(41, 41, |x, y| {
            if x > 30 && y < 10 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 0])
            }
        });
        let r = rotate(&img, std::f32::consts::FRAC_PI_2);
        assert_eq!(r.get_pixel(20, 20), img.get_pixel(20, 20));
        // Top right goes to bottom right for a quarter turn clockwise in
        // image coordinates (y down).
        assert_eq!(r.get_pixel(35, 35).0[0], 255);
    }
}
