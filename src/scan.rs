//! The read-only scan: walk each root, index images, generate thumbnails,
//! and report found / indexed / skipped so they reconcile (SC-002).
//! Idempotent and resumable (SC-003), bounded and low priority (SC-005).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::Result;
use serde::Serialize;

use crate::config::{Config, Limits};
use crate::datadir;
use crate::index::{self, Index};
use crate::media::{self, Class, ImageInfo};
use crate::source::{self, Entry, Skip};

/// Where the thumbnail for a content hash lives.
pub fn thumb_path(data_dir: &Path, hash: &str) -> PathBuf {
    data_dir
        .join("thumbs")
        .join(&hash[..2.min(hash.len())])
        .join(format!("{hash}.jpg"))
}

#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Stop after this many files were processed, as if the process had been
    /// killed (tests of SC-003).
    pub stop_after: Option<usize>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct ThumbReport {
    pub generated: u64,
    pub reused: u64,
    pub failed: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct RootReport {
    pub label: String,
    pub found: u64,
    pub indexed: u64,
    pub skipped: u64,
    pub skipped_by_reason: BTreeMap<String, u64>,
    pub new: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub marked_missing: u64,
    pub live_in_index: u64,
    pub thumbnails: ThumbReport,
    pub reconciled: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    pub version: String,
    pub complete: bool,
    pub reconciled: bool,
    pub priority: String,
    pub workers: usize,
    pub roots: Vec<RootReport>,
    pub problems: Vec<String>,
}

impl Report {
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for r in &self.roots {
            s += &format!(
                "root {}: found {}, indexed {} (new {}, updated {}, unchanged {}), skipped {}, marked missing {}, live in index {}{}\n",
                r.label, r.found, r.indexed, r.new, r.updated, r.unchanged, r.skipped,
                r.marked_missing, r.live_in_index,
                if r.reconciled { "" } else { "  NOT RECONCILED" }
            );
            for (k, v) in &r.skipped_by_reason {
                s += &format!("  skipped {v:>7}  {k}\n");
            }
            s += &format!(
                "  thumbnails: generated {}, reused {}",
                r.thumbnails.generated, r.thumbnails.reused
            );
            for (k, v) in &r.thumbnails.failed {
                s += &format!(", {k} {v}");
            }
            s += "\n";
        }
        for p in &self.problems {
            s += &format!("problem: {p}\n");
        }
        s += &format!(
            "scan {}, {} (workers {}, {})\n",
            if self.complete {
                "complete"
            } else {
                "INTERRUPTED"
            },
            if self.reconciled {
                "reconciled"
            } else {
                "NOT RECONCILED"
            },
            self.workers,
            self.priority
        );
        s
    }
}

/// Lower this process's CPU and I/O priority before any worker starts, so
/// the workers inherit it (SC-005).
pub fn lower_priority() -> String {
    // SAFETY: plain libc calls on the current process with constant arguments.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 19) };
    // Report what the kernel says, not what was asked for.
    // SAFETY: getpriority(2) on the current process.
    let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    #[cfg(target_os = "linux")]
    let io = {
        const IOPRIO_WHO_PROCESS: libc::c_long = 1;
        const IOPRIO_CLASS_IDLE: libc::c_long = 3;
        // SAFETY: ioprio_set(2) with constant arguments for the current process.
        let r = unsafe {
            libc::syscall(
                libc::SYS_ioprio_set,
                IOPRIO_WHO_PROCESS,
                0 as libc::c_long,
                IOPRIO_CLASS_IDLE << 13,
            )
        };
        if r == 0 {
            "io idle"
        } else {
            "io priority unchanged"
        }
    };
    #[cfg(not(target_os = "linux"))]
    let io = "io priority unchanged (not linux)";
    format!("nice {nice}, {io}")
}

struct Job {
    known_id: Option<u64>,
    rel: PathBuf,
    size: u64,
    mtime_ns: i64,
}

enum Outcome {
    Indexed {
        info: ImageInfo,
        thumb: String,
        generated: bool,
    },
    Skipped(Skip),
}

struct Shared {
    root: PathBuf,
    data_dir: PathBuf,
    limits: Limits,
    cache_bytes: AtomicU64,
}

