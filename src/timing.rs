//! How quickly the server answers (REQ-WEB-025): every request's route
//! (the template, such as `/v/{who}/{id}`, never the ids or a query) and
//! how long the answer took, for the last hour, in memory. The status page
//! shows the typical and the slow answers; a slow one is also logged.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long the record goes back.
pub const WINDOW: Duration = Duration::from_secs(3600);
/// At most this many answers are kept (the oldest go first).
pub const KEEP: usize = 20_000;
/// An answer slower than this is logged and listed.
pub const SLOW: Duration = Duration::from_secs(1);

/// What a request was for: a page someone waits for, or a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Page,
    Picture,
}

/// The kind of a route, or None for what is not worth counting (static
/// files, map tiles and assets, health checks).
pub fn kind_of(route: &str) -> Option<Kind> {
    let first = route
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    match first {
        "static" | "healthz" => None,
        "map" if route.starts_with("/map/tiles") || route.starts_with("/map/assets") => None,
        "thumb" | "preview" | "original" | "face" | "captions" => Some(Kind::Picture),
        _ => Some(Kind::Page),
    }
}

#[derive(Debug, Clone)]
struct Answer {
    at: Instant,
    route: String,
    kind: Kind,
    took: Duration,
}

#[derive(Default)]
pub struct Timings {
    answers: Mutex<VecDeque<Answer>>,
}

/// The last hour of one kind of answer.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub median: Duration,
    pub p95: Duration,
    /// The slowest routes over SLOW: (route, slowest answer, how many).
    pub slow: Vec<(String, Duration, usize)>,
}

impl Timings {
    pub fn record(&self, route: &str, took: Duration) {
        self.record_at(Instant::now(), route, took);
    }

    pub fn record_at(&self, at: Instant, route: &str, took: Duration) {
        let Some(kind) = kind_of(route) else { return };
        if took >= SLOW {
            tracing::warn!("slow answer: {route} took {} ms", took.as_millis());
        }
        let mut a = self.answers.lock().unwrap_or_else(|e| e.into_inner());
        while a.front().is_some_and(|x| at.duration_since(x.at) > WINDOW) || a.len() >= KEEP {
            a.pop_front();
        }
        a.push_back(Answer {
            at,
            route: route.to_string(),
            kind,
            took,
        });
    }

    /// The answers of `kind` in the last hour before `now`.
    pub fn summary(&self, kind: Kind, now: Instant) -> Summary {
        let a = self.answers.lock().unwrap_or_else(|e| e.into_inner());
        let mut took: Vec<Duration> = Vec::new();
        let mut slow: std::collections::HashMap<&str, (Duration, usize)> = Default::default();
        for x in a.iter() {
            if x.kind != kind || now.saturating_duration_since(x.at) > WINDOW {
                continue;
            }
            took.push(x.took);
            if x.took >= SLOW {
                let e = slow.entry(x.route.as_str()).or_default();
                e.0 = e.0.max(x.took);
                e.1 += 1;
            }
        }
        if took.is_empty() {
            return Summary::default();
        }
        took.sort_unstable();
        let at = |q: f64| took[((took.len() - 1) as f64 * q).round() as usize];
        let mut slow: Vec<(String, Duration, usize)> = slow
            .into_iter()
            .map(|(r, (d, n))| (r.to_string(), d, n))
            .collect();
        slow.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        slow.truncate(5);
        Summary {
            n: took.len(),
            median: at(0.5),
            p95: at(0.95),
            slow,
        }
    }
}

/// "35 ms", "2.4 s".
pub fn short(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_pages_pictures_or_not_counted() {
        assert_eq!(kind_of("/v/{who}/{id}"), Some(Kind::Page));
        assert_eq!(kind_of("/"), Some(Kind::Page));
        assert_eq!(kind_of("/map"), Some(Kind::Page));
        assert_eq!(kind_of("/map/clusters"), Some(Kind::Page));
        assert_eq!(kind_of("/thumb/{id}"), Some(Kind::Picture));
        assert_eq!(kind_of("/static/{file}"), None);
        assert_eq!(kind_of("/map/tiles.pmtiles"), None);
        assert_eq!(kind_of("/healthz"), None);
    }

    #[test]
    fn the_last_hour_typical_and_slow() {
        let t = Timings::default();
        let now = Instant::now();
        let old = now - Duration::from_secs(4000);
        t.record_at(old, "/search", Duration::from_secs(9));
        for ms in 1..=100u64 {
            t.record_at(now, "/t/{who}", Duration::from_millis(ms));
        }
        t.record_at(now, "/search", Duration::from_millis(2400));
        t.record_at(now, "/search", Duration::from_millis(1200));
        t.record_at(now, "/thumb/{id}", Duration::from_millis(5));
        let s = t.summary(Kind::Page, now);
        assert_eq!(s.n, 102);
        assert_eq!(s.median, Duration::from_millis(52));
        assert_eq!(s.p95, Duration::from_millis(97));
        assert_eq!(
            s.slow,
            vec![("/search".to_string(), Duration::from_millis(2400), 2)]
        );
        assert_eq!(t.summary(Kind::Picture, now).n, 1);
        assert_eq!(short(Duration::from_millis(2400)), "2.4 s");
        assert_eq!(short(Duration::from_millis(35)), "35 ms");
    }
}
