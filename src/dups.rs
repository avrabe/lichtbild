//! Duplicates (REQ-DUP-001, DD-020): the same picture more than once, shown
//! once. Two layers:
//! - exact copies: the same content hash;
//! - the same picture in another file (a messenger copy without its date, a
//!   resized export, the JPEG twin of a HEIC): a 64-bit difference hash of
//!   the thumbnail within a few bits, plus evidence that one is a copy, so
//!   a burst of camera shots stays separate (that is "similar", later).
//!
//! Fingerprints come from the thumbnails in the data directory; originals
//! are not read again. Groups are rebuilt in memory after each scan. Which
//! copy is shown depends on what the viewer may see: the best copy among
//! the visible ones (REQ-AUTH-002).

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::Result;

use crate::index::{Index, Record};
use crate::media::DateSource;

/// Most differing bits for "the same picture" (of 64).
pub const NEAR_BITS: u32 = 6;

/// A difference hash: the thumbnail as 9x8 grey, one bit per neighbouring
/// pair (left brighter than right). `None` for an almost flat picture
/// (black, white, fog), whose hash says nothing.
pub fn dhash(img: &image::DynamicImage) -> Option<u64> {
    let g = img
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let px: Vec<f32> = g.pixels().map(|p| f32::from(p[0])).collect();
    let mean = px.iter().sum::<f32>() / px.len() as f32;
    let var = px.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / px.len() as f32;
    if var.sqrt() < 6.0 {
        return None;
    }
    let mut h = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            h <<= 1;
            if g.get_pixel(x, y)[0] > g.get_pixel(x + 1, y)[0] {
                h |= 1;
            }
        }
    }
    Some(h)
}

/// Compute missing fingerprints from the thumbnails. Returns how many were
/// added.
pub fn fingerprint_pass(data_dir: &std::path::Path, idx: &Index) -> Result<usize> {
    let mut added = 0;
    for hash in idx.missing_fingerprints()? {
        let path = crate::scan::thumb_path(data_dir, &hash);
        // A thumbnail that cannot be read gets no fingerprint this time.
        let Ok(img) = image::open(&path) else {
            continue;
        };
        idx.set_fingerprint(&hash, dhash(&img))?;
        added += 1;
    }
    Ok(added)
}

#[derive(Debug, Clone)]
struct Member {
    id: u64,
    label: String,
}

#[derive(Debug, Default)]
struct Groups {
    /// item id -> group index
    group_of: HashMap<u64, usize>,
    /// members, best copy first
    groups: Vec<Vec<Member>>,
}

/// What the timeline needs to know about one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    /// The item is the one shown for its group (or has no group).
    pub shown: bool,
    /// Copies the viewer may see, this one included.
    pub copies: usize,
}

/// A rebuild's counts, for the log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub groups: usize,
    pub items: usize,
    pub exact_pairs: usize,
    pub near_pairs: usize,
}

/// The duplicate groups the server holds; empty until the first rebuild.
#[derive(Default)]
pub struct Dups {
    inner: RwLock<Groups>,
}

impl std::fmt::Debug for Dups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dups")
    }
}

struct Candidate {
    id: u64,
    rec: Record,
    fp: Option<u64>,
}

fn is_video(r: &Record) -> bool {
    r.video.is_some()
}

fn ext(r: &Record) -> String {
    let rel = String::from_utf8_lossy(&r.rel).to_ascii_lowercase();
    match rel.rsplit_once('.') {
        Some((_, e)) if !e.contains('/') => e.to_string(),
        _ => String::new(),
    }
}

/// The evidence that two pictures with nearly the same fingerprint are one
/// picture, not two shots of a burst: one lost its camera date (a copy), or
/// they differ in size (an export), or they are the same second in two
/// formats (HEIC and its JPEG twin).
fn same_picture(a: &Record, b: &Record) -> bool {
    if is_video(a) != is_video(b) {
        return false;
    }
    let weak = |r: &Record| DateSource::is_weak(&r.date_source);
    let px = |r: &Record| u64::from(r.width) * u64::from(r.height);
    let (pa, pb) = (px(a), px(b));
    let size_differs = pa.max(pb) * 10 > pa.min(pb) * 11;
    let ratio = |r: &Record| f64::from(r.width) / f64::from(r.height.max(1));
    // Rotated or cropped differently: not the same picture.
    if (ratio(a) - ratio(b)).abs() > 0.03 * ratio(a).max(ratio(b)) {
        return false;
    }
    let twin = a.taken_local == b.taken_local && ext(a) != ext(b);
    weak(a) || weak(b) || size_differs || twin
}

