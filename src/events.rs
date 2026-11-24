//! Events (REQ-TRIP-002): many pictures taken close together in time, as
//! at a party, a wedding or a day out. A pause of more than GAP_MIN minutes
//! ends one; an event holds at least MIN_ITEMS pictures. It is named by the
//! town most of its positioned pictures are nearest to.

use std::collections::HashMap;

use chrono::NaiveDateTime;

use crate::index::Record;
use crate::media::DateSource;
use crate::place::Position;
use crate::places::Places;

/// A pause this long, at most, keeps pictures in one event.
pub const GAP_MIN: i64 = 90;
/// Pictures an event holds at least.
pub const MIN_ITEMS: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    /// Oldest first.
    pub ids: Vec<u64>,
    pub place: Option<String>,
    /// A photo from the middle of the event.
    pub cover: u64,
}

fn time(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s.get(..19)?, "%Y-%m-%dT%H:%M:%S").ok()
}

/// The events in `items`, newest first. Items with a date only guessed
/// from a file name or time stamp are left out: such dates bunch up
/// when files were copied.
pub fn find(
    items: &[(u64, Record)],
    pos: &HashMap<u64, Position>,
    places: Option<&Places>,
) -> Vec<Event> {
    let mut timed: Vec<(NaiveDateTime, u64, &Record)> = items
        .iter()
        .filter(|(_, r)| !DateSource::is_weak(&r.date_source))
        .filter_map(|(id, r)| Some((time(&r.taken_local)?, *id, r)))
        .collect();
    timed.sort_by_key(|(t, id, _)| (*t, *id));
    let mut runs: Vec<Vec<(NaiveDateTime, u64, &Record)>> = Vec::new();
    for e in timed {
        match runs.last_mut() {
            Some(run) if (e.0 - run.last().map_or(e.0, |l| l.0)).num_minutes() <= GAP_MIN => {
                run.push(e)
            }
            _ => runs.push(vec![e]),
        }
    }
    let mut out: Vec<Event> = runs
        .into_iter()
        .filter(|run| run.len() >= MIN_ITEMS)
        .map(|run| {
            let mut names: HashMap<String, usize> = HashMap::new();
            if let Some(pl) = places {
                for (_, id, _) in &run {
                    if let Some(n) = pos.get(id).and_then(|p| pl.nearest(p.lat, p.lon)) {
                        *names.entry(n.label).or_default() += 1;
                    }
                }
            }
            let place = names
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                .map(|(n, _)| n);
            let photos: Vec<u64> = run
                .iter()
                .filter(|(_, _, r)| r.video.is_none() && r.thumb == "ok")
                .map(|(_, id, _)| *id)
                .collect();
            let cover = photos
                .get(photos.len() / 2)
                .copied()
                .unwrap_or(run[run.len() / 2].1);
            Event {
                start: run[0].0,
                end: run[run.len() - 1].0,
                ids: run.iter().map(|(_, id, _)| *id).collect(),
                place,
                cover,
            }
        })
        .collect();
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(taken: &str, source: &str) -> Record {
        Record {
            label: "p1".into(),
            rel: taken.as_bytes().to_vec(),
            size: 1,
            mtime_ns: 0,
            hash: taken.into(),
            width: 4,
            height: 3,
            taken_local: taken.into(),
            taken_offset: None,
            date_source: source.into(),
            lat: None,
            lon: None,
            thumb: "ok".into(),
            missing: false,
            preview: false,
            video: None,
            live: None,
            live_of: None,
            added: 0,
        }
    }

    /// `n` pictures from `start` (HH:MM on 2024-07-06), `every` minutes apart.
    fn burst(first_id: u64, start: &str, n: u64, every: u64) -> Vec<(u64, Record)> {
        let t0 = time(&format!("2024-07-06T{start}:00")).unwrap();
        (0..n)
            .map(|i| {
                let t = t0 + chrono::Duration::minutes((i * every) as i64);
                (
                    first_id + i,
                    rec(&t.format("%Y-%m-%dT%H:%M:%S").to_string(), "exif"),
                )
            })
            .collect()
    }

    #[test]
    fn a_long_pause_splits_and_small_runs_are_no_events() {
        let mut items = burst(1, "10:00", 15, 5); // 10:00..11:10
        items.extend(burst(100, "14:00", 20, 3)); // a party after 3 h
        items.extend(burst(200, "20:00", 5, 1)); // too few
        let ev = find(&items, &HashMap::new(), None);
        assert_eq!(ev.len(), 2);
        assert_eq!((ev[0].ids.len(), ev[0].ids[0]), (20, 100), "newest first");
        assert_eq!((ev[1].ids.len(), ev[1].ids[0]), (15, 1));
        assert_eq!(ev[1].end.format("%H:%M").to_string(), "11:10");
        assert!(ev[1].ids.contains(&ev[1].cover));
    }

    #[test]
    fn guessed_dates_make_no_event() {
        let items: Vec<(u64, Record)> = burst(1, "10:00", 30, 1)
            .into_iter()
            .map(|(id, mut r)| {
                r.date_source = "mtime".into();
                (id, r)
            })
            .collect();
        assert!(find(&items, &HashMap::new(), None).is_empty());
    }
}