fn process(sh: &Shared, job: &Job) -> Outcome {
    let bytes = match source::read_limited(&sh.root, &job.rel, sh.limits.max_file_bytes) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
            return Outcome::Skipped(Skip::OverLimit)
        }
        Err(_) => return Outcome::Skipped(Skip::Unreadable),
    };
    let info = match media::inspect(&bytes, &job.rel, job.mtime_ns, sh.limits.max_pixels) {
        Ok(i) => i,
        Err(s) => return Outcome::Skipped(s),
    };
    let dest = thumb_path(&sh.data_dir, &info.hash);
    if dest.is_file() {
        return Outcome::Indexed {
            info,
            thumb: "ok".into(),
            generated: false,
        };
    }
    if sh.cache_bytes.load(Ordering::Relaxed) >= sh.limits.thumb_cache_bytes {
        return Outcome::Indexed {
            info,
            thumb: "cache-full".into(),
            generated: false,
        };
    }
    let thumb = match media::thumbnail(&bytes, sh.limits.thumb_size, sh.limits.max_pixels) {
        Ok(t) => t,
        Err(_) => {
            return Outcome::Indexed {
                info,
                thumb: "decode-error".into(),
                generated: false,
            }
        }
    };
    match datadir::write_atomic(&dest, &thumb) {
        Ok(()) => {
            sh.cache_bytes
                .fetch_add(thumb.len() as u64, Ordering::Relaxed);
            Outcome::Indexed {
                info,
                thumb: "ok".into(),
                generated: true,
            }
        }
        Err(_) => Outcome::Indexed {
            info,
            thumb: "write-error".into(),
            generated: false,
        },
    }
}

