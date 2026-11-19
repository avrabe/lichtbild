//! OCR spike for lichtbild v0.7 (text in pictures): which engine reads
//! German text in photos well enough, and how fast on a Pi 5.
//!
//! Test pictures are synthetic: German sentences written for this spike,
//! drawn with a system font at run time (nothing is committed but this
//! code), so the right answer is known exactly. Engines:
//!   ocrs        ocrs 0.14 with its own models (Latin alphabet without
//!               umlauts or ß)
//!   paddle-v3   PaddleOCR PP-OCRv3 mobile detection + PP-OCRv5 Latin
//!               recognition (ONNX, run by rten)
//!   paddle-v5   the same with the PP-OCRv5 server detection model
//!
//! Usage: ocr-bench <models dir> <font.ttf>[,<font.ttf>...] [engine...]
//! Prints per engine: model load time, per-picture time, and the
//! character error rate (CER) over lines, plus how many umlauts and ß
//! came out right. No text is printed unless OCR_BENCH_TEXT=1.

use std::path::Path;
use std::time::Instant;

use ab_glyph::{FontVec, PxScale};
use anyhow::{bail, Context, Result};
use image::{GrayImage, Luma, Rgb, RgbImage};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};

/// Sentences written for this spike (no third-party text).
const LINES: &[&str] = &[
    "Rechnung Nr. 2024-0815 vom 12. März 2024",
    "Gesamtbetrag: 1.234,56 € inkl. 19 % MwSt.",
    "Bitte überweisen Sie den Betrag bis zum Fälligkeitsdatum.",
    "Sehr geehrte Frau Müller, vielen Dank für Ihre Bestellung.",
    "Öffnungszeiten: Montag bis Freitag 9:00 – 18:00 Uhr",
    "Straße der Einheit 42, 01234 Großdorf",
    "Achtung! Durchfahrt verboten für Fahrzeuge über 7,5 t",
    "Frühstück wird von 7 bis 10 Uhr im Erdgeschoss serviert.",
    "Kündigungsfrist: drei Monate zum Ende des Quartals",
    "Schlüssel bitte an der Rezeption abgeben.",
    "Zugverspätung wegen einer Störung am Stellwerk",
    "Herzlichen Glückwunsch zum Geburtstag, liebe Oma!",
    "Tagesgericht: Kartoffelsuppe mit Würstchen 6,90 €",
    "Ausgang · Notausgang · Treppenhaus",
    "Gültig bis 31.12.2025, nicht übertragbar",
    "Größe M, 100 % Baumwolle, bei 40 °C waschen",
];

struct Sample {
    name: String,
    image: RgbImage,
    lines: Vec<String>,
}

fn paper(w: u32, h: u32, seed: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let n = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503) ^ seed) % 23;
        let v = 228 + n as u8;
        Rgb([v, v - 3, v - 10])
    })
}

/// A page of `lines` in `font` at `size` px, rotated by `deg`.
fn page(name: &str, font: &FontVec, lines: &[&str], size: f32, deg: f32, seed: u32) -> Sample {
    page_spaced(name, font, lines, size, deg, seed, 1.9)
}

/// `page` with lines `spacing` × the text size apart.
fn page_spaced(
    name: &str,
    font: &FontVec,
    lines: &[&str],
    size: f32,
    deg: f32,
    seed: u32,
    spacing: f32,
) -> Sample {
    let (w, h) = (1600, 1200);
    let mut img = paper(w, h, seed);
    let ink = Rgb([30u8, 28, 26]);
    let mut y = 80.0;
    for l in lines {
        imageproc::drawing::draw_text_mut(&mut img, ink, 90, y as i32, PxScale::from(size), font, l);
        y += size * spacing;
    }
    if deg != 0.0 {
        img = imageproc::geometric_transformations::rotate_about_center(
            &img,
            deg.to_radians(),
            imageproc::geometric_transformations::Interpolation::Bilinear,
            imageproc::geometric_transformations::Border::Constant(Rgb([200, 196, 188])),
        );
    }
    Sample {
        name: name.into(),
        image: img,
        lines: lines.iter().map(|s| s.to_string()).collect(),
    }
}

