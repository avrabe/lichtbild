//! What the server is doing, for the status page (REQ-WEB-009): the scan,
//! the transcription pass, duplicates and search, updated by the scan
//! thread. Counts and times only; no paths or names.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::stt::PassReport;

/// The scan thread's current work.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Starting,
    Scanning {
        since: SystemTime,
    },
    Transcribing {
        since: SystemTime,
        done: u64,
        total: u64,
        report: PassReport,
    },
    Idle {
        next: Option<SystemTime>,
    },
}

/// The last finished scan.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanDone {
    pub ended: SystemTime,
    pub took: Duration,
    pub found: u64,
    pub indexed: u64,
    pub skipped: u64,
    pub reconciled: bool,
    /// The scan failed; why goes to the server log only, since the error
    /// can name paths.
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PassDone {
    pub ended: SystemTime,
    pub report: PassReport,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub started: SystemTime,
    pub phase: Phase,
    pub last_scan: Option<ScanDone>,
    pub last_pass: Option<PassDone>,
    /// (groups, items in them)
    pub duplicates: Option<(usize, usize)>,
    pub search_docs: usize,
}

/// The board the scan thread writes and the status page reads.
pub struct Status {
    inner: Mutex<Snapshot>,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            inner: Mutex::new(Snapshot {
                started: SystemTime::now(),
                phase: Phase::Starting,
                last_scan: None,
                last_pass: None,
                duplicates: None,
                search_docs: 0,
            }),
        }
    }
}

impl Status {
    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().expect("status lock").clone()
    }

    pub fn set_phase(&self, phase: Phase) {
        self.inner.lock().expect("status lock").phase = phase;
    }

    pub fn scan_done(&self, done: ScanDone) {
        self.inner.lock().expect("status lock").last_scan = Some(done);
    }

    pub fn pass_done(&self, report: PassReport) {
        self.inner.lock().expect("status lock").last_pass = Some(PassDone {
            ended: SystemTime::now(),
            report,
        });
    }

    pub fn set_duplicates(&self, groups: usize, items: usize) {
        self.inner.lock().expect("status lock").duplicates = Some((groups, items));
    }

    pub fn set_search_docs(&self, n: usize) {
        self.inner.lock().expect("status lock").search_docs = n;
    }
}

/// "3 min ago", "in 5 h", "just now".
pub fn relative(t: SystemTime, now: SystemTime) -> String {
    let (secs, past) = match now.duration_since(t) {
        Ok(d) => (d.as_secs(), true),
        Err(e) => (e.duration().as_secs(), false),
    };
    let amount = match secs {
        0..=59 => return "just now".into(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=172_799 => format!("{} h", secs / 3600),
        _ => format!("{} days", secs / 86_400),
    };
    if past {
        format!("{amount} ago")
    } else {
        format!("in {amount}")
    }
}

/// "1 h 5 min", "42 s".
pub fn span(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {} min", s / 3600, s / 60 % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_naturally() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(relative(now - Duration::from_secs(30), now), "just now");
        assert_eq!(relative(now - Duration::from_secs(180), now), "3 min ago");
        assert_eq!(relative(now + Duration::from_secs(5 * 3600), now), "in 5 h");
        assert_eq!(
            relative(now - Duration::from_secs(3 * 86_400), now),
            "3 days ago"
        );
        assert_eq!(span(Duration::from_secs(42)), "42 s");
        assert_eq!(span(Duration::from_secs(3900)), "1 h 5 min");
    }
}
