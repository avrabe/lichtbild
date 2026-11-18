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
use crate::video;

/// Where the browser-viewable preview for a content hash lives (HEIC).
pub fn preview_path(data_dir: &Path, hash: &str) -> PathBuf {
    data_dir
        .join("previews")
        .join(&hash[..2.min(hash.len())])
        .join(format!("{hash}.jpg"))
}

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
    /// The program that decodes video keyframes (`<exe> decode-keyframe`);
    /// this executable when unset. Tests name the lichtbild binary.
    pub keyframe_decoder: Option<PathBuf>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct ThumbReport {
    pub generated: u64,
    pub reused: u64,
    /// Indexed items without a thumbnail, by reason and format, e.g.
    /// "decode-error (heic)". Each is an empty tile in the timeline.
    pub failed: BTreeMap<String, u64>,
    /// Items with a JPEG preview (HEIC), present after this scan.
    pub previews: u64,
}

/// One line of the operator's problem list (scan-problems.tsv). It names
/// files, so it stays in the data directory and is never in the report.
fn problem_line(kind: &str, reason: &str, label: &str, rel: &Path) -> String {
    let clean = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
    };
    format!(
        "{kind}\t{}\t{}\t{}",
        clean(reason),
        clean(label),
        clean(&rel.to_string_lossy())
    )
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
    /// Indexed videos by codec: the denominator of the poster kill
    /// criterion (DD-016).
    pub videos_by_codec: BTreeMap<String, u64>,
    pub reconciled: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    pub version: String,
    pub complete: bool,
    pub reconciled: bool,
    pub priority: String,
    pub workers: usize,
    /// Sums over all roots.
    pub total_found: u64,
    pub total_indexed: u64,
    pub total_skipped: u64,
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
                "  thumbnails: generated {}, reused {}, previews {}",
                r.thumbnails.generated, r.thumbnails.reused, r.thumbnails.previews
            );
            for (k, v) in &r.thumbnails.failed {
                s += &format!(", {k} {v}");
            }
            s += "\n";
            if !r.videos_by_codec.is_empty() {
                let v: Vec<String> = r
                    .videos_by_codec
                    .iter()
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect();
                s += &format!("  videos: {}\n", v.join(", "));
            }
        }
        s += &format!(
            "total: found {}, indexed {}, skipped {}\n",
            self.total_found, self.total_indexed, self.total_skipped
        );
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
    format: media::Format,
    rel: PathBuf,
    size: u64,
    mtime_ns: i64,
}

enum Outcome {
    Indexed {
        info: ImageInfo,
        thumb: String,
        generated: bool,
        preview: bool,
    },
    Skipped(Skip),
}

struct Shared {
    root: PathBuf,
    data_dir: PathBuf,
    limits: Limits,
    cache_bytes: AtomicU64,
    decoder: PathBuf,
}

fn process(sh: &Shared, job: &Job) -> Outcome {
    if job.format.is_video() {
        return process_video(sh, job);
    }
    let bytes = match source::read_limited(&sh.root, &job.rel, sh.limits.max_file_bytes) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
            return Outcome::Skipped(Skip::OverLimit)
        }
        Err(_) => return Outcome::Skipped(Skip::Unreadable),
    };
    let info = match media::inspect(
        &bytes,
        job.format,
        &job.rel,
        job.mtime_ns,
        sh.limits.max_pixels,
    ) {
        Ok(i) => i,
        Err(s) => return Outcome::Skipped(s),
    };
    let (format, max_pixels) = (job.format, sh.limits.max_pixels);
    // One decode serves the thumbnail and, for HEIC, the preview.
    thumbnails(sh, info, format.needs_preview(), move || {
        media::decode(&bytes, format, max_pixels)
    })
}