/// A "photo": a dark scene with a small bright sign holding two lines.
fn sign(font: &FontVec, lines: &[&str], seed: u32) -> Sample {
    let (w, h) = (2000, 1500);
    let mut img = RgbImage::from_fn(w, h, |x, y| {
        let n = (x.wrapping_mul(2_246_822_519) ^ y.wrapping_mul(3_266_489_917) ^ seed) % 40;
        Rgb([40 + n as u8, 70 + (x / 40 % 30) as u8, 50 + (y / 30 % 40) as u8])
    });
    let (sx, sy, sw, sh) = (700, 600, 760, 170);
    for y in sy..sy + sh {
        for x in sx..sx + sw {
            img.put_pixel(x, y, Rgb([235, 232, 220]));
        }
    }
    for (i, l) in lines.iter().enumerate() {
        imageproc::drawing::draw_text_mut(
            &mut img,
            Rgb([20, 30, 90]),
            sx as i32 + 20,
            sy as i32 + 20 + i as i32 * 70,
            PxScale::from(44.0),
            font,
            l,
        );
    }
    Sample {
        name: format!("sign-{seed}"),
        image: img,
        lines: lines.iter().map(|s| s.to_string()).collect(),
    }
}

fn samples(fonts: &[(String, FontVec)]) -> Vec<Sample> {
    let mut out = Vec::new();
    for (fi, (fname, font)) in fonts.iter().enumerate() {
        let a = (fi * 5) % LINES.len();
        let pick: Vec<&str> = (0..8).map(|i| LINES[(a + i) % LINES.len()]).collect();
        out.push(page(&format!("{fname}-flat-36"), font, &pick, 36.0, 0.0, fi as u32));
        out.push(page(&format!("{fname}-flat-24"), font, &pick, 24.0, 0.0, fi as u32 + 7));
        out.push(page(&format!("{fname}-tilt-3"), font, &pick, 32.0, 3.0, fi as u32 + 13));
        out.push(sign(font, &pick[..2], fi as u32 + 21));
        out.push(page_spaced(&format!("{fname}-tight-28"), font, &pick, 28.0, 0.0, fi as u32 + 31, 1.25));
        out.push(page_spaced(&format!("{fname}-tight-tilt"), font, &pick, 28.0, -2.0, fi as u32 + 37, 1.3));
    }
    out
}

