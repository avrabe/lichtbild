//! The index: one redb database in the data directory (REQ-IDX-001, DD-004).
//!
//! redb holds an exclusive lock on its file, so one process owns the index:
//! `lichtbild serve` scans in its own threads, and `lichtbild scan` refuses
//! to run while a server holds the index.
//!
//! Tables:
//! - `meta`:    name -> u64 (schema version, next item id)
//! - `items`:   id -> JSON record (every item, live or missing)
//! - `paths`:   (label, root-relative path bytes) -> id
//! - `by_root`: (label, taken_local, id) -> ()   live items only
//! - `by_time`: (taken_local, id) -> label        live items only

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::media::ImageInfo;

const SCHEMA_VERSION: u64 = 1;

const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const ITEMS: TableDefinition<u64, &[u8]> = TableDefinition::new("items");
const PATHS: TableDefinition<(&str, &[u8]), u64> = TableDefinition::new("paths");
const BY_ROOT: TableDefinition<(&str, &str, u64), ()> = TableDefinition::new("by_root");
const BY_TIME: TableDefinition<(&str, u64), &str> = TableDefinition::new("by_time");
/// Content hash -> transcript JSON (crate::stt::Transcript), so a moved or
/// renamed video keeps its transcript (REQ-MEDIA-004).
const TRANSCRIPTS: TableDefinition<&str, &[u8]> = TableDefinition::new("transcripts");

/// Sorts after every timestamp string ("YYYY-...").
const TAKEN_MAX: &str = "\u{7f}";

pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("index.redb")
}

pub fn rel_bytes(rel: &Path) -> &[u8] {
    rel.as_os_str().as_bytes()
}

pub fn rel_from_bytes(b: &[u8]) -> PathBuf {
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

/// One stored item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub label: String,
    pub rel: Vec<u8>,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: String,
    pub width: u32,
    pub height: u32,
    pub taken_local: String,
    pub taken_offset: Option<String>,
    pub date_source: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub thumb: String,
    pub missing: bool,
    /// A JPEG preview exists for an original the browser cannot show (HEIC).
    #[serde(default)]
    pub preview: bool,
    /// Length and codec, for a video (REQ-MEDIA-001).
    #[serde(default)]
    pub video: Option<crate::video::VideoMeta>,
}

/// What the index knows about one file, for change detection.
#[derive(Debug, Clone)]
pub struct Known {
    pub id: u64,
    pub size: u64,
    pub mtime_ns: i64,
    pub missing: bool,
    pub hash: String,
    pub thumb: String,
    /// The video codec, which names a failed poster in the report.
    pub codec: Option<String>,
}

/// One timeline entry.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: u64,
    pub label: String,
    pub taken_local: String,
    pub date_source: String,
    pub thumb_ok: bool,
    /// Content hash; part of the thumbnail URL so a replaced file gets a
    /// new URL instead of a cached old thumbnail.
    pub hash: String,
    pub width: u32,
    pub height: u32,
    /// Length of a video, in milliseconds.
    pub video_ms: Option<u64>,
}

pub struct Index {
    db: Database,
}

fn decode(b: &[u8]) -> Result<Record> {
    serde_json::from_slice(b).context("corrupt item record")
}

impl Index {
    /// Open or create the index and check its schema version.
    pub fn open(data_dir: &Path) -> Result<Index> {
        let db = match Database::create(db_path(data_dir)) {
            Ok(db) => db,
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => bail!(
                "the index is in use by another lichtbild process (a running `lichtbild serve` scans by itself)"
            ),
            Err(e) => return Err(e).context("cannot open the index"),
        };
        let w = db.begin_write()?;
        {
            let mut meta = w.open_table(META)?;
            let v = meta.get("schema_version")?.map(|g| g.value());
            match v {
                None => {
                    meta.insert("schema_version", SCHEMA_VERSION)?;
                    meta.insert("next_id", 1)?;
                }
                Some(v) if v > SCHEMA_VERSION => {
                    bail!("index schema {v} is newer than this lichtbild ({SCHEMA_VERSION})")
                }
                Some(_) => {}
            }
            w.open_table(ITEMS)?;
            w.open_table(PATHS)?;
            w.open_table(BY_ROOT)?;
            w.open_table(BY_TIME)?;
            w.open_table(TRANSCRIPTS)?;
        }
        w.commit()?;
        Ok(Index { db })
    }

