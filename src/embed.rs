//! What a picture shows, as a vector (REQ-SEE-002, DD-024): SigLIP
//! base-patch16-256 multilingual puts pictures and sentences (German,
//! English and 100 more) in one space, so "zwei Katzen auf dem Sofa" finds
//! the picture, and a picture finds the ones that look like it.
//!
//! The image half runs in the `look` child; the text half in the server,
//! loaded on the first query. Vectors are stored as 768 signed bytes
//! (unit length × 127): 24 MB in memory for 31,000 pictures.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use image::RgbImage;
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};

pub const DIM: usize = 768;
const SIDE: u32 = 256;
/// SigLIP was trained with every text padded to 64 tokens.
const TOKENS: usize = 64;
const PAD: u32 = 1;

/// A query and a picture match from this cosine on, and only from this
/// share of the best match's (DD-024: matches 0.049–0.144; single words on
/// noise pictures up to 0.032, sentences on unrelated photos at most 0.022).
pub const TEXT_MIN: f32 = 0.05;
pub const TEXT_OF_BEST: f32 = 0.6;
/// The text encoder is dropped after this long without a query.
pub const IDLE: std::time::Duration = std::time::Duration::from_secs(600);

/// Pictures look alike from this cosine on.
pub const SIMILAR_MIN: f32 = 0.80;
/// Pictures taken one after another that look at least this alike are one
/// series (REQ-WEB-012): a burst, or the same scene taken again.
pub const SERIES_MIN: f32 = 0.92;

fn unit(v: &[f32]) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter().map(|x| x / n).collect()
}

/// A vector as signed bytes, scaled so its largest component is ±127:
/// SigLIP's 768 components are small (about 0.036 each when unit length),
/// so a fixed scale would leave each only a few steps.
pub fn quantize(v: &[f32]) -> Vec<i8> {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-9);
    v.iter()
        .map(|x| (x / max * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

/// The length of a byte vector.
fn norm_i8(a: &[i8]) -> f32 {
    a.iter()
        .map(|x| (*x as f32) * (*x as f32))
        .sum::<f32>()
        .sqrt()
        .max(1e-9)
}

/// Cosine of a byte vector (with its length) and a unit vector.
fn cos_i8_f32(a: &[i8], na: f32, b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| *x as f32 * y).sum::<f32>() / na
}

/// Cosine of two byte vectors with their lengths.
fn cos_i8(a: &[i8], na: f32, b: &[i8], nb: f32) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 * *y as i32) as f32)
        .sum::<f32>()
        / (na * nb)
}

/// The image encoder.
pub struct ImageEncoder {
    model: rten::Model,
}

impl ImageEncoder {
    pub fn new(model: rten::Model) -> ImageEncoder {
        ImageEncoder { model }
    }