/// Scan every root into `idx`. The caller owns the index: the server passes
/// its own, the `scan` command opens it (redb allows one process).
pub fn run(cfg: &Config, idx: &Index, opts: &Options) -> Result<Report> {
    let mut report = Report {
        version: crate::VERSION.into(),
        complete: true,
        reconciled: true,
        priority: lower_priority(),
        workers: cfg.workers,
        ..Default::default()
    };
    let cache_bytes = datadir::sweep_and_measure(&cfg.data_dir.join("thumbs"))?;
    let cache = Arc::new(AtomicU64::new(cache_bytes));
    let mut budget = opts.stop_after;

    for root in &cfg.roots {
        let known = idx.known_for_root(&root.label)?;
        let mut rr = RootReport {
            label: root.label.clone(),
            ..Default::default()
        };
        let skip = |rr: &mut RootReport, s: Skip| {
            rr.skipped += 1;
            *rr.skipped_by_reason.entry(s.to_string()).or_default() += 1;
        };

        // 1. Walk and classify; collect the files that need work.
        let mut unreadable_dirs: Vec<PathBuf> = Vec::new();
        let mut indexed_rels: HashSet<Vec<u8>> = HashSet::new();
        let mut jobs: Vec<Job> = Vec::new();
        let mut entries = Vec::new();
        source::walk(&root.path, |e| entries.push(e));
        for e in entries {
            rr.found += 1;
            match e {
                Entry::Skipped { rel, reason } => {
                    if reason == Skip::UnreadableDir {
                        unreadable_dirs.push(rel);
                    }
                    skip(&mut rr, reason);
                }
                Entry::File {
                    rel,
                    size,
                    mtime_ns,
                } => match media::classify(&rel) {
                    Class::Skip(s) => skip(&mut rr, s),
                    Class::Image => {
                        let k = known.get(index::rel_bytes(&rel));
                        let unchanged = k.is_some_and(|k| {
                            k.size == size
                                && k.mtime_ns == mtime_ns
                                && (k.thumb != "ok" || thumb_path(&cfg.data_dir, &k.hash).is_file())
                        });
                        if let (true, Some(k)) = (unchanged, k) {
                            if k.missing {
                                let b = idx.batch()?;
                                b.set_missing(k.id, false)?;
                                b.commit()?;
                            }
                            rr.indexed += 1;
                            rr.unchanged += 1;
                            indexed_rels.insert(index::rel_bytes(&rel).to_vec());
                        } else {
                            jobs.push(Job {
                                known_id: k.map(|k| k.id),
                                rel,
                                size,
                                mtime_ns,
                            });
                        }
                    }
                },
            }
        }

        // 2. Process the jobs with bounded workers; write from this thread.
        let take = budget.map_or(jobs.len(), |b| b.min(jobs.len()));
        let interrupted = take < jobs.len();
        jobs.truncate(take);
        if let Some(b) = budget.as_mut() {
            *b -= take;
        }
        let shared = Arc::new(Shared {
            root: root.path.clone(),
            data_dir: cfg.data_dir.clone(),
            limits: cfg.limits.clone(),
            cache_bytes: AtomicU64::new(cache.load(Ordering::Relaxed)),
        });
        let queue = Arc::new(Mutex::new(jobs.into_iter()));
        let (tx, rx) = mpsc::channel::<(Job, Outcome)>();
        let mut handles = Vec::new();
        for _ in 0..cfg.workers {
            let (q, tx, sh) = (queue.clone(), tx.clone(), shared.clone());
            handles.push(std::thread::spawn(move || loop {
                let job = match q.lock().map(|mut it| it.next()) {
                    Ok(Some(j)) => j,
                    _ => break,
                };
                // A panic in a decoder must not lose the file or the worker.
                let out =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(&sh, &job)))
                        .unwrap_or(Outcome::Skipped(Skip::DecodeError));
                if tx.send((job, out)).is_err() {
                    break;
                }
            }));
        }
        drop(tx);
        let mut pending = 0;
        let mut tx_open = idx.batch()?;
        for (job, out) in rx {
            match out {
                Outcome::Skipped(s) => skip(&mut rr, s),
                Outcome::Indexed {
                    info,
                    thumb,
                    generated,
                } => {
                    tx_open.upsert(&root.label, &job.rel, job.size, job.mtime_ns, &info, &thumb)?;
                    rr.indexed += 1;
                    if job.known_id.is_some() {
                        rr.updated += 1;
                    } else {
                        rr.new += 1;
                    }
                    if generated {
                        rr.thumbnails.generated += 1;
                    } else if thumb == "ok" {
                        rr.thumbnails.reused += 1;
                    } else {
                        *rr.thumbnails.failed.entry(thumb).or_default() += 1;
                    }
                    indexed_rels.insert(index::rel_bytes(&job.rel).to_vec());
                }
            }
            pending += 1;
            if pending >= 100 {
                tx_open.commit()?;
                tx_open = idx.batch()?;
                pending = 0;
            }
        }
        tx_open.commit()?;
        for h in handles {
            let _ = h.join();
        }
        cache.store(
            shared.cache_bytes.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );

        if interrupted {
            report.complete = false;
            report.roots.push(rr);
            break;
        }

        // 3. Files indexed before and not indexed now become missing, unless
        //    they sit under a directory that could not be read this time.
        let mut preserved = 0;
        let tx_m = idx.batch()?;
        for (rel, k) in &known {
            if indexed_rels.contains(rel) {
                continue;
            }
            let p = index::rel_from_bytes(rel);
            if unreadable_dirs.iter().any(|d| p.starts_with(d)) {
                if !k.missing {
                    preserved += 1;
                }
                continue;
            }
            if !k.missing {
                tx_m.set_missing(k.id, true)?;
                rr.marked_missing += 1;
            }
        }
        tx_m.commit()?;

        // 4. Reconcile: counted at walk time, and against the index itself.
        rr.live_in_index = idx.live_count(&root.label)?;
        let walk_ok = rr.found == rr.indexed + rr.skipped;
        let index_ok = rr.live_in_index == rr.indexed + preserved;
        rr.reconciled = walk_ok && index_ok;
        if !walk_ok {
            report.problems.push(format!(
                "root {}: found {} != indexed {} + skipped {}",
                rr.label, rr.found, rr.indexed, rr.skipped
            ));
        }
        if !index_ok {
            report.problems.push(format!(
                "root {}: index holds {} live items, expected {}",
                rr.label,
                rr.live_in_index,
                rr.indexed + preserved
            ));
        }
        report.reconciled &= rr.reconciled;
        report.roots.push(rr);
    }
    if !report.complete {
        report.reconciled = false;
        report
            .problems
            .push("scan interrupted before the end".into());
    }
    datadir::write_report(&cfg.data_dir, &serde_json::to_string_pretty(&report)?)?;
    Ok(report)
}
