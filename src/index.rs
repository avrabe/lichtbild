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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
/// Content hash -> (difference hash of the thumbnail, has one): the
/// fingerprint duplicates are found by (REQ-DUP-001). A flat picture
/// stores (0, false).
const FINGERPRINTS: TableDefinition<&str, (u64, bool)> = TableDefinition::new("fingerprints");
/// SHA-256 of a session id -> (person, identity, expiry in Unix seconds), so a
/// restart does not sign anyone out (crate::auth::SessionStore).
const SESSIONS: TableDefinition<&str, (&str, &str, u64)> = TableDefinition::new("sessions");
/// (person, item id) -> (): what each person chose not to see
/// (REQ-DUP-002). Per person: one person's choice never changes another's
/// view; nothing in the libraries changes.
const HIDDEN: TableDefinition<(&str, u64), ()> = TableDefinition::new("hidden");

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
    /// Copies of this picture the viewer may see, this one included.
    pub copies: usize,
}

pub struct Index {
    db: Database,
    /// Counts committed write batches, so caches built from the index
    /// (the map's positions) know when to rebuild.
    generation: Arc<AtomicU64>,
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
            w.open_table(FINGERPRINTS)?;
            w.open_table(SESSIONS)?;
            w.open_table(HIDDEN)?;
        }
        w.commit()?;
        Ok(Index {
            db,
            generation: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Changes after every committed batch; equal values mean the live
    /// items have not changed in between.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
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
            generation: self.generation.clone(),
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
        self.timeline_where(labels, before, limit, |_, _| true)
    }

    /// The timeline with only the items `keep` accepts; the limit counts
    /// kept items, so a page is full however many are left out.
    pub fn timeline_where(
        &self,
        labels: &[String],
        before: Option<(&str, u64)>,
        limit: usize,
        keep: impl Fn(u64, &Record) -> bool,
    ) -> Result<Vec<Item>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let mut out = Vec::new();
        // Each candidate's record is read once and decides with `keep`.
        let mut take = |id: u64| -> Result<bool> {
            let Some(raw) = items.get(id)? else {
                return Ok(false);
            };
            let rec = decode(raw.value())?;
            if keep(id, &rec) {
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
                    copies: 1,
                });
            }
            Ok(out.len() >= limit)
        };
        match labels {
            [] => {}
            [one] => {
                let t = r.open_table(BY_ROOT)?;
                let (bt, bid) = before.unwrap_or((TAKEN_MAX, 0));
                for e in t
                    .range((one.as_str(), "", 0)..(one.as_str(), bt, bid))?
                    .rev()
                {
                    if take(e?.0.value().2)? {
                        break;
                    }
                }
            }
            many => {
                let t = r.open_table(BY_TIME)?;
                let (bt, bid) = before.unwrap_or((TAKEN_MAX, 0));
                for e in t.range(("", 0)..(bt, bid))?.rev() {
                    let (k, v) = e?;
                    if many.iter().any(|l| l == v.value()) && take(k.value().1)? {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Items taken on `month_day` ("MM-DD") in earlier years than `year`,
    /// in the libraries `labels`, newest first: (id, record).
    pub fn on_this_day(
        &self,
        labels: &[String],
        month_day: &str,
        year: &str,
        limit: usize,
    ) -> Result<Vec<(u64, Record)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(BY_TIME)?;
        let items = r.open_table(ITEMS)?;
        let mut out = Vec::new();
        for e in t.iter()?.rev() {
            let (k, v) = e?;
            let (taken, id) = k.value();
            if taken.get(5..10) != Some(month_day)
                || taken.get(..4).is_none_or(|y| y >= year)
                || !labels.iter().any(|l| l == v.value())
            {
                continue;
            }
            if let Some(raw) = items.get(id)? {
                out.push((id, decode(raw.value())?));
                if out.len() >= limit {
                    break;
                }
            }
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

    /// Live items with a thumbnail whose content has no fingerprint yet:
    /// their content hashes, one per hash.
    pub fn missing_fingerprints(&self) -> Result<Vec<String>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let fp = r.open_table(FINGERPRINTS)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for e in items.iter()? {
            let (_, v) = e?;
            let rec = decode(v.value())?;
            if rec.missing || rec.thumb != "ok" || !seen.insert(rec.hash.clone()) {
                continue;
            }
            if fp.get(rec.hash.as_str())?.is_none() {
                out.push(rec.hash);
            }
        }
        Ok(out)
    }

    pub fn set_fingerprint(&self, hash: &str, fp: Option<u64>) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(FINGERPRINTS)?;
            t.insert(hash, (fp.unwrap_or(0), fp.is_some()))?;
        }
        w.commit()?;
        Ok(())
    }

    /// Every stored fingerprint by content hash (None: a flat picture).
    pub fn fingerprints(&self) -> Result<HashMap<String, Option<u64>>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(FINGERPRINTS)?;
        let mut out = HashMap::new();
        for e in t.iter()? {
            let (k, v) = e?;
            let (h, some) = v.value();
            out.insert(k.value().to_string(), some.then_some(h));
        }
        Ok(out)
    }

    /// Hide or show again an item for one person.
    pub fn set_hidden(&self, person: &str, id: u64, hidden: bool) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(HIDDEN)?;
            if hidden {
                t.insert((person, id), ())?;
            } else {
                t.remove((person, id))?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// The items one person has hidden.
    pub fn hidden(&self, person: &str) -> Result<std::collections::HashSet<u64>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(HIDDEN)?;
        let mut out = std::collections::HashSet::new();
        for e in t.range((person, 0u64)..=(person, u64::MAX))? {
            out.insert(e?.0.value().1);
        }
        Ok(out)
    }

    /// Live videos (by content), how many have a transcript from `model`,
    /// and how many of those have speech.
    pub fn transcript_counts(&self, model: &str) -> Result<(u64, u64, u64)> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let t = r.open_table(TRANSCRIPTS)?;
        let mut seen = std::collections::HashSet::new();
        let (mut videos, mut done, mut speech) = (0, 0, 0);
        for e in items.iter()? {
            let rec = decode(e?.1.value())?;
            if rec.missing || rec.video.is_none() || !seen.insert(rec.hash.clone()) {
                continue;
            }
            videos += 1;
            if let Some(v) = t.get(rec.hash.as_str())? {
                if let Ok(tr) = serde_json::from_slice::<crate::stt::Transcript>(v.value()) {
                    if tr.model == model {
                        done += 1;
                        speech += u64::from(!tr.pieces.is_empty());
                    }
                }
            }
        }
        Ok((videos, done, speech))
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
    generation: Arc<AtomicU64>,
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
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

impl crate::auth::SessionStore for Index {
    fn put_session(
        &self,
        key: &str,
        person: &str,
        identity: &str,
        expires_unix: u64,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(SESSIONS)?;
            // Expired sessions go when a new one comes.
            let expired: Vec<String> = t
                .iter()?
                .filter_map(|e| e.ok())
                .filter(|(_, v)| v.value().2 <= now)
                .map(|(k, _)| k.value().to_string())
                .collect();
            for k in expired {
                t.remove(k.as_str())?;
            }
            t.insert(key, (person, identity, expires_unix))?;
        }
        w.commit()?;
        Ok(())
    }

    fn get_session(&self, key: &str) -> Result<Option<(String, String, u64)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(SESSIONS)?;
        Ok(t.get(key)?.map(|v| {
            let (p, i, e) = v.value();
            (p.to_string(), i.to_string(), e)
        }))
    }

    fn remove_session(&self, key: &str) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            w.open_table(SESSIONS)?.remove(key)?;
        }
        w.commit()?;
        Ok(())
    }
}