    pub fn encode(&self, img: &RgbImage) -> Result<Vec<i8>> {
        let r = image::imageops::resize(img, SIDE, SIDE, image::imageops::FilterType::CatmullRom);
        let s = SIDE as usize;
        let mut t = NdTensor::<f32, 4>::zeros([1, 3, s, s]);
        for (x, y, p) in r.enumerate_pixels() {
            for c in 0..3 {
                t[[0, c, y as usize, x as usize]] = (p[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
        let input = self.model.node_id("pixel_values")?;
        let output = self.model.node_id("pooler_output")?;
        let out: Tensor<f32> = self
            .model
            .run(vec![(input, t.view().into())], &[output], None)?
            .into_iter()
            .next()
            .context("no output")?
            .try_into()?;
        let v = out.to_vec();
        anyhow::ensure!(v.len() == DIM, "image vector of {}", v.len());
        Ok(quantize(&v))
    }
}

/// The text encoder, for queries.
pub struct TextEncoder {
    model: rten::Model,
    tokenizer: tokenizers::Tokenizer,
}

impl TextEncoder {
    /// Load from the vision directory (files pinned by vision::check).
    pub fn load(dir: &Path) -> Result<TextEncoder> {
        let model = rten::Model::load(crate::vision::read_pinned(dir, "siglip-text.onnx")?)?;
        let tok = crate::vision::read_pinned(dir, "siglip-tokenizer.json")?;
        let tokenizer = tokenizers::Tokenizer::from_bytes(tok).map_err(anyhow::Error::msg)?;
        Ok(TextEncoder { model, tokenizer })
    }

    /// A unit vector for `text` (lower-cased, as SigLIP was trained).
    pub fn encode(&self, text: &str) -> Result<Vec<f32>> {
        let enc = self
            .tokenizer
            .encode(text.to_lowercase(), true)
            .map_err(anyhow::Error::msg)?;
        let mut ids: Vec<i32> = enc.get_ids().iter().map(|&i| i as i32).collect();
        ids.truncate(TOKENS);
        ids.resize(TOKENS, PAD as i32);
        let x = NdTensor::from_data([1, TOKENS], ids);
        let input = self.model.node_id("input_ids")?;
        let output = self.model.node_id("pooler_output")?;
        let out: Tensor<f32> = self
            .model
            .run(vec![(input, x.view().into())], &[output], None)?
            .into_iter()
            .next()
            .context("no output")?
            .try_into()?;
        Ok(unit(&out.to_vec()))
    }
}

/// One stored picture: its content hash, the items holding it (id and
/// library), and its vector.
struct Entry {
    hash: String,
    items: Vec<(u64, String)>,
    v: Vec<i8>,
    norm: f32,
}

/// The vectors, and where each content hash is among them.
#[derive(Default)]
struct Vectors {
    list: Vec<Entry>,
    at: HashMap<String, usize>,
}

/// The loaded text encoder and when it was last used.
type Slot = Arc<Mutex<Option<(Arc<TextEncoder>, std::time::Instant)>>>;

/// The stored picture vectors, for search and "looks like"; and the text
/// encoder, loaded on first use.
#[derive(Default)]
pub struct Space {
    vectors: RwLock<Vectors>,
    /// The text encoder and when it was last used; dropped after IDLE.
    text: Slot,
    reaper: std::sync::OnceLock<()>,
    dir: RwLock<Option<std::path::PathBuf>>,
}

impl Space {
    pub fn with_dir(dir: Option<&Path>) -> Space {
        Space {
            dir: RwLock::new(dir.map(Path::to_path_buf)),
            ..Space::default()
        }
    }

    /// Reload the vectors, and the live items holding each, from the
    /// index.
    pub fn rebuild(&self, idx: &crate::index::Index) -> Result<usize> {
        let mut items: HashMap<String, Vec<(u64, String)>> = HashMap::new();
        for (id, r) in idx.all()? {
            if !r.missing {
                items.entry(r.hash).or_default().push((id, r.label));
            }
        }
        let v: Vec<Entry> = idx
            .embeddings()?
            .into_iter()
            .filter_map(|(hash, v)| {
                let it = items.remove(&hash)?;
                let norm = norm_i8(&v);
                Some(Entry {
                    hash,
                    items: it,
                    v,
                    norm,
                })
            })
            .collect();
        let n = v.len();
        let at = v
            .iter()
            .enumerate()
            .map(|(i, e)| (e.hash.clone(), i))
            .collect();
        *self.vectors.write().expect("space lock") = Vectors { list: v, at };
        Ok(n)
    }

    pub fn len(&self) -> usize {
        self.vectors.read().expect("space lock").list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The text encoder, loaded on first use. It holds about 0.6 GB (the
    /// tokenizer of 250,000 pieces 0.3 GB, the int8 model 0.3 GB, measured
    /// on an M4), so it is dropped after IDLE without a query (DD-024).
    fn encoder(&self) -> Result<Option<Arc<TextEncoder>>> {
        let Some(dir) = self.dir.read().expect("space lock").clone() else {
            return Ok(None);
        };
        let mut t = self.text.lock().expect("space lock");
        let enc = match t.take() {
            Some((enc, _)) => enc,
            None => Arc::new(TextEncoder::load(&dir)?),
        };
        *t = Some((enc.clone(), std::time::Instant::now()));
        drop(t);
        let slot = self.text.clone();
        self.reaper.get_or_init(move || {
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let mut t = slot.lock().unwrap_or_else(|e| e.into_inner());
                if t.as_ref().is_some_and(|(_, used)| used.elapsed() >= IDLE) {
                    *t = None;
                    tracing::info!("search by meaning idle: text encoder unloaded");
                }
            });
        });
        Ok(Some(enc))
    }

    /// Whether the text encoder is loaded (for the status page and tests).
    pub fn encoder_loaded(&self) -> bool {
        self.text.lock().expect("space lock").is_some()
    }

    /// Pictures that show `query`, best first: the ids holding each in
    /// `labels`, and the cosine.
    pub fn search(
        &self,
        query: &str,
        labels: &[String],
        limit: usize,
    ) -> Result<Vec<(Vec<u64>, f32)>> {
        let Some(enc) = self.encoder()? else {
            return Ok(Vec::new());
        };
        let q = enc.encode(query)?;
        let vs = self.vectors.read().expect("space lock");
        let scored: Vec<(&Entry, f32)> = vs
            .list
            .iter()
            .map(|e| (e, cos_i8_f32(&e.v, e.norm, &q)))
            .collect();
        let top = scored.iter().map(|(_, c)| *c).fold(f32::MIN, f32::max);
        let min = TEXT_MIN.max(top * TEXT_OF_BEST);
        Ok(best(scored.into_iter(), min, labels, limit))
    }

    /// How alike the pictures with contents `a` and `b` look (cosine),
    /// when both have been looked at.
    pub fn alike(&self, a: &str, b: &str) -> Option<f32> {
        let vs = self.vectors.read().expect("space lock");
        let (x, y) = (&vs.list[*vs.at.get(a)?], &vs.list[*vs.at.get(b)?]);
        Some(cos_i8(&x.v, x.norm, &y.v, y.norm))
    }

    /// Pictures that look like the one with content `hash`, best first.
    pub fn similar(&self, hash: &str, labels: &[String], limit: usize) -> Vec<(Vec<u64>, f32)> {
        let vs = self.vectors.read().expect("space lock");
        let Some(me) = vs.at.get(hash).map(|i| &vs.list[*i]) else {
            return Vec::new();
        };
        best(
            vs.list
                .iter()
                .filter(|e| e.hash != hash)
                .map(|e| (e, cos_i8(&e.v, e.norm, &me.v, me.norm))),
            SIMILAR_MIN,
            labels,
            limit,
        )
    }
}

/// The `limit` best scored entries from `min` on, with their ids in
/// `labels` (entries with none left out).
fn best<'a>(
    scored: impl Iterator<Item = (&'a Entry, f32)>,
    min: f32,
    labels: &[String],
    limit: usize,
) -> Vec<(Vec<u64>, f32)> {
    let mut hits: Vec<(Vec<u64>, f32)> = scored
        .filter(|(_, c)| *c >= min)
        .filter_map(|(e, c)| {
            let ids: Vec<u64> = e
                .items
                .iter()
                .filter(|(_, l)| labels.contains(l))
                .map(|(id, _)| *id)
                .collect();
            (!ids.is_empty()).then_some((ids, c))
        })
        .collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_vectors_keep_the_cosine() {
        // Small components, as SigLIP's are, and cosines near a match's.
        let a: Vec<f32> = (0..DIM).map(|i| (i as f32 * 0.37).sin() * 0.03).collect();
        let b: Vec<f32> = (0..DIM)
            .map(|i| ((i as f32 * 0.37).sin() * 0.2 + (i as f32 * 1.3).cos()) * 0.03)
            .collect();
        let exact: f32 = unit(&a).iter().zip(unit(&b)).map(|(x, y)| x * y).sum();
        let (qa, qb) = (quantize(&a), quantize(&b));
        let (na, nb) = (norm_i8(&qa), norm_i8(&qb));
        assert!((cos_i8(&qa, na, &qb, nb) - exact).abs() < 0.005, "{exact}");
        assert!((cos_i8_f32(&qa, na, &unit(&b)) - exact).abs() < 0.005);
        assert!((cos_i8(&qa, na, &qa, na) - 1.0).abs() < 1e-4);
        assert!(qa.iter().any(|x| x.abs() == 127), "the full range is used");
    }
}
