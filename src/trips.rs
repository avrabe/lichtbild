//! Trips (REQ-TRIP-001): days whose pictures were taken far from home,
//! one after another, grouped and named by the places visited. Home is the
//! place with the most days of pictures; nothing leaves the house, and a
//! viewer's trips come only from the items they may see.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;

use crate::index::Record;
use crate::place::{distance_m, Position};
use crate::places::Places;

/// A day's pictures this far from home, at least, make it a day away.
pub const AWAY_KM: f64 = 50.0;
/// Days away at most this many days apart (a day without pictures between
/// them) are one trip.
pub const GAP_DAYS: i64 = 2;
/// Items a trip holds at least.
pub const MIN_ITEMS: usize = 3;

/// One trip: its first and last day, the places visited (most days first),
/// how many items it holds, and the photo shown for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Trip {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub places: Vec<String>,
    pub items: usize,
    pub cover: u64,
}

/// A run of days away: first day, last day, where each day was.
type Run = (NaiveDate, NaiveDate, Vec<(f64, f64)>);

#[derive(Default)]
struct Day {
    lat: f64,
    lon: f64,
    placed: usize,
    ids: Vec<u64>,
    cover: Option<u64>,
}

impl Day {
    fn at(&self) -> Option<(f64, f64)> {
        (self.placed > 0).then(|| (self.lat / self.placed as f64, self.lon / self.placed as f64))
    }
}

/// The trips in `items`, newest first. `pos` holds the items' positions
/// (recorded, set by hand or estimated); `places` names them. Without place
/// names, places are told apart by a grid of about 20 km and trips have no
/// names.
pub fn find(
    items: &[(u64, Record)],
    pos: &HashMap<u64, Position>,
    places: Option<&Places>,
) -> Vec<Trip> {
    let mut days: BTreeMap<NaiveDate, Day> = BTreeMap::new();
    for (id, rec) in items {
        let Some(day) = rec
            .taken_local
            .get(..10)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        else {
            continue;
        };
        let d = days.entry(day).or_default();
        d.ids.push(*id);
        if d.cover.is_none() && rec.video.is_none() && rec.thumb == "ok" {
            d.cover = Some(*id);
        }
        if let Some(p) = pos.get(id) {
            d.lat += p.lat;
            d.lon += p.lon;
            d.placed += 1;
        }
    }
    let name = |at: (f64, f64)| -> String {
        match places.and_then(|p| p.nearest(at.0, at.1)) {
            Some(n) => n.label,
            None => format!("{:.0}:{:.0}", at.0 * 5.0, at.1 * 5.0),
        }
    };
    // Home: the place with the most days, at the mean of those days.
    let mut per: HashMap<String, (usize, f64, f64)> = HashMap::new();
    for at in days.values().filter_map(Day::at) {
        let e = per.entry(name(at)).or_default();
        e.0 += 1;
        e.1 += at.0;
        e.2 += at.1;
    }
    let Some((_, (n, lat, lon))) = per
        .into_iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(b.0.cmp(&a.0)))
    else {
        return Vec::new();
    };
    let home = (lat / n as f64, lon / n as f64);

    // Runs of days away; a day at home ends one.
    let mut runs: Vec<Run> = Vec::new();
    let mut open = false;
    for (day, d) in &days {
        let Some(at) = d.at() else { continue };
        if distance_m(at, home) <= AWAY_KM * 1000.0 {
            open = false;
            continue;
        }
        match runs.last_mut() {
            Some(r) if open && (*day - r.1).num_days() <= GAP_DAYS => {
                r.1 = *day;
                r.2.push(at);
            }
            _ => runs.push((*day, *day, vec![at])),
        }
        open = true;
    }

    let mut trips: Vec<Trip> = runs
        .into_iter()
        .filter_map(|(start, end, ats)| {
            let inside: Vec<&Day> = days.range(start..=end).map(|(_, d)| d).collect();
            let n: usize = inside.iter().map(|d| d.ids.len()).sum();
            if n < MIN_ITEMS {
                return None;
            }
            let cover = inside
                .get(inside.len() / 2)
                .and_then(|d| d.cover)
                .or_else(|| inside.iter().find_map(|d| d.cover))
                .or_else(|| inside.first().and_then(|d| d.ids.first().copied()))?;
            let mut count: Vec<(String, usize)> = Vec::new();
            if places.is_some() {
                for at in ats {
                    let p = name(at);
                    match count.iter_mut().find(|(q, _)| *q == p) {
                        Some(c) => c.1 += 1,
                        None => count.push((p, 1)),
                    }
                }
            }
            count.sort_by_key(|c| std::cmp::Reverse(c.1));
            Some(Trip {
                start,
                end,
                places: count.into_iter().map(|(p, _)| p).collect(),
                items: n,
                cover,
            })
        })
        .collect();
    trips.reverse();
    trips
}