    pub fn known_for_root(&self, label: &str) -> Result<HashMap<Vec<u8>, Known>> {
        let r = self.db.begin_read()?;
        let paths = r.open_table(PATHS)?;
        let items = r.open_table(ITEMS)?;
        let mut out = HashMap::new();
        for e in paths.range((label, &[][..])..)? {
            let (k, v) = e?;
            let (l, rel) = k.value();
            if l != label {
                break;
            }
            let id = v.value();
            let Some(raw) = items.get(id)? else { continue };
            let rec = decode(raw.value())?;
            out.insert(
                rel.to_vec(),
                Known {
                    id,
                    size: rec.size,
                    mtime_ns: rec.mtime_ns,
                    missing: rec.missing,
                    hash: rec.hash,
                    thumb: rec.thumb,
                    codec: rec.video.map(|v| v.codec),
                },
            );
        }
        Ok(out)
    }

    pub fn batch(&self) -> Result<Batch> {
        Ok(Batch {
            w: self.db.begin_write()?,
        })
    }

    pub fn live_count(&self, label: &str) -> Result<u64> {
        let r = self.db.begin_read()?;
        let t = r.open_table(BY_ROOT)?;
        let mut n = 0;
        for e in t.range((label, "", 0)..(label, TAKEN_MAX, 0))? {
            e?;
            n += 1;
        }
        Ok(n)
    }

