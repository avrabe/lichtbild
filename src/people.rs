//! Who is in the photos (REQ-FACE-002, REQ-FACE-003, DD-025): the faces
//! of all libraries, linked to their closest look-alikes, and grouped by
//! passing names along those links. A face someone named keeps its name; an
//! unnamed face takes the name most of its look-alikes carry, unless it is
//! marked "not them" or is too far from the faces named so; what stays
//! unnamed forms groups of one person each, waiting for a name.
//!
//! Names are the household's (owner's decision): one name for everyone.
//! What a viewer is shown is cut to the photos they may see by the caller.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::Result;

/// Two faces are linked from this cosine on (DD-025: AuraFace on public
/// portraits: other people at most 0.25, p99 0.24; the same person p10
/// 0.44, median 0.66).
pub const LINK: f32 = 0.35;
/// An unnamed face takes a name only this close to the mean of the faces
/// named so, so a name does not creep along a chain of look-alikes.
pub const JOIN: f32 = 0.35;
/// Links kept per face (the closest).
const NEIGHBOURS: usize = 24;
/// Rounds of passing names along.
const ROUNDS: usize = 12;

/// A face: content hash of its photo and its number there.
pub type Key = (String, u32);

/// What a face belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Who {
    /// A named person, by id.
    Person(u64),
    /// An unnamed group, by the number of its first face.
    Group(u32),
}

#[derive(Default)]
struct State {
    keys: Vec<Key>,
    /// Each face's box in its photo (shares) and detector score.
    boxes: Vec<[f32; 4]>,
    vectors: Vec<Vec<i8>>,
    links: Vec<Vec<(u32, f32)>>,
    who: Vec<Who>,
    /// Whether a face carries its name because someone gave it.
    named: Vec<bool>,
    by_key: HashMap<Key, u32>,
    by_hash: HashMap<String, Vec<u32>>,
}

/// The faces and who they are, rebuilt after a faces pass and after every
/// change of a name.
#[derive(Default)]
pub struct People {
    state: RwLock<State>,
    /// The faces were read from the index at least once since the start.
    loaded: std::sync::atomic::AtomicBool,
}

/// One face as callers see it.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceInfo {
    pub n: u32,
    pub key: Key,
    pub b: [f32; 4],
    pub who: Who,
    /// Named by someone, not passed along.
    pub named: bool,
}

/// The closest look-alikes of every face, from LINK on.
fn link(vectors: &[Vec<i8>]) -> Vec<Vec<(u32, f32)>> {
    let n = vectors.len();
    let norms: Vec<f32> = vectors
        .iter()
        .map(|v| {
            v.iter()
                .map(|x| (*x as i32 * *x as i32) as f32)
                .sum::<f32>()
                .sqrt()
                .max(1e-9)
        })
        .collect();
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(4);
    let chunk = n.div_ceil(threads).max(1);
    let mut out: Vec<Vec<(u32, f32)>> = vec![Vec::new(); n];
    std::thread::scope(|s| {
        for (c, part) in out.chunks_mut(chunk).enumerate() {
            let (vectors, norms) = (&vectors, &norms);
            s.spawn(move || {
                for (o, slot) in part.iter_mut().enumerate() {
                    let i = c * chunk + o;
                    let a = &vectors[i];
                    let mut near: Vec<(u32, f32)> = Vec::new();
                    for (j, b) in vectors.iter().enumerate() {
                        if j == i {
                            continue;
                        }
                        let dot: i32 = a.iter().zip(b).map(|(x, y)| *x as i32 * *y as i32).sum();
                        let c = dot as f32 / (norms[i] * norms[j]);
                        if c >= LINK {
                            near.push((j as u32, c));
                        }
                    }
                    near.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
                    near.truncate(NEIGHBOURS);
                    *slot = near;
                }
            });
        }
    });
    out
}

