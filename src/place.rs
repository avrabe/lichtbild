//! Where a picture was taken when it does not say (REQ-PLACE-002, DD-022).
//!
//! A picture without a recorded position takes one from the pictures of the
//! same library taken shortly before and after it: between two of them
//! when both are close in time and near each other, or from one of them
//! when it is only minutes away. A position set by hand counts like a
//! recorded one, so placing one picture of a trip places its neighbours.
//! Estimates never feed other estimates, never cross libraries, and are
//! never written to the source files; they are worked out again from the
//! index whenever it changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::index::{Index, Record};

/// Both neighbours within this many seconds: a position between them.
pub const BETWEEN_MAX_S: i64 = 2 * 3600;
/// One neighbour within this many seconds: its position.
pub const NEAR_MAX_S: i64 = 30 * 60;
/// Estimates less certain than this are not made.
pub const RADIUS_MAX_M: f64 = 2000.0;

/// A position set by hand, by whom and when (unix seconds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placed {
    pub lat: f64,
    pub lon: f64,
    pub by: String,
    pub at: u64,
}

/// Where a position comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The file records it (EXIF, the video container).
    Recorded,
    /// Someone set it by hand.
    ByHand,
    /// Between the pictures before and after, `gap_s` apart in all.
    Between { gap_s: u32 },
    /// From one picture `gap_s` before or after.
    Near { gap_s: u32 },
}