/// Better copy first: a camera date, a position, more pixels, a larger
/// file, then the older file (copies are usually made later), then the
/// older id.
type Rank = (
    bool,
    bool,
    u64,
    u64,
    std::cmp::Reverse<i64>,
    std::cmp::Reverse<u64>,
);

fn rank(r: &Record, id: u64) -> Rank {
    (
        !DateSource::is_weak(&r.date_source),
        r.lat.is_some(),
        u64::from(r.width) * u64::from(r.height),
        r.size,
        std::cmp::Reverse(r.mtime_ns),
        std::cmp::Reverse(id),
    )
}

/// Seconds between two capture times (a day apart when either is
/// unreadable).
fn seconds_apart(a: &Record, b: &Record) -> i64 {
    let p = |r: &Record| {
        chrono::NaiveDateTime::parse_from_str(&r.taken_local, "%Y-%m-%dT%H:%M:%S").ok()
    };
    match (p(a), p(b)) {
        (Some(x), Some(y)) => (x - y).num_seconds().abs(),
        _ => 86_400,
    }
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

impl Dups {
    /// Rebuild the groups from the index and the stored fingerprints.
    pub fn rebuild(&self, idx: &Index) -> Result<Report> {
        let fps = idx.fingerprints()?;
        let cands: Vec<Candidate> = idx
            .all()?
            .into_iter()
            .filter(|(_, r)| r.shown())
            .map(|(id, rec)| Candidate {
                id,
                fp: fps.get(&rec.hash).copied().flatten(),
                rec,
            })
            .collect();
        let n = cands.len();
        let mut parent: Vec<usize> = (0..n).collect();
        let mut rep = Report::default();
        let union = |parent: &mut Vec<usize>, a: usize, b: usize| {
            let (ra, rb) = (find(parent, a), find(parent, b));
            if ra != rb {
                parent[ra] = rb;
            }
        };
        // Exact copies.
        let mut by_hash: HashMap<&str, usize> = HashMap::new();
        for (i, c) in cands.iter().enumerate() {
            if let Some(&j) = by_hash.get(c.rec.hash.as_str()) {
                union(&mut parent, i, j);
                rep.exact_pairs += 1;
            } else {
                by_hash.insert(&c.rec.hash, i);
            }
        }
        // The same picture: eight 8-bit bands; two hashes within 7 bits
        // agree on at least one band, so only band-mates are compared.
        let mut bands: HashMap<(u8, u8), Vec<usize>> = HashMap::new();
        for (i, c) in cands.iter().enumerate() {
            if let Some(fp) = c.fp {
                for b in 0..8u8 {
                    bands.entry((b, (fp >> (8 * b)) as u8)).or_default().push(i);
                }
            }
        }
        // Each copy (the lower-ranked of a pair) joins only its nearest
        // match: a messenger copy of the first shot of a burst nearly
        // matches the second shot too, and linking both would pull the
        // burst together through the copy.
        let mut nearest: HashMap<usize, (u32, i64, usize)> = HashMap::new();
        for members in bands.values() {
            for (k, &i) in members.iter().enumerate() {
                for &j in &members[k + 1..] {
                    let (a, b) = (&cands[i], &cands[j]);
                    if a.rec.hash == b.rec.hash {
                        continue;
                    }
                    let (Some(fa), Some(fb)) = (a.fp, b.fp) else {
                        continue;
                    };
                    let dist = (fa ^ fb).count_ones();
                    if dist > NEAR_BITS || !same_picture(&a.rec, &b.rec) {
                        continue;
                    }
                    let (copy, orig) = if rank(&a.rec, a.id) < rank(&b.rec, b.id) {
                        (i, j)
                    } else {
                        (j, i)
                    };
                    let gap = seconds_apart(&cands[copy].rec, &cands[orig].rec);
                    let better = nearest
                        .get(&copy)
                        .is_none_or(|&(d, g, o)| (dist, gap, cands[orig].id) < (d, g, cands[o].id));
                    if better {
                        nearest.insert(copy, (dist, gap, orig));
                    }
                }
            }
        }
        for (copy, (_, _, orig)) in nearest {
            if find(&mut parent, copy) != find(&mut parent, orig) {
                union(&mut parent, copy, orig);
                rep.near_pairs += 1;
            }
        }
        let mut by_root: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            by_root.entry(r).or_default().push(i);
        }
        let mut g = Groups::default();
        for mut members in by_root.into_values().filter(|m| m.len() > 1) {
            members.sort_by(|&a, &b| {
                rank(&cands[b].rec, cands[b].id).cmp(&rank(&cands[a].rec, cands[a].id))
            });
            let gi = g.groups.len();
            for &m in &members {
                g.group_of.insert(cands[m].id, gi);
            }
            rep.items += members.len();
            g.groups.push(
                members
                    .iter()
                    .map(|&m| Member {
                        id: cands[m].id,
                        label: cands[m].rec.label.clone(),
                    })
                    .collect(),
            );
        }
        rep.groups = g.groups.len();
        *self.inner.write().expect("dups lock") = g;
        Ok(rep)
    }

    /// Whether `id` is shown to a viewer who may see `labels` and has
    /// hidden `hidden`, and how many copies of it they may see. The shown
    /// copy is the best one that is visible and not hidden; hiding it
    /// shows the next, hiding all shows none.
    pub fn shown(&self, id: u64, labels: &[String], hidden: &HashSet<u64>) -> Shown {
        let g = self.inner.read().expect("dups lock");
        let Some(&gi) = g.group_of.get(&id) else {
            return Shown {
                shown: !hidden.contains(&id),
                copies: 1,
            };
        };
        let visible: Vec<&Member> = g.groups[gi]
            .iter()
            .filter(|m| labels.contains(&m.label))
            .collect();
        Shown {
            shown: visible
                .iter()
                .find(|m| !hidden.contains(&m.id))
                .is_some_and(|m| m.id == id),
            copies: visible.len().max(1),
        }
    }

    /// The copies of `id` the viewer may see, best first (empty when it has
    /// none).
    pub fn copies(&self, id: u64, labels: &[String]) -> Vec<u64> {
        let g = self.inner.read().expect("dups lock");
        let Some(&gi) = g.group_of.get(&id) else {
            return Vec::new();
        };
        g.groups[gi]
            .iter()
            .filter(|m| labels.contains(&m.label))
            .map(|m| m.id)
            .collect()
    }

    /// Groups of exact copies (same content) with at least two files in
    /// one library, for the cleanup list (REQ-DUP-003): (label, the ids of
    /// that library's copies, best first). Copies of the same picture in
    /// other files are left out: they are not the same file.
    pub fn exact_in(&self, idx: &Index, label: &str) -> Result<Vec<Vec<u64>>> {
        let ids: Vec<Vec<u64>> = {
            let g = self.inner.read().expect("dups lock");
            g.groups
                .iter()
                .map(|m| {
                    m.iter()
                        .filter(|m| m.label == label)
                        .map(|m| m.id)
                        .collect()
                })
                .collect()
        };
        let mut out = Vec::new();
        for members in ids.into_iter().filter(|m: &Vec<u64>| m.len() > 1) {
            let mut by_hash: HashMap<String, Vec<u64>> = HashMap::new();
            for id in members {
                if let Some(r) = idx.locate(id)? {
                    by_hash.entry(r.hash).or_default().push(id);
                }
            }
            out.extend(by_hash.into_values().filter(|v| v.len() > 1));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(w: u32, h: u32, date_source: &str, taken: &str, rel: &str) -> Record {
        Record {
            label: "p1".into(),
            rel: rel.as_bytes().to_vec(),
            size: 1000,
            mtime_ns: 0,
            hash: "x".into(),
            width: w,
            height: h,
            taken_local: taken.into(),
            taken_offset: None,
            date_source: date_source.into(),
            lat: None,
            lon: None,
            thumb: "ok".into(),
            missing: false,
            preview: false,
            video: None,
            live: None,
            live_of: None,
        }
    }

    #[test]
    fn copies_count_but_bursts_do_not() {
        let cam = rec(4000, 3000, "exif", "2024-07-01T10:00:00", "a/IMG_1.jpg");
        let burst = rec(4000, 3000, "exif", "2024-07-01T10:00:01", "a/IMG_2.jpg");
        let messenger = rec(
            1600,
            1200,
            "file-name",
            "2024-07-01T12:00:00",
            "w/IMG-WA.jpg",
        );
        let export = rec(2000, 1500, "exif", "2024-07-01T10:00:00", "e/IMG_1.jpg");
        let twin = rec(4000, 3000, "exif", "2024-07-01T10:00:00", "a/IMG_1.heic");
        let portrait = rec(1200, 1600, "file-name", "2024-07-01T12:00:00", "w/x.jpg");
        assert!(!same_picture(&cam, &burst));
        assert!(same_picture(&cam, &messenger));
        assert!(same_picture(&cam, &export));
        assert!(same_picture(&cam, &twin));
        assert!(!same_picture(&cam, &portrait));
    }

    #[test]
    fn flat_pictures_have_no_fingerprint() {
        let black = image::DynamicImage::new_rgb8(64, 48);
        assert_eq!(dhash(&black), None);
        let mut g = image::RgbImage::new(64, 48);
        for (x, _, p) in g.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 4) as u8, 0, 0]);
        }
        assert!(dhash(&image::DynamicImage::ImageRgb8(g)).is_some());
    }
}