/// Pass names along the links. `fixed` holds the named faces; `not` the
/// names a face may not take.
fn label(
    vectors: &[Vec<i8>],
    links: &[Vec<(u32, f32)>],
    fixed: &HashMap<u32, u64>,
    not: &HashSet<(u32, u64)>,
) -> Vec<Who> {
    let n = vectors.len();
    // The mean of each person's named faces.
    let mut sums: HashMap<u64, (Vec<f32>, usize)> = HashMap::new();
    for (&i, &p) in fixed {
        let e = sums
            .entry(p)
            .or_insert_with(|| (vec![0.0; vectors[i as usize].len()], 0));
        for (s, x) in e.0.iter_mut().zip(&vectors[i as usize]) {
            *s += *x as f32;
        }
        e.1 += 1;
    }
    let means: HashMap<u64, Vec<f32>> = sums
        .into_iter()
        .map(|(p, (s, _))| {
            let norm = s.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            (p, s.iter().map(|x| x / norm).collect())
        })
        .collect();
    let close_to = |i: usize, p: u64| -> bool {
        let Some(m) = means.get(&p) else { return false };
        let v = &vectors[i];
        let nv = v
            .iter()
            .map(|x| (*x as f32) * (*x as f32))
            .sum::<f32>()
            .sqrt()
            .max(1e-9);
        v.iter().zip(m).map(|(x, y)| *x as f32 * y).sum::<f32>() / nv >= JOIN
    };
    let mut who: Vec<Who> = (0..n)
        .map(|i| match fixed.get(&(i as u32)) {
            Some(p) => Who::Person(*p),
            None => Who::Group(i as u32),
        })
        .collect();
    for _ in 0..ROUNDS {
        let mut changed = false;
        for i in 0..n {
            if fixed.contains_key(&(i as u32)) {
                continue;
            }
            let mut votes: HashMap<Who, f32> = HashMap::new();
            for &(j, c) in &links[i] {
                let w = who[j as usize];
                if let Who::Person(p) = w {
                    if not.contains(&(i as u32, p)) || !close_to(i, p) {
                        continue;
                    }
                }
                // A face someone named counts double: a person said so.
                let weight = if fixed.contains_key(&j) { 2.0 } else { 1.0 };
                *votes.entry(w).or_default() += c * weight;
            }
            // Stay as one's own group when nothing pulls.
            let best = votes
                .into_iter()
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
                .map(|(w, _)| w);
            if let Some(b) = best {
                if b != who[i] {
                    who[i] = b;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // An unnamed group is known by its first face.
    let mut first: HashMap<u32, u32> = HashMap::new();
    for (i, w) in who.iter().enumerate() {
        if let Who::Group(g) = w {
            first.entry(*g).or_insert(i as u32);
        }
    }
    who.into_iter()
        .map(|w| match w {
            Who::Group(g) => Who::Group(first[&g]),
            p => p,
        })
        .collect()
}

impl People {
    /// Load every face from the index and link them (the costly part).
    pub fn rebuild(&self, idx: &crate::index::Index) -> Result<usize> {
        let mut keys = Vec::new();
        let mut boxes = Vec::new();
        let mut vectors = Vec::new();
        let mut all = idx.all_faces()?;
        all.sort_by(|a, b| a.0.cmp(&b.0));
        for (hash, faces) in all {
            for (i, f) in faces.into_iter().enumerate() {
                if f.v.len() == crate::faces::DIM {
                    keys.push((hash.clone(), i as u32));
                    boxes.push(f.b);
                    vectors.push(f.v);
                }
            }
        }
        let links = link(&vectors);
        let n = keys.len();
        let mut by_key = HashMap::new();
        let mut by_hash: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, k) in keys.iter().enumerate() {
            by_key.insert(k.clone(), i as u32);
            by_hash.entry(k.0.clone()).or_default().push(i as u32);
        }
        {
            let mut s = self.state.write().expect("people lock");
            *s = State {
                keys,
                boxes,
                vectors,
                links,
                who: Vec::new(),
                named: Vec::new(),
                by_key,
                by_hash,
            };
        }
        self.relabel(idx)?;
        self.loaded
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(n)
    }

    /// Pass the names along again (after a name was given or taken).
    pub fn relabel(&self, idx: &crate::index::Index) -> Result<()> {
        let named = idx.named_faces()?;
        let nots = idx.not_faces()?;
        let people: HashSet<u64> = idx.people()?.into_iter().map(|(id, _)| id).collect();
        let mut s = self.state.write().expect("people lock");
        let mut fixed = HashMap::new();
        for (k, p) in named {
            if let Some(&i) = s.by_key.get(&k) {
                if people.contains(&p) {
                    fixed.insert(i, p);
                }
            }
        }
        let mut not = HashSet::new();
        for (h, f, p) in nots {
            if let Some(&i) = s.by_key.get(&(h, f)) {
                not.insert((i, p));
            }
        }
        let who = label(&s.vectors, &s.links, &fixed, &not);
        s.named = (0..who.len() as u32)
            .map(|i| fixed.contains_key(&i))
            .collect();
        s.who = who;
        Ok(())
    }

    /// The faces in one photo, by content hash, in their order there.
    pub fn in_photo(&self, hash: &str) -> Vec<FaceInfo> {
        let s = self.state.read().expect("people lock");
        s.by_hash
            .get(hash)
            .map(|v| v.iter().map(|&i| s.info(i)).collect())
            .unwrap_or_default()
    }

    /// Every face, for the people page and search; the caller cuts them
    /// to what the viewer may see.
    pub fn all(&self) -> Vec<FaceInfo> {
        let s = self.state.read().expect("people lock");
        (0..s.keys.len() as u32).map(|i| s.info(i)).collect()
    }

    /// A face by its number, if the numbering is still the same.
    pub fn face(&self, n: u32, key: &Key) -> Option<FaceInfo> {
        let s = self.state.read().expect("people lock");
        (s.keys.get(n as usize) == Some(key)).then(|| s.info(n))
    }

    pub fn len(&self) -> usize {
        self.state.read().expect("people lock").keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// False until the faces were first read from the index after a start,
    /// which takes about a minute on a Pi with tens of thousands of faces.
    pub fn loaded(&self) -> bool {
        self.loaded.load(std::sync::atomic::Ordering::Acquire)
    }
}

impl State {
    fn info(&self, i: u32) -> FaceInfo {
        FaceInfo {
            n: i,
            key: self.keys[i as usize].clone(),
            b: self.boxes[i as usize],
            who: self.who.get(i as usize).copied().unwrap_or(Who::Group(i)),
            named: self.named.get(i as usize).copied().unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vector near direction `d` of 8, with a little of `e`.
    fn v(d: usize, e: usize, k: i8) -> Vec<i8> {
        let mut x = vec![0i8; crate::faces::DIM];
        x[d] = 100;
        x[e] = k;
        x
    }

    #[test]
    fn look_alikes_link_and_strangers_do_not() {
        let vs = vec![v(0, 1, 20), v(0, 2, 30), v(5, 6, 10)];
        let l = link(&vs);
        assert_eq!(l[0].len(), 1);
        assert_eq!(l[0][0].0, 1);
        assert!(l[2].is_empty());
    }

    #[test]
    fn unnamed_look_alikes_form_one_group() {
        let vs = vec![
            v(0, 1, 20),
            v(0, 2, 30),
            v(0, 3, 10),
            v(5, 6, 10),
            v(5, 7, 20),
        ];
        let who = label(&vs, &link(&vs), &HashMap::new(), &HashSet::new());
        assert_eq!(who[0], who[1]);
        assert_eq!(who[1], who[2]);
        assert_eq!(who[3], who[4]);
        assert_ne!(who[0], who[3]);
        assert_eq!(who[0], Who::Group(0));
        assert_eq!(who[3], Who::Group(3));
    }

    #[test]
    fn a_name_passes_to_look_alikes_but_not_to_strangers_or_refusers() {
        let vs = vec![v(0, 1, 20), v(0, 2, 30), v(0, 3, 10), v(5, 6, 10)];
        let fixed = HashMap::from([(0u32, 7u64)]);
        let who = label(&vs, &link(&vs), &fixed, &HashSet::new());
        assert_eq!(who[..3], [Who::Person(7); 3]);
        assert_eq!(who[3], Who::Group(3));
        let not = HashSet::from([(2u32, 7u64)]);
        let who = label(&vs, &link(&vs), &fixed, &not);
        assert_eq!(who[1], Who::Person(7));
        assert_ne!(who[2], Who::Person(7));
    }
}