impl How {
    pub fn estimated(self) -> bool {
        matches!(self, How::Between { .. } | How::Near { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
    /// How far off it may be, in metres (0 for a recorded position).
    pub radius_m: u32,
    pub how: How,
}

/// How many items have a position, by where it comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub recorded: u64,
    pub by_hand: u64,
    pub between: u64,
    pub near: u64,
    pub without: u64,
}

pub fn valid(lat: f64, lon: f64) -> bool {
    lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0
}

/// Great-circle distance in metres.
pub fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (
        a.0.to_radians(),
        a.1.to_radians(),
        b.0.to_radians(),
        b.1.to_radians(),
    );
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * h.sqrt().min(1.0).asin()
}

/// The camera's wall-clock time in seconds, for dates good enough to place
/// by: not the file's time, and not a video's UTC time (its local offset is
/// unknown, so it does not line up with the photos' local times).
fn when(rec: &Record) -> Option<i64> {
    if matches!(rec.date_source.as_str(), "file-time" | "video-utc") {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(&rec.taken_local, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|t| t.and_utc().timestamp())
}

/// One item on a library's time line: (time, id, its anchor position).
type Entry = (i64, u64, Option<(f64, f64)>);

/// Positions by item id.
pub type Positions = HashMap<u64, Position>;

/// Every live item's position, recorded, set by hand or estimated.
pub fn positions(items: &[(u64, Record)], placed: &HashMap<u64, Placed>) -> HashMap<u64, Position> {
    let mut out = HashMap::new();
    // Per library: (time, id, anchor position).
    let mut libs: HashMap<&str, Vec<Entry>> = HashMap::new();
    for (id, rec) in items {
        if !rec.shown() {
            continue;
        }
        let anchor = match (rec.lat, rec.lon) {
            (Some(lat), Some(lon)) if valid(lat, lon) => {
                out.insert(
                    *id,
                    Position {
                        lat,
                        lon,
                        radius_m: 0,
                        how: How::Recorded,
                    },
                );
                Some((lat, lon))
            }
            _ => match placed.get(id) {
                Some(p) if valid(p.lat, p.lon) => {
                    out.insert(
                        *id,
                        Position {
                            lat: p.lat,
                            lon: p.lon,
                            radius_m: 0,
                            how: How::ByHand,
                        },
                    );
                    Some((p.lat, p.lon))
                }
                _ => None,
            },
        };
        if let Some(t) = when(rec) {
            libs.entry(rec.label.as_str())
                .or_default()
                .push((t, *id, anchor));
        }
    }
    for (_, mut v) in libs {
        v.sort_by_key(|e| (e.0, e.1));
        // The nearest anchor before each entry, then after it.
        let mut before = vec![None; v.len()];
        let mut last = None;
        for (i, e) in v.iter().enumerate() {
            if let Some(p) = e.2 {
                last = Some((e.0, p));
            }
            before[i] = last;
        }
        let mut next = None;
        for i in (0..v.len()).rev() {
            let (t, id, anchor) = v[i];
            if let Some(p) = anchor {
                next = Some((t, p));
                continue;
            }
            if let Some(p) = estimate(t, before[i], next) {
                out.insert(id, p);
            }
        }
    }
    out
}

/// A position for a picture taken at `t`, from the anchors before and
/// after it (time, position): between them, or from the nearer one,
/// whichever is more certain.
fn estimate(
    t: i64,
    prev: Option<(i64, (f64, f64))>,
    next: Option<(i64, (f64, f64))>,
) -> Option<Position> {
    let gap = |a: i64, b: i64| (b - a).max(0);
    let mut between = None;
    if let (Some((tp, pp)), Some((tn, pn))) = (prev, next) {
        let (gp, gn) = (gap(tp, t), gap(t, tn));
        if gp <= BETWEEN_MAX_S && gn <= BETWEEN_MAX_S && (pp.1 - pn.1).abs() <= 180.0 {
            let r = 100.0 + distance_m(pp, pn) / 2.0;
            let f = if gp + gn == 0 {
                0.5
            } else {
                gp as f64 / (gp + gn) as f64
            };
            between = Some(Position {
                lat: pp.0 + (pn.0 - pp.0) * f,
                lon: pp.1 + (pn.1 - pp.1) * f,
                radius_m: r.round() as u32,
                how: How::Between {
                    gap_s: (gp + gn) as u32,
                },
            });
        }
    }
    // One side: the nearer one in time, if minutes away. Someone walking
    // covers about a kilometre in a quarter of an hour.
    let near = [
        prev.map(|(tp, p)| (gap(tp, t), p)),
        next.map(|(tn, p)| (gap(t, tn), p)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(g, _)| *g)
    .filter(|(g, _)| *g <= NEAR_MAX_S)
    .map(|(g, p)| Position {
        lat: p.0,
        lon: p.1,
        radius_m: (100.0 + g as f64).round() as u32,
        how: How::Near { gap_s: g as u32 },
    });
    [between, near]
        .into_iter()
        .flatten()
        .filter(|p| f64::from(p.radius_m) <= RADIUS_MAX_M)
        .min_by_key(|p| p.radius_m)
}

/// Counts by source, over the live items of `labels`.
pub fn counts(items: &[(u64, Record)], pos: &HashMap<u64, Position>, labels: &[String]) -> Counts {
    let mut c = Counts::default();
    for (id, rec) in items {
        if !rec.shown() || !labels.contains(&rec.label) {
            continue;
        }
        match pos.get(id).map(|p| p.how) {
            Some(How::Recorded) => c.recorded += 1,
            Some(How::ByHand) => c.by_hand += 1,
            Some(How::Between { .. }) => c.between += 1,
            Some(How::Near { .. }) => c.near += 1,
            None => c.without += 1,
        }
    }
    c
}

/// Positions worked out once per index generation, shared by the map,
/// the viewer and the status page.
#[derive(Default)]
pub struct Cache {
    inner: Mutex<Option<(u64, Arc<Positions>)>>,
}

impl Cache {
    pub fn get(&self, idx: &Index) -> Result<Arc<HashMap<u64, Position>>> {
        let generation = idx.generation();
        if let Some((g, p)) = &*self.inner.lock().unwrap_or_else(|e| e.into_inner()) {
            if *g == generation {
                return Ok(p.clone());
            }
        }
        let p = Arc::new(positions(&idx.all()?, &idx.placed()?));
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, p.clone()));
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(label: &str, taken: &str, pos: Option<(f64, f64)>) -> Record {
        Record {
            label: label.into(),
            rel: taken.as_bytes().to_vec(),
            size: 1,
            mtime_ns: 0,
            hash: taken.into(),
            width: 4,
            height: 3,
            taken_local: taken.into(),
            taken_offset: None,
            date_source: "exif".into(),
            lat: pos.map(|p| p.0),
            lon: pos.map(|p| p.1),
            thumb: "ok".into(),
            missing: false,
            preview: false,
            video: None,
            live: None,
            live_of: None,
        }
    }

    // Synthetic positions: a square near 0°N 0°E (Gulf of Guinea).
    const A: (f64, f64) = (0.0, 0.0);
    const B: (f64, f64) = (0.0, 0.009); // about 1 km east of A

    #[test]
    fn between_two_neighbours_by_time() {
        let items = vec![
            (1, rec("p1", "2024-05-01T10:00:00", Some(A))),
            (2, rec("p1", "2024-05-01T10:15:00", None)),
            (3, rec("p1", "2024-05-01T11:00:00", Some(B))),
        ];
        let p = positions(&items, &HashMap::new());
        let e = p[&2];
        assert_eq!(e.how, How::Between { gap_s: 3600 });
        assert!(
            (e.lon - 0.009 / 4.0).abs() < 1e-9,
            "a quarter of the way: {e:?}"
        );
        assert!((550..=610).contains(&e.radius_m), "{e:?}");
        assert_eq!(p[&1].how, How::Recorded);
    }

    #[test]
    fn near_one_neighbour_only_within_minutes() {
        let items = vec![
            (1, rec("p1", "2024-05-01T10:00:00", Some(A))),
            (2, rec("p1", "2024-05-01T10:10:00", None)),
            (3, rec("p1", "2024-05-01T11:00:00", None)),
        ];
        let p = positions(&items, &HashMap::new());
        assert_eq!(p[&2].how, How::Near { gap_s: 600 });
        assert_eq!(p[&2].radius_m, 700);
        assert!(!p.contains_key(&3), "an hour away is too far");
    }

    #[test]
    fn far_apart_neighbours_give_no_position_between() {
        let far = (0.0, 0.5); // about 55 km
        let items = vec![
            (1, rec("p1", "2024-05-01T10:00:00", Some(A))),
            (2, rec("p1", "2024-05-01T10:45:00", None)),
            (3, rec("p1", "2024-05-01T11:00:00", Some(far))),
        ];
        let p = positions(&items, &HashMap::new());
        // Not between; the nearer one, 15 minutes away, is close enough.
        assert_eq!(p[&2].how, How::Near { gap_s: 900 });
        assert_eq!((p[&2].lat, p[&2].lon), far);
    }

    #[test]
    fn libraries_and_weak_dates_do_not_mix() {
        let mut weak = rec("p1", "2024-05-01T10:01:00", None);
        weak.date_source = "file-time".into();
        let items = vec![
            (1, rec("p1", "2024-05-01T10:00:00", Some(A))),
            (2, rec("p2", "2024-05-01T10:01:00", None)),
            (3, weak),
        ];
        let p = positions(&items, &HashMap::new());
        assert!(
            !p.contains_key(&2),
            "another library's picture does not place it"
        );
        assert!(!p.contains_key(&3), "a file time does not place it");
    }

    #[test]
    fn a_hand_placed_picture_places_its_neighbours_but_not_a_recorded_one() {
        let items = vec![
            (1, rec("p1", "2024-05-01T10:00:00", None)),
            (2, rec("p1", "2024-05-01T10:05:00", None)),
            (3, rec("p1", "2024-05-01T12:00:00", Some(A))),
        ];
        let placed = HashMap::from([
            (
                1,
                Placed {
                    lat: B.0,
                    lon: B.1,
                    by: "p1".into(),
                    at: 0,
                },
            ),
            (
                3,
                Placed {
                    lat: B.0,
                    lon: B.1,
                    by: "p1".into(),
                    at: 0,
                },
            ),
        ]);
        let p = positions(&items, &placed);
        assert_eq!(p[&1].how, How::ByHand);
        assert_eq!(p[&2].how, How::Near { gap_s: 300 });
        assert_eq!((p[&2].lat, p[&2].lon), B);
        assert_eq!(p[&3].how, How::Recorded, "the file's own position wins");
        assert_eq!((p[&3].lat, p[&3].lon), A);
        let c = counts(&items, &p, &["p1".into()]);
        assert_eq!(
            c,
            Counts {
                recorded: 1,
                by_hand: 1,
                near: 1,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn estimates_do_not_chain() {
        // 2 is placed from 1; 3 is 25 min after 2 but 50 min after 1.
        let items = vec![
            (1, rec("p1", "2024-05-01T10:00:00", Some(A))),
            (2, rec("p1", "2024-05-01T10:25:00", None)),
            (3, rec("p1", "2024-05-01T10:50:00", None)),
        ];
        let p = positions(&items, &HashMap::new());
        assert!(p.contains_key(&2));
        assert!(!p.contains_key(&3));
        // The same, backwards in time.
        let items = vec![
            (1, rec("p1", "2024-05-01T10:50:00", Some(A))),
            (2, rec("p1", "2024-05-01T10:25:00", None)),
            (3, rec("p1", "2024-05-01T10:00:00", None)),
        ];
        let p = positions(&items, &HashMap::new());
        assert!(p.contains_key(&2));
        assert!(!p.contains_key(&3));
    }
}