    /// Live item count per label.
    pub fn counts(&self) -> Result<HashMap<String, u64>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(BY_ROOT)?;
        let mut out: HashMap<String, u64> = HashMap::new();
        for e in t.iter()? {
            let (k, _) = e?;
            *out.entry(k.value().0.to_string()).or_default() += 1;
        }
        Ok(out)
    }

    /// Live items per month ("YYYY-MM"), newest first, for the timeline's
    /// date scrubber. One label reads `by_root`; several read `by_time`.
    pub fn months(&self, labels: &[String]) -> Result<Vec<(String, u64)>> {
        let r = self.db.begin_read()?;
        let mut out: Vec<(String, u64)> = Vec::new();
        let mut add = |taken: &str| {
            let ym = taken.get(..7).unwrap_or(taken);
            match out.last_mut() {
                Some((m, n)) if m == ym => *n += 1,
                _ => out.push((ym.to_string(), 1)),
            }
        };
        match labels {
            [] => {}
            [one] => {
                let t = r.open_table(BY_ROOT)?;
                for e in t
                    .range((one.as_str(), "", 0)..(one.as_str(), TAKEN_MAX, 0))?
                    .rev()
                {
                    add(e?.0.value().1);
                }
            }
            many => {
                let t = r.open_table(BY_TIME)?;
                for e in t.iter()?.rev() {
                    let (k, v) = e?;
                    if many.iter().any(|l| l == v.value()) {
                        add(k.value().0);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The neighbours of item `id` (taken at `taken`) in the timeline of
    /// `labels`: (newer, older). Same order as `timeline`.
    pub fn neighbours(
        &self,
        labels: &[String],
        taken: &str,
        id: u64,
    ) -> Result<(Option<u64>, Option<u64>)> {
        let r = self.db.begin_read()?;
        match labels {
            [] => Ok((None, None)),
            [one] => {
                let t = r.open_table(BY_ROOT)?;
                let l = one.as_str();
                let newer = match t.range((l, taken, id + 1)..(l, TAKEN_MAX, 0))?.next() {
                    Some(e) => Some(e?.0.value().2),
                    None => None,
                };
                let older = match t.range((l, "", 0)..(l, taken, id))?.next_back() {
                    Some(e) => Some(e?.0.value().2),
                    None => None,
                };
                Ok((newer, older))
            }
            many => {
                let t = r.open_table(BY_TIME)?;
                let mut newer = None;
                for e in t.range((taken, id + 1)..(TAKEN_MAX, 0))? {
                    let (k, v) = e?;
                    if many.iter().any(|x| x == v.value()) {
                        newer = Some(k.value().1);
                        break;
                    }
                }
                let mut older = None;
                for e in t.range(("", 0)..(taken, id))?.rev() {
                    let (k, v) = e?;
                    if many.iter().any(|x| x == v.value()) {
                        older = Some(k.value().1);
                        break;
                    }
                }
                Ok((newer, older))
            }
        }
    }

    /// Keyset page, newest first. One label reads `by_root`; several read
    /// `by_time` and keep only those labels. `before` is (taken_local, id).
    pub fn timeline(
        &self,
        labels: &[String],
        before: Option<(&str, u64)>,
        limit: usize,
    ) -> Result<Vec<Item>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let mut ids = Vec::new();
        match labels {
            [] => {}
            [one] => {
                let t = r.open_table(BY_ROOT)?;
                let (bt, bid) = before.unwrap_or((TAKEN_MAX, 0));
                for e in t
                    .range((one.as_str(), "", 0)..(one.as_str(), bt, bid))?
                    .rev()
                {
                    ids.push(e?.0.value().2);
                    if ids.len() >= limit {
                        break;
                    }
                }
            }
            many => {
                let t = r.open_table(BY_TIME)?;
                let (bt, bid) = before.unwrap_or((TAKEN_MAX, 0));
                for e in t.range(("", 0)..(bt, bid))?.rev() {
                    let (k, v) = e?;
                    if many.iter().any(|l| l == v.value()) {
                        ids.push(k.value().1);
                        if ids.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(raw) = items.get(id)? else { continue };
            let rec = decode(raw.value())?;
            out.push(Item {
                id,
                label: rec.label,
                taken_local: rec.taken_local,
                date_source: rec.date_source,
                thumb_ok: rec.thumb == "ok",
                hash: rec.hash,
                width: rec.width,
                height: rec.height,
                video_ms: rec.video.map(|v| v.duration_ms),
            });
        }
        Ok(out)
    }

    /// For serving files: the live item's record.
    pub fn locate(&self, id: u64) -> Result<Option<Record>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let Some(raw) = items.get(id)? else {
            return Ok(None);
        };
        let rec = decode(raw.value())?;
        Ok((!rec.missing).then_some(rec))
    }

    /// Every record with its id, for tests and diagnostics.
    pub fn all(&self) -> Result<Vec<(u64, Record)>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let mut out = Vec::with_capacity(items.len()? as usize);
        for e in items.iter()? {
            let (k, v) = e?;
            out.push((k.value(), decode(v.value())?));
        }
        Ok(out)
    }

    /// The stored transcript for a content hash.
    pub fn transcript(&self, hash: &str) -> Result<Option<crate::stt::Transcript>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(TRANSCRIPTS)?;
        match t.get(hash)? {
            Some(v) => Ok(Some(
                serde_json::from_slice(v.value()).context("corrupt transcript")?,
            )),
            None => Ok(None),
        }
    }

    pub fn set_transcript(&self, hash: &str, tr: &crate::stt::Transcript) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(TRANSCRIPTS)?;
            t.insert(hash, serde_json::to_vec(tr)?.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Live videos whose content has no transcript from `model` yet,
    /// oldest id first: (id, record).
    pub fn videos_to_transcribe(&self, model: &str) -> Result<Vec<(u64, Record)>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let t = r.open_table(TRANSCRIPTS)?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for e in items.iter()? {
            let (k, v) = e?;
            let rec = decode(v.value())?;
            if rec.missing || rec.video.is_none() || !seen.insert(rec.hash.clone()) {
                continue;
            }
            let done = match t.get(rec.hash.as_str())? {
                Some(v) => serde_json::from_slice::<crate::stt::Transcript>(v.value())
                    .is_ok_and(|tr| tr.model == model),
                None => false,
            };
            if !done {
                out.push((k.value(), rec));
            }
        }
        Ok(out)
    }

    /// Overwrite a record's stored path (tests of path containment only).
    #[doc(hidden)]
    pub fn tamper_rel_for_test(&self, id: u64, rel: &[u8]) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut items = w.open_table(ITEMS)?;
            let mut rec = decode(items.get(id)?.context("no such item")?.value())?;
            rec.rel = rel.to_vec();
            items.insert(id, serde_json::to_vec(&rec)?.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }
}

/// One write transaction; nothing is visible until `commit`.
pub struct Batch {
    w: redb::WriteTransaction,
}

impl Batch {
    fn load(&self, id: u64) -> Result<Option<Record>> {
        let items = self.w.open_table(ITEMS)?;
        let rec = items.get(id)?.map(|raw| decode(raw.value())).transpose()?;
        Ok(rec)
    }

    fn unlink(&self, id: u64, rec: &Record) -> Result<()> {
        self.w
            .open_table(BY_ROOT)?
            .remove((rec.label.as_str(), rec.taken_local.as_str(), id))?;
        self.w
            .open_table(BY_TIME)?
            .remove((rec.taken_local.as_str(), id))?;
        Ok(())
    }

    fn link(&self, id: u64, rec: &Record) -> Result<()> {
        self.w
            .open_table(BY_ROOT)?
            .insert((rec.label.as_str(), rec.taken_local.as_str(), id), ())?;
        self.w
            .open_table(BY_TIME)?
            .insert((rec.taken_local.as_str(), id), rec.label.as_str())?;
        Ok(())
    }

    fn store(&self, id: u64, rec: &Record) -> Result<()> {
        self.w
            .open_table(ITEMS)?
            .insert(id, serde_json::to_vec(rec)?.as_slice())?;
        Ok(())
    }

    /// Insert or replace the item at (label, rel) and make it live.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert(
        &self,
        label: &str,
        rel: &Path,
        size: u64,
        mtime_ns: i64,
        info: &ImageInfo,
        thumb: &str,
        preview: bool,
    ) -> Result<()> {
        let key = (label, rel_bytes(rel));
        let existing = self.w.open_table(PATHS)?.get(key)?.map(|g| g.value());
        let id = match existing {
            Some(id) => {
                if let Some(old) = self.load(id)? {
                    if !old.missing {
                        self.unlink(id, &old)?;
                    }
                }
                id
            }
            None => {
                let mut meta = self.w.open_table(META)?;
                let id = meta.get("next_id")?.map(|g| g.value()).unwrap_or(1);
                meta.insert("next_id", id + 1)?;
                self.w.open_table(PATHS)?.insert(key, id)?;
                id
            }
        };
        let rec = Record {
            label: label.to_string(),
            rel: rel_bytes(rel).to_vec(),
            size,
            mtime_ns,
            hash: info.hash.clone(),
            width: info.width,
            height: info.height,
            taken_local: info.taken_local.clone(),
            taken_offset: info.taken_offset.clone(),
            date_source: info.date_source.as_str().to_string(),
            lat: info.position.map(|p| p.0),
            lon: info.position.map(|p| p.1),
            thumb: thumb.to_string(),
            missing: false,
            preview,
            video: info.video.clone(),
        };
        self.store(id, &rec)?;
        self.link(id, &rec)
    }

    /// Mark an item missing (gone from its root) or live again.
    pub fn set_missing(&self, id: u64, missing: bool) -> Result<()> {
        let Some(mut rec) = self.load(id)? else {
            return Ok(());
        };
        if rec.missing == missing {
            return Ok(());
        }
        rec.missing = missing;
        self.store(id, &rec)?;
        if missing {
            self.unlink(id, &rec)
        } else {
            self.link(id, &rec)
        }
    }

    pub fn commit(self) -> Result<()> {
        self.w.commit()?;
        Ok(())
    }
}
