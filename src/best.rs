//! The best shot of a series (REQ-WEB-021): the sharpest, well-exposed
//! picture leads it, unless the viewer made one a favourite.
//!
//! Sharpness is the variance of the Laplacian over the thumbnail's grey
//! values (a blurred or shaken shot has little edge energy); exposure takes
//! points off for a mean far from the middle and for clipped pixels. The
//! score compares look-alike shots of one moment, not unrelated photos.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use crate::index::Item;

/// How good one thumbnail looks; higher is better. None when it cannot be
/// read.
pub fn score(jpeg: &[u8]) -> Option<f32> {
    let img = image::load_from_memory(jpeg).ok()?.to_luma8();
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return None;
    }
    let px = |x: u32, y: u32| f64::from(img.get_pixel(x, y)[0]);
    let (mut sum, mut sq, mut n) = (0.0, 0.0, 0.0);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let l = px(x - 1, y) + px(x + 1, y) + px(x, y - 1) + px(x, y + 1) - 4.0 * px(x, y);
            sum += l;
            sq += l * l;
            n += 1.0;
        }
    }
    let var = sq / n - (sum / n).powi(2);
    let all = f64::from(w * h);
    let mean = img.pixels().map(|p| f64::from(p[0])).sum::<f64>() / all;
    let clipped = img.pixels().filter(|p| p[0] <= 4 || p[0] >= 251).count() as f64 / all;
    let s = (1.0 + var).ln() - 2.0 * ((mean - 127.5).abs() / 127.5) - 3.0 * clipped;
    Some(s as f32)
}

/// Scores by content hash, read once from the thumbnails.
#[derive(Default)]
pub struct Best {
    memo: Mutex<HashMap<String, Option<f32>>>,
}

impl Best {
    /// The score of an item's thumbnail; an item without one scores lowest.
    pub fn of(&self, data_dir: &Path, it: &Item) -> f32 {
        if let Some(s) = self
            .memo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&it.hash)
        {
            return s.unwrap_or(f32::MIN);
        }
        let s = it
            .thumb_ok
            .then(|| std::fs::read(crate::scan::thumb_path(data_dir, &it.hash)).ok())
            .flatten()
            .and_then(|b| score(&b));
        self.memo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(it.hash.clone(), s);
        s.unwrap_or(f32::MIN)
    }
}

/// Put each series' best item first, the rest after it in their order.
/// `runs` is what `web::series` returned for `items`; its counts stay
/// right, since only places inside a series change. A favourite wins.
pub fn best_first(items: &mut [Item], runs: &[usize], score: impl Fn(&Item) -> f32) {
    let mut i = 0;
    while i < items.len() {
        let n = runs.get(i).copied().unwrap_or(1).max(1);
        if n >= 2 && i + n <= items.len() {
            let key = |it: &Item| (it.fav, score(it));
            let mut best = i;
            for j in i + 1..i + n {
                let (a, b) = (key(&items[j]), key(&items[best]));
                if (a.0 && !b.0) || (a.0 == b.0 && a.1 > b.1) {
                    best = j;
                }
            }
            items[i..=best].rotate_right(1);
        }
        i += n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(f: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        let img = image::GrayImage::from_fn(64, 64, |x, y| image::Luma([f(x, y)]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
            .encode_image(&img)
            .unwrap();
        out
    }

    #[test]
    fn sharp_beats_blurred_and_dark() {
        let sharp = score(&jpeg(
            |x, y| if (x / 4 + y / 4) % 2 == 0 { 60 } else { 190 },
        ))
        .unwrap();
        let soft = score(&jpeg(|x, _| 60 + (x * 2) as u8)).unwrap();
        let dark = score(&jpeg(|x, y| if (x / 4 + y / 4) % 2 == 0 { 0 } else { 12 })).unwrap();
        assert!(sharp > soft, "{sharp} {soft}");
        assert!(sharp > dark, "{sharp} {dark}");
        assert!(score(b"not a picture").is_none());
    }

    fn item(id: u64, fav: bool) -> Item {
        Item {
            id,
            fav,
            ..Item::default()
        }
    }

    #[test]
    fn the_best_leads_its_series_and_a_favourite_wins() {
        let mut items: Vec<Item> = (1..=6).map(|i| item(i, i == 6)).collect();
        // 1 alone; 2,3,4 a series with 4 best; 5,6 a series with 6 a favourite.
        let runs = [1, 3, 0, 0, 2, 0];
        let score = |it: &Item| [0.0, 9.0, 1.0, 2.0, 3.0, 5.0, 0.0][it.id as usize];
        best_first(&mut items, &runs, score);
        let ids: Vec<u64> = items.iter().map(|it| it.id).collect();
        assert_eq!(ids, vec![1, 4, 2, 3, 6, 5]);
    }
}