fn edit(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Errors over the expected lines: each matched with the recognized line
/// closest to it; (edits, characters, umlauts right, umlauts).
fn score(expected: &[String], got: &[String]) -> (usize, usize, usize, usize) {
    let norm = |s: &str| -> Vec<char> { s.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect() };
    let got: Vec<Vec<char>> = got.iter().map(|g| norm(g)).collect();
    let (mut e, mut n, mut ur, mut un) = (0, 0, 0, 0);
    for x in expected {
        let x = norm(x);
        let best = got.iter().min_by_key(|g| edit(&x, g));
        e += best.map_or(x.len(), |g| edit(&x, g));
        n += x.len();
        let um = |c: &char| "äöüÄÖÜß".contains(*c);
        un += x.iter().filter(|c| um(c)).count();
        if let Some(g) = best {
            for c in x.iter().filter(|c| um(c)) {
                ur += usize::from(g.contains(c));
            }
        }
    }
    (e, n, ur, un)
}

trait Engine {
    fn read(&self, img: &RgbImage) -> Result<Vec<String>>;
}

struct Ocrs(ocrs::OcrEngine);

impl Engine for Ocrs {
    fn read(&self, img: &RgbImage) -> Result<Vec<String>> {
        let src = ocrs::ImageSource::from_bytes(img.as_raw(), img.dimensions())?;
        let input = self.0.prepare_input(src)?;
        let words = self.0.detect_words(&input)?;
        let lines = self.0.find_text_lines(&input, &words);
        let text = self.0.recognize_text(&input, &lines)?;
        Ok(text.into_iter().flatten().map(|l| l.to_string()).collect())
    }
}

struct Paddle {
    det: rten::Model,
    rec: rten::Model,
    dict: Vec<char>,
    rec_h: usize,
}

impl Paddle {
    fn load(det: &Path, rec: &Path, dict: &Path) -> Result<Paddle> {
        let det = rten::Model::load_file(det).context("det")?;
        let rec = rten::Model::load_file(rec).context("rec")?;
        let mut dict: Vec<char> = std::fs::read_to_string(dict)?
            .lines()
            .filter_map(|l| l.chars().next())
            .collect();
        dict.push(' '); // use_space_char
        let rec_h = rec
            .input_shape(0)
            .and_then(|s| match s.get(2) {
                Some(rten::Dimension::Fixed(h)) => Some(*h),
                _ => None,
            })
            .or_else(|| std::env::var("OCR_BENCH_REC_H").ok()?.parse().ok())
            .unwrap_or(48);
        eprintln!("rec input shape {:?}, height {rec_h}", rec.input_shape(0));
        Ok(Paddle { det, rec, dict, rec_h })
    }

    /// DB detection: boxes (x0, y0, x1, y1) in image pixels, one per line,
    /// and the text's skew in degrees (the median orientation of the wide
    /// regions, from their second moments).
    fn detect(&self, img: &RgbImage) -> Result<(Vec<(u32, u32, u32, u32)>, f32)> {
        let (w, h) = img.dimensions();
        let scale = (960.0 / w.max(h) as f32).min(1.0);
        let rw = ((w as f32 * scale / 32.0).round().max(1.0) * 32.0) as u32;
        let rh = ((h as f32 * scale / 32.0).round().max(1.0) * 32.0) as u32;
        let small = image::imageops::resize(img, rw, rh, image::imageops::FilterType::Triangle);
        let (mean, std) = ([0.485f32, 0.456, 0.406], [0.229f32, 0.224, 0.225]);
        let mut t = NdTensor::<f32, 4>::zeros([1, 3, rh as usize, rw as usize]);
        for (x, y, p) in small.enumerate_pixels() {
            // PaddleOCR feeds BGR.
            let bgr = [p[2], p[1], p[0]];
            for c in 0..3 {
                t[[0, c, y as usize, x as usize]] = (bgr[c] as f32 / 255.0 - mean[c]) / std[c];
            }
        }
        let out: Tensor<f32> = self.det.run_one(t.view().into(), None)?.try_into()?;
        let prob = out.into_shape([rh as usize, rw as usize]);
        let mask = GrayImage::from_fn(rw, rh, |x, y| {
            Luma([if prob[[y as usize, x as usize]] > 0.3 { 255 } else { 0 }])
        });
        let labels = imageproc::region_labelling::connected_components(
            &mask,
            imageproc::region_labelling::Connectivity::Eight,
            Luma([0]),
        );
        let mut boxes: std::collections::HashMap<u32, (u32, u32, u32, u32, f32, u32)> =
            std::collections::HashMap::new();
        // Per region: sums of x, y, xx, yy, xy for the orientation.
        let mut mom: std::collections::HashMap<u32, [f64; 5]> = std::collections::HashMap::new();
        for (x, y, l) in labels.enumerate_pixels() {
            if l[0] == 0 {
                continue;
            }
            let m = mom.entry(l[0]).or_insert([0.0; 5]);
            let (fx, fy) = (x as f64, y as f64);
            m[0] += fx;
            m[1] += fy;
            m[2] += fx * fx;
            m[3] += fy * fy;
            m[4] += fx * fy;
            let p = prob[[y as usize, x as usize]];
            let b = boxes.entry(l[0]).or_insert((x, y, x, y, 0.0, 0));
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
            b.4 += p;
            b.5 += 1;
        }
        let mut angles = Vec::new();
        for (l, &(x0, _, x1, _, _, n)) in &boxes {
            let m = mom[l];
            let n = n as f64;
            if n < 50.0 || x1 - x0 < 40 {
                continue;
            }
            let (cx, cy) = (m[0] / n, m[1] / n);
            let (mu20, mu02, mu11) = (m[2] / n - cx * cx, m[3] / n - cy * cy, m[4] / n - cx * cy);
            angles.push((0.5 * (2.0 * mu11).atan2(mu20 - mu02)).to_degrees() as f32);
        }
        angles.sort_by(f32::total_cmp);
        let skew = angles.get(angles.len() / 2).copied().unwrap_or(0.0);
        let mut out = Vec::new();
        for (x0, y0, x1, y1, sum, n) in boxes.into_values() {
            if n < 10 || sum / n as f32 <= 0.6 {
                continue;
            }
            // Unclip by area * 1.5 / perimeter, then back to image pixels.
            let (bw, bh) = ((x1 - x0 + 1) as f32, (y1 - y0 + 1) as f32);
            let ratio: f32 = std::env::var("OCR_BENCH_UNCLIP").ok().and_then(|v| v.parse().ok()).unwrap_or(2.5);
            let d = bw * bh * ratio / (2.0 * (bw + bh));
            let f = |v: f32, max: u32| (v / scale).clamp(0.0, max as f32 - 1.0) as u32;
            out.push((
                f(x0 as f32 - d, w),
                f(y0 as f32 - d, h),
                f(x1 as f32 + d, w),
                f(y1 as f32 + d, h),
            ));
        }
        out.sort_by_key(|b| (b.1 / 20, b.0));
        Ok((out, skew))
    }

    fn recognize(&self, img: &RgbImage, b: (u32, u32, u32, u32)) -> Result<String> {
        let crop = image::imageops::crop_imm(img, b.0, b.1, b.2 - b.0 + 1, b.3 - b.1 + 1).to_image();
        let h = self.rec_h;
        let w = ((crop.width() as f32 * h as f32 / crop.height() as f32).ceil() as usize).clamp(16, 3200);
        let r = image::imageops::resize(&crop, w as u32, h as u32, image::imageops::FilterType::Triangle);
        let mut t = NdTensor::<f32, 4>::zeros([1, 3, h, w]);
        let rgb = std::env::var("OCR_BENCH_REC_RGB").is_ok();
        for (x, y, p) in r.enumerate_pixels() {
            let bgr = if rgb { [p[0], p[1], p[2]] } else { [p[2], p[1], p[0]] };
            for c in 0..3 {
                t[[0, c, y as usize, x as usize]] = (bgr[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
        let out: Tensor<f32> = self.rec.run_one(t.view().into(), None)?.try_into()?;
        let shape = out.shape().to_vec();
        if shape.len() != 3 {
            bail!("rec output shape {shape:?}");
        }
        let (steps, classes) = (shape[1], shape[2]);
        let out = out.into_shape([steps, classes]);
        let mut s = String::new();
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
                }
            }
            last = best;
        }
        Ok(s)
    }
}

impl Engine for Paddle {
    fn read(&self, img: &RgbImage) -> Result<Vec<String>> {
        let (boxes, skew) = self.detect(img)?;
        // A tilted page: straighten it and detect again, so line boxes do
        // not overlap. (A real page photo is tilted as a whole.)
        if skew.abs() > 0.7 && std::env::var("OCR_BENCH_NO_DESKEW").is_err() {
            let straight = imageproc::geometric_transformations::rotate_about_center(
                img,
                -skew.to_radians(),
                imageproc::geometric_transformations::Interpolation::Bilinear,
                imageproc::geometric_transformations::Border::Replicate,
            );
            let (boxes, _) = self.detect(&straight)?;
            return boxes.into_iter().map(|b| self.recognize(&straight, b)).collect();
        }
        boxes.into_iter().map(|b| self.recognize(img, b)).collect()
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        bail!("usage: ocr-bench <models dir> <font.ttf>[,<font.ttf>...] [ocrs|paddle-v3|paddle-v5 ...]");
    }
    let m = Path::new(&args[1]);
    let fonts: Vec<(String, FontVec)> = args[2]
        .split(',')
        .map(|p| {
            let name = Path::new(p).file_stem().unwrap().to_string_lossy().replace(' ', "_");
            (name, FontVec::try_from_vec(std::fs::read(p).unwrap()).unwrap())
        })
        .collect();
    let engines: Vec<String> = if args.len() > 3 {
        args[3..].to_vec()
    } else {
        vec!["ocrs".into(), "paddle-v3".into(), "paddle-v5".into()]
    };
    let set = samples(&fonts);
    let show = std::env::var("OCR_BENCH_TEXT").is_ok();
    println!(
        "{} pictures, {} lines, threads {}",
        set.len(),
        set.iter().map(|s| s.lines.len()).sum::<usize>(),
        std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "default".into())
    );
    for name in engines {
        let t0 = Instant::now();
        let engine: Box<dyn Engine> = match name.as_str() {
            "ocrs" => Box::new(Ocrs(ocrs::OcrEngine::new(ocrs::OcrEngineParams {
                detection_model: Some(rten::Model::load_file(m.join("ocrs/text-detection.rten"))?),
                recognition_model: Some(rten::Model::load_file(m.join("ocrs/text-recognition.rten"))?),
                ..Default::default()
            })?)),
            "paddle-v3" | "paddle-v5" => Box::new(Paddle::load(
                &m.join(if name == "paddle-v3" {
                    "paddle/detection_v3_det.onnx"
                } else {
                    "paddle/detection_v5_det.onnx"
                }),
                &m.join("paddle/languages_latin_rec.onnx"),
                &m.join("paddle/languages_latin_dict.txt"),
            )?),
            other => bail!("unknown engine {other}"),
        };
        let load = t0.elapsed();
        let (mut e, mut n, mut ur, mut un) = (0, 0, 0, 0);
        let mut times = Vec::new();
        for s in &set {
            let t = Instant::now();
            let got = engine.read(&s.image)?;
            times.push(t.elapsed().as_secs_f64());
            let (se, sn, sur, sun) = score(&s.lines, &got);
            if show {
                println!("--- {name} {}: {:.1}% CER", s.name, 100.0 * se as f64 / sn as f64);
                for g in &got {
                    println!("    {g}");
                }
            } else {
                println!(
                    "  {name:<9} {:<28} {:>6.2} s  CER {:>5.1}%  lines {:>2}/{}",
                    s.name,
                    times.last().unwrap(),
                    100.0 * se as f64 / sn as f64,
                    got.len(),
                    s.lines.len()
                );
            }
            (e, n, ur, un) = (e + se, n + sn, ur + sur, un + sun);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{name}: load {:.2} s, per picture median {:.2} s (max {:.2} s), CER {:.1}%, umlauts/ß right {}/{}",
            load.as_secs_f64(),
            times[times.len() / 2],
            times.last().unwrap(),
            100.0 * e as f64 / n as f64,
            ur,
            un
        );
    }
    Ok(())
}