/// A video: container and metadata from the `moov` index, the poster from
/// one keyframe; the file is never read whole except to hash it
/// (REQ-MEDIA-001). A container that cannot be read is skipped; a codec
/// that cannot be decoded still indexes the video, without a poster.
fn process_video(sh: &Shared, job: &Job) -> Outcome {
    let Ok(mut f) = source::open(&sh.root, &job.rel) else {
        return Outcome::Skipped(Skip::Unreadable);
    };
    let (info, track) = match video::inspect(&mut f, &job.rel, job.mtime_ns, sh.limits.max_pixels) {
        Ok(x) => x,
        Err(s) => return Outcome::Skipped(s),
    };
    let max_pixels = sh.limits.max_pixels;
    thumbnails(sh, info, true, move || {
        video::poster(&mut f, &track, max_pixels, &sh.decoder)
    })
}

/// Write the thumbnail and, when wanted, the preview for `info`, decoding
/// only if one of them is missing.
fn thumbnails(
    sh: &Shared,
    info: ImageInfo,
    wants_preview: bool,
    decode: impl FnOnce() -> Result<image::DynamicImage, Skip>,
) -> Outcome {
    let thumb_dest = thumb_path(&sh.data_dir, &info.hash);
    let preview_dest = preview_path(&sh.data_dir, &info.hash);
    let has_thumb = thumb_dest.is_file();
    let has_preview = !wants_preview || preview_dest.is_file();
    let indexed = |thumb: &str, generated: bool, preview: bool| Outcome::Indexed {
        info: info.clone(),
        thumb: thumb.into(),
        generated,
        preview,
    };
    if has_thumb && has_preview {
        return indexed("ok", false, wants_preview);
    }
    if sh.cache_bytes.load(Ordering::Relaxed) >= sh.limits.thumb_cache_bytes {
        return indexed("cache-full", false, false);
    }
    let img = match decode() {
        Ok(i) => i,
        Err(_) => return indexed("decode-error", false, false),
    };
    let mut written = 0u64;
    if !has_thumb {
        let Ok(t) = media::jpeg_fit(&img, sh.limits.thumb_size, 80) else {
            return indexed("decode-error", false, false);
        };
        if datadir::write_atomic(&thumb_dest, &t).is_err() {
            return indexed("write-error", false, false);
        }
        written += t.len() as u64;
    }
    if !has_preview {
        let Ok(p) = media::jpeg_fit(&img, media::PREVIEW_SIZE, 85) else {
            return indexed("decode-error", false, false);
        };
        if datadir::write_atomic(&preview_dest, &p).is_err() {
            return indexed("write-error", false, false);
        }
        written += p.len() as u64;
    }
    sh.cache_bytes.fetch_add(written, Ordering::Relaxed);
    indexed("ok", !has_thumb, wants_preview)
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
    let cache_bytes = datadir::sweep_and_measure(&cfg.data_dir.join("thumbs"))?
        + datadir::sweep_and_measure(&cfg.data_dir.join("previews"))?;
    let mut problems: Vec<String> = Vec::new();
    let cache = Arc::new(AtomicU64::new(cache_bytes));
    let mut budget = opts.stop_after;
    let decoder = match &opts.keyframe_decoder {
        Some(p) => p.clone(),
        None => std::env::current_exe()?,
    };

    for root in &cfg.roots {
        let known = idx.known_for_root(&root.label)?;
        let mut rr = RootReport {
            label: root.label.clone(),
            ..Default::default()
        };
        let label = root.label.clone();
        let skip = |rr: &mut RootReport, problems: &mut Vec<String>, s: Skip, rel: &Path| {
            let reason = s.to_string();
            problems.push(problem_line("skipped", &reason, &label, rel));
            rr.skipped += 1;
            *rr.skipped_by_reason.entry(reason).or_default() += 1;
        };
        let thumb_failed = |rr: &mut RootReport,
                            problems: &mut Vec<String>,
                            thumb: &str,
                            kind: &str,
                            rel: &Path| {
            // Images by format, videos by codec (the kill criterion for
            // posters counts by codec).
            let reason = format!("{thumb} ({kind})");
            problems.push(problem_line("thumbnail", &reason, &label, rel));
            *rr.thumbnails.failed.entry(reason).or_default() += 1;
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
                    let unreadable = reason == Skip::UnreadableDir;
                    skip(&mut rr, &mut problems, reason, &rel);
                    if unreadable {
                        unreadable_dirs.push(rel);
                    }
                }
                Entry::File {
                    rel,
                    size,
                    mtime_ns,
                } => match media::classify(&rel) {
                    Class::Skip(s) => skip(&mut rr, &mut problems, s, &rel),
                    Class::Media(format) => {
                        let k = known.get(index::rel_bytes(&rel));
                        // Unchanged: same size and time, and the thumbnail is
                        // either present or failed for good (decode error).
                        // A cache-full or write-error thumbnail is retried.
                        let unchanged = k.is_some_and(|k| {
                            k.size == size
                                && k.mtime_ns == mtime_ns
                                && match k.thumb.as_str() {
                                    "ok" => {
                                        thumb_path(&cfg.data_dir, &k.hash).is_file()
                                            && (!format.needs_preview()
                                                || preview_path(&cfg.data_dir, &k.hash).is_file())
                                    }
                                    "decode-error" => true,
                                    _ => false,
                                }
                        });
                        if let (true, Some(k)) = (unchanged, k) {
                            if k.missing {
                                let b = idx.batch()?;
                                b.set_missing(k.id, false)?;
                                b.commit()?;
                            }
                            rr.indexed += 1;
                            rr.unchanged += 1;
                            if let Some(c) = &k.codec {
                                *rr.videos_by_codec.entry(c.clone()).or_default() += 1;
                            }
                            if k.thumb == "ok" {
                                rr.thumbnails.reused += 1;
                                if format.needs_preview() {
                                    rr.thumbnails.previews += 1;
                                }
                            } else {
                                let kind = k.codec.as_deref().unwrap_or(format.name());
                                thumb_failed(&mut rr, &mut problems, &k.thumb, kind, &rel);
                            }
                            indexed_rels.insert(index::rel_bytes(&rel).to_vec());
                        } else {
                            jobs.push(Job {
                                known_id: k.map(|k| k.id),
                                format,
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
            decoder: decoder.clone(),
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
                        .unwrap_or_else(|_| {
                            Outcome::Skipped(Skip::DecodeError(job.format.name().into()))
                        });
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
                Outcome::Skipped(s) => skip(&mut rr, &mut problems, s, &job.rel),
                Outcome::Indexed {
                    info,
                    thumb,
                    generated,
                    preview,
                } => {
                    tx_open.upsert(
                        &root.label,
                        &job.rel,
                        job.size,
                        job.mtime_ns,
                        &info,
                        &thumb,
                        preview,
                    )?;
                    rr.indexed += 1;
                    if let Some(v) = &info.video {
                        *rr.videos_by_codec.entry(v.codec.clone()).or_default() += 1;
                    }
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
                        let kind = info
                            .video
                            .as_ref()
                            .map_or(job.format.name(), |v| v.codec.as_str());
                        thumb_failed(&mut rr, &mut problems, &thumb, kind, &job.rel);
                    }
                    if preview {
                        rr.thumbnails.previews += 1;
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
    for r in &report.roots {
        report.total_found += r.found;
        report.total_indexed += r.indexed;
        report.total_skipped += r.skipped;
    }
    if !report.complete {
        report.reconciled = false;
        report
            .problems
            .push("scan interrupted before the end".into());
    }
    datadir::write_report(&cfg.data_dir, &serde_json::to_string_pretty(&report)?)?;
    let mut tsv = String::from("kind\treason\troot\tpath\n");
    for line in &problems {
        tsv += line;
        tsv.push('\n');
    }
    datadir::write_problems(&cfg.data_dir, &tsv)?;
    Ok(report)
}
