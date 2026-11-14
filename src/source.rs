//! All access to source roots (DD-006). This module lists directories,
//! reads metadata and opens files for reading. It has no write call, and
//! clippy.toml makes adding one a lint error (SC-001).

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Why an entry under a root was not indexed. Every non-directory entry the
/// walk meets ends up either indexed or as one of these (SC-002).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Skip {
    /// A directory that could not be listed; its files are not counted.
    UnreadableDir,
    /// A file whose metadata or content could not be read.
    Unreadable,
    /// A symlink whose target lies outside its root (SSC-004).
    SymlinkOutsideRoot,
    /// A symlink to a directory inside the root (not followed, avoids loops).
    SymlinkToDir,
    /// A broken symlink.
    BrokenSymlink,
    /// A socket, fifo or device.
    NotRegularFile,
    /// Hidden or system file (".DS_Store", "._x", "Thumbs.db", ...).
    HiddenOrSystem,
    /// A format lichtbild does not index yet (HEIC, video, raw).
    NotYetSupported(String),
    /// A format lichtbild does not index.
    Unsupported(String),
    /// Bigger than the configured file or pixel limit.
    OverLimit,
    /// Header or pixels could not be decoded; names the format.
    DecodeError(String),
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Skip::UnreadableDir => write!(f, "unreadable directory (contents not counted)"),
            Skip::Unreadable => write!(f, "unreadable"),
            Skip::SymlinkOutsideRoot => write!(f, "symlink outside root"),
            Skip::SymlinkToDir => write!(f, "symlink to directory"),
            Skip::BrokenSymlink => write!(f, "broken symlink"),
            Skip::NotRegularFile => write!(f, "not a regular file"),
            Skip::HiddenOrSystem => write!(f, "hidden or system file"),
            Skip::NotYetSupported(k) => write!(f, "not yet supported ({k})"),
            Skip::Unsupported(ext) => write!(f, "unsupported type (.{ext})"),
            Skip::OverLimit => write!(f, "over size limit"),
            Skip::DecodeError(k) => write!(f, "decode error ({k})"),
        }
    }
}

/// A non-directory entry met by the walk.
#[derive(Debug, Clone)]
pub enum Entry {
    File {
        rel: PathBuf,
        size: u64,
        mtime_ns: i64,
    },
    Skipped {
        rel: PathBuf,
        reason: Skip,
    },
}

/// Walk `root` (canonical) depth-first and call `f` once per non-directory
/// entry, and once per directory that could not be listed.
pub fn walk(root: &Path, mut f: impl FnMut(Entry)) {
    let mut stack = vec![PathBuf::new()];
    while let Some(rel_dir) = stack.pop() {
        let abs_dir = root.join(&rel_dir);
        let rd = match std::fs::read_dir(&abs_dir) {
            Ok(rd) => rd,
            Err(_) => {
                f(Entry::Skipped {
                    rel: rel_dir,
                    reason: Skip::UnreadableDir,
                });
                continue;
            }
        };
        let mut entries: Vec<_> = rd.collect();
        // Stable order makes reports and resumed runs reproducible.
        entries.sort_by_key(|e| e.as_ref().map(|e| e.file_name()).ok());
        for e in entries {
            let e = match e {
                Ok(e) => e,
                Err(_) => {
                    f(Entry::Skipped {
                        rel: rel_dir.clone(),
                        reason: Skip::UnreadableDir,
                    });
                    continue;
                }
            };
            let rel = rel_dir.join(e.file_name());
            let abs = e.path();
            let meta = match std::fs::symlink_metadata(&abs) {
                Ok(m) => m,
                Err(_) => {
                    f(Entry::Skipped {
                        rel,
                        reason: Skip::Unreadable,
                    });
                    continue;
                }
            };
            let ft = meta.file_type();
            if ft.is_dir() {
                stack.push(rel);
            } else if ft.is_symlink() {
                f(classify_symlink(root, &abs, rel));
            } else if ft.is_file() {
                f(file_entry(rel, &meta));
            } else {
                f(Entry::Skipped {
                    rel,
                    reason: Skip::NotRegularFile,
                });
            }
        }
    }
}

fn classify_symlink(root: &Path, abs: &Path, rel: PathBuf) -> Entry {
    let target = match abs.canonicalize() {
        Ok(t) => t,
        Err(_) => {
            return Entry::Skipped {
                rel,
                reason: Skip::BrokenSymlink,
            }
        }
    };
    if !target.starts_with(root) {
        return Entry::Skipped {
            rel,
            reason: Skip::SymlinkOutsideRoot,
        };
    }
    match std::fs::metadata(&target) {
        Ok(m) if m.is_dir() => Entry::Skipped {
            rel,
            reason: Skip::SymlinkToDir,
        },
        Ok(m) if m.is_file() => file_entry(rel, &m),
        Ok(_) => Entry::Skipped {
            rel,
            reason: Skip::NotRegularFile,
        },
        Err(_) => Entry::Skipped {
            rel,
            reason: Skip::Unreadable,
        },
    }
}

fn file_entry(rel: PathBuf, meta: &std::fs::Metadata) -> Entry {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Entry::File {
        rel,
        size: meta.len(),
        mtime_ns,
    }
}

/// Resolve `rel` under `root` and refuse anything that resolves outside it
/// (SSC-004). `root` must be canonical.
pub fn resolve(root: &Path, rel: &Path) -> io::Result<PathBuf> {
    let p = root.join(rel).canonicalize()?;
    if p.starts_with(root) && p != root {
        Ok(p)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path resolves outside its root",
        ))
    }
}

/// Open a file under a root for reading (read-only by construction).
pub fn open(root: &Path, rel: &Path) -> io::Result<File> {
    File::open(resolve(root, rel)?)
}

/// Read a whole file under a root, refusing files over `max` bytes.
pub fn read_limited(root: &Path, rel: &Path, max: u64) -> io::Result<Vec<u8>> {
    let f = open(root, rel)?;
    let len = f.metadata()?.len();
    if len > max {
        return Err(io::Error::new(io::ErrorKind::FileTooLarge, "over limit"));
    }
    let mut buf = Vec::with_capacity(len as usize);
    f.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::FileTooLarge, "over limit"));
    }
    Ok(buf)
}
