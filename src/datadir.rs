//! The only module that creates, writes, renames or removes files (DD-006).
//! Every function here takes a path the caller derived from the data
//! directory; `config` has already refused a data directory that overlaps a
//! source root (SC-001).
#![allow(clippy::disallowed_methods)] // the one place writes are allowed (DD-006)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `dest` through a temporary file in the same directory
/// and a rename, so `dest` is either absent or complete (SC-003).
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    let dir = dest.parent().context("destination has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp: PathBuf = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Remove temporary files a killed run left behind, and return the total
/// size of the complete files under `dir`.
pub fn sweep_and_measure(dir: &Path) -> Result<u64> {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for e in rd {
            let e = e?;
            let ft = e.file_type()?;
            if ft.is_dir() {
                stack.push(e.path());
            } else if e.file_name().to_string_lossy().starts_with(".tmp-") {
                std::fs::remove_file(e.path())?;
            } else if ft.is_file() {
                total += e.metadata()?.len();
            }
        }
    }
    Ok(total)
}

/// Write the scan report next to the index.
pub fn write_report(data_dir: &Path, json: &str) -> Result<()> {
    write_atomic(&data_dir.join("scan-report.json"), json.as_bytes())
}

/// The operator's list of skipped files and missing thumbnails, with paths.
/// Private: it stays in the data directory, unlike the shareable report.
pub fn write_problems(data_dir: &Path, tsv: &str) -> Result<()> {
    write_atomic(&data_dir.join("scan-problems.tsv"), tsv.as_bytes())
}
