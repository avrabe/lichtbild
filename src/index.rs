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
/// Text read from pictures (REQ-TEXT-001): content hash -> JSON `ocr::Text`.
const TEXTS: TableDefinition<&str, &[u8]> = TableDefinition::new("texts");
/// Things in pictures (REQ-SEE-001): content hash -> JSON `[objects::Object]`.
const OBJECTS: TableDefinition<&str, &[u8]> = TableDefinition::new("objects");
/// What pictures show (REQ-SEE-002): content hash -> embed::DIM signed bytes.
const VECTORS: TableDefinition<&str, &[u8]> = TableDefinition::new("vectors");
/// Which pictures were looked at: content hash -> vision::VERSION, or
/// "<version> failed (<reason>)".
const LOOKED: TableDefinition<&str, &str> = TableDefinition::new("looked");
/// Content hash -> (difference hash of the thumbnail, has one): the
/// fingerprint duplicates are found by (REQ-DUP-001). A flat picture
/// stores (0, false).
const FINGERPRINTS: TableDefinition<&str, (u64, bool)> = TableDefinition::new("fingerprints");
/// Content hash -> (sharpness, mean grey, clipped share) of its thumbnail
/// (REQ-WEB-030).
const QUALITY: TableDefinition<&str, (f32, f32, f32)> = TableDefinition::new("quality");
/// SHA-256 of a session id -> (person, identity, expiry in Unix seconds), so a
/// restart does not sign anyone out (crate::auth::SessionStore).
const SESSIONS: TableDefinition<&str, (&str, &str, u64)> = TableDefinition::new("sessions");
/// (person, item id) -> (): what each person chose not to see
/// (REQ-DUP-002). Per person: one person's choice never changes another's
/// view; nothing in the libraries changes.
const HIDDEN: TableDefinition<(&str, u64), ()> = TableDefinition::new("hidden");
/// (person, item id) -> (): each person's favourites (REQ-WEB-016), kept
/// apart like what they hid.
const FAVS: TableDefinition<(&str, u64), ()> = TableDefinition::new("favourites");
/// Album id -> JSON `Album` (REQ-WEB-017): each person's own albums.
const ALBUMS: TableDefinition<u64, &[u8]> = TableDefinition::new("albums");
/// (album id, item id) -> (): what an album holds.
const ALBUM_ITEMS: TableDefinition<(u64, u64), ()> = TableDefinition::new("album_items");
/// The last album id given out; ids are never given out twice, so an old
/// link to a deleted album never opens a newer one.
const ALBUM_SEQ: TableDefinition<&str, u64> = TableDefinition::new("album_seq");
/// Faces in pictures (REQ-FACE-002): content hash -> JSON `[faces::Face]`.
const FACES: TableDefinition<&str, &[u8]> = TableDefinition::new("faces");
/// Which pictures were looked at for faces: content hash -> faces::VERSION,
/// or "<version> failed (<reason>)".
const FACED: TableDefinition<&str, &str> = TableDefinition::new("faced");
/// Person id -> JSON `Person` (REQ-FACE-003): the names given to faces,
/// one set for the whole household.
const PEOPLE: TableDefinition<u64, &[u8]> = TableDefinition::new("people");
/// The last person id given out; never given out twice.
const PEOPLE_SEQ: TableDefinition<&str, u64> = TableDefinition::new("people_seq");
/// (content hash, face number) -> person id: faces someone named.
const FACE_PERSON: TableDefinition<(&str, u32), u64> = TableDefinition::new("face_person");
/// (content hash, face number, person id) -> (): "this is not them".
const FACE_NOT: TableDefinition<(&str, u32, u64), ()> = TableDefinition::new("face_not");
/// Corrections of things found (REQ-FIX-001): content hash -> JSON
/// `[fixes::ThingFix]`.
const THING_FIXES: TableDefinition<&str, &[u8]> = TableDefinition::new("thing_fixes");
/// Kinds never in the house (REQ-FIX-001): kind -> JSON `fixes::Mark`.
const KIND_OFF: TableDefinition<u8, &[u8]> = TableDefinition::new("kind_off");
/// The house's own words for things (REQ-FIX-003): kind (from
/// `fixes::OWN_FIRST`) -> JSON `fixes::OwnWord` (the word and the
/// libraries it was used in).
const OWN_KINDS: TableDefinition<u8, &[u8]> = TableDefinition::new("own_kinds");
/// Corrections of text read (REQ-FIX-002): content hash -> JSON
/// `[fixes::TextFix]`.
const TEXT_FIXES: TableDefinition<&str, &[u8]> = TableDefinition::new("text_fixes");
/// Positions set by hand (REQ-PLACE-002): item id -> JSON `Placed`.
const PLACED: TableDefinition<u64, &[u8]> = TableDefinition::new("placed");
/// How named people stand to each other (REQ-FACE-004): (a, b, kind) ->
/// how much it matters (1 to 3). See `relations::Kind` for the order of a
/// and b.
const RELATIONS: TableDefinition<(u64, u64, u8), u8> = TableDefinition::new("relations");

/// Sorts after every timestamp string ("YYYY-...").
const TAKEN_MAX: &str = "\u{7f}";

pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("index.redb")
}

pub fn rel_bytes(rel: &Path) -> &[u8] {
    rel.as_os_str().as_bytes()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
    /// A Live Photo: the id of its short clip, shown with it (REQ-MEDIA-005).
    #[serde(default)]
    pub live: Option<u64>,
    /// The clip of a Live Photo: the photo's id. Such a clip is not an
    /// item of its own: no timeline, search, map or pass lists it.
    #[serde(default)]
    pub live_of: Option<u64>,
    /// When lichtbild first indexed this file, in seconds since 1970; 0 for
    /// files indexed before v0.12.0 (REQ-WEB-029).
    #[serde(default)]
    pub added: u64,
}

impl Record {
    /// Listed as an item of its own: live and not a Live Photo's clip.
    pub fn shown(&self) -> bool {
        !self.missing && self.live_of.is_none()
    }
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
#[derive(Debug, Clone, Default)]
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
    /// The viewer's favourite.
    pub fav: bool,
    /// A Live Photo, with a clip (REQ-MEDIA-005).
    pub live: bool,
}

/// A name given to faces (REQ-FACE-003), shared in the household.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Person {
    /// Empty for faces hidden without a name (strangers).
    pub name: String,
    /// Who gave the name.
    pub by: String,
    pub created: u64,
    /// Hidden (REQ-FACE-005): not on the People page, in a photo's faces,
    /// in name suggestions, search by name or relations; the photos stay.
    #[serde(default)]
    pub hidden: bool,
    /// "MM-DD", or "YYYY-MM-DD" when the year is known (REQ-FACE-007).
    #[serde(default)]
    pub birthday: Option<String>,
    /// The face chosen to stand for this person: (content hash, face
    /// number), shown on People and in the family tree (REQ-FACE-008).
    #[serde(default)]
    pub portrait: Option<(String, u32)>,
}

/// An album (REQ-WEB-017): whose it is, its name, when it was made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub owner: String,
    pub name: String,
    pub created: u64,
    /// Shown to everyone in the household (only what each may see);
    /// off unless the owner turns it on.
    #[serde(default)]
    pub shared: bool,
}

impl Item {
    fn of(id: u64, rec: Record) -> Item {
        Item {
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
            fav: false,
            live: rec.live.is_some(),
        }
    }
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
        let path = db_path(data_dir);
        let db = match Database::create(&path) {
            Ok(db) => db,
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => bail!(
                "the index {} is in use by another lichtbild process (a running `lichtbild serve` scans by itself)",
                path.display()
            ),
            Err(e) => return Err(e).context(format!("cannot open the index {}", path.display())),
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
            w.open_table(QUALITY)?;
            w.open_table(SESSIONS)?;
            w.open_table(HIDDEN)?;
            w.open_table(FAVS)?;
            w.open_table(ALBUMS)?;
            w.open_table(ALBUM_ITEMS)?;
            w.open_table(ALBUM_SEQ)?;
            w.open_table(PLACED)?;
            w.open_table(TEXTS)?;
            w.open_table(OBJECTS)?;
            w.open_table(VECTORS)?;
            w.open_table(LOOKED)?;
            w.open_table(FACES)?;
            w.open_table(FACED)?;
            w.open_table(PEOPLE)?;
            w.open_table(PEOPLE_SEQ)?;
            w.open_table(FACE_PERSON)?;
            w.open_table(FACE_NOT)?;
            w.open_table(THING_FIXES)?;
            w.open_table(KIND_OFF)?;
            w.open_table(OWN_KINDS)?;
            w.open_table(TEXT_FIXES)?;
            w.open_table(RELATIONS)?;
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
        self.months_where(labels, |_| true, None)
    }

    /// `months`, counting only the items `keep` accepts and, when `video`
    /// is set, only videos (true) or only photos (false). The kind needs
    /// the record, so only a filtered count reads it.
    pub fn months_where(
        &self,
        labels: &[String],
        keep: impl Fn(u64) -> bool,
        video: Option<bool>,
    ) -> Result<Vec<(String, u64)>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let wanted = |id: u64| -> Result<bool> {
            if !keep(id) {
                return Ok(false);
            }
            let Some(v) = video else { return Ok(true) };
            Ok(match items.get(id)? {
                Some(raw) => decode(raw.value())?.video.is_some() == v,
                None => false,
            })
        };
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
                    let (k, _) = e?;
                    let (_, taken, id) = k.value();
                    if wanted(id)? {
                        add(taken);
                    }
                }
            }
            many => {
                let t = r.open_table(BY_TIME)?;
                for e in t.iter()?.rev() {
                    let (k, v) = e?;
                    if many.iter().any(|l| l == v.value()) && wanted(k.value().1)? {
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
        self.neighbours_where(labels, taken, id, |_, _| true)
    }

    /// `neighbours`, stepping over the items `keep` rejects, so the viewer
    /// walks the same items as the timeline it came from.
    pub fn neighbours_where(
        &self,
        labels: &[String],
        taken: &str,
        id: u64,
        keep: impl Fn(u64, &Record) -> bool,
    ) -> Result<(Option<u64>, Option<u64>)> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let ok = |n: u64| -> Result<bool> {
            Ok(match items.get(n)? {
                Some(raw) => {
                    let rec = decode(raw.value())?;
                    rec.shown() && keep(n, &rec)
                }
                None => false,
            })
        };
        let mut newer = None;
        let mut older = None;
        match labels {
            [] => {}
            [one] => {
                let t = r.open_table(BY_ROOT)?;
                let l = one.as_str();
                for e in t.range((l, taken, id + 1)..(l, TAKEN_MAX, 0))? {
                    let n = e?.0.value().2;
                    if ok(n)? {
                        newer = Some(n);
                        break;
                    }
                }
                for e in t.range((l, "", 0)..(l, taken, id))?.rev() {
                    let n = e?.0.value().2;
                    if ok(n)? {
                        older = Some(n);
                        break;
                    }
                }
            }
            many => {
                let t = r.open_table(BY_TIME)?;
                for e in t.range((taken, id + 1)..(TAKEN_MAX, 0))? {
                    let (k, v) = e?;
                    if many.iter().any(|x| x == v.value()) && ok(k.value().1)? {
                        newer = Some(k.value().1);
                        break;
                    }
                }
                for e in t.range(("", 0)..(taken, id))?.rev() {
                    let (k, v) = e?;
                    if many.iter().any(|x| x == v.value()) && ok(k.value().1)? {
                        older = Some(k.value().1);
                        break;
                    }
                }
            }
        }
        Ok((newer, older))
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
                out.push(Item::of(id, rec));
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

    /// The oldest item taken at or after `from` (a "taken" prefix such as
    /// "YYYY-MM-DDT99") that `keep` accepts: the next day for the days page.
    pub fn first_from(
        &self,
        labels: &[String],
        from: &str,
        keep: impl Fn(u64, &Record) -> bool,
    ) -> Result<Option<Item>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let pick = |id: u64| -> Result<Option<Item>> {
            let Some(raw) = items.get(id)? else {
                return Ok(None);
            };
            let rec = decode(raw.value())?;
            Ok(keep(id, &rec).then(|| Item::of(id, rec)))
        };
        match labels {
            [] => {}
            [one] => {
                let t = r.open_table(BY_ROOT)?;
                for e in t.range((one.as_str(), from, 0)..(one.as_str(), TAKEN_MAX, 0))? {
                    if let Some(it) = pick(e?.0.value().2)? {
                        return Ok(Some(it));
                    }
                }
            }
            many => {
                let t = r.open_table(BY_TIME)?;
                for e in t.range((from, 0)..(TAKEN_MAX, 0))? {
                    let (k, v) = e?;
                    if many.iter().any(|l| l == v.value()) {
                        if let Some(it) = pick(k.value().1)? {
                            return Ok(Some(it));
                        }
                    }
                }
            }
        }
        Ok(None)
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

    /// Photos with a thumbnail and no quality measure yet: their content
    /// hashes, one per hash.
    pub fn missing_quality(&self) -> Result<Vec<String>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let q = r.open_table(QUALITY)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for e in items.iter()? {
            let rec = decode(e?.1.value())?;
            if !rec.shown()
                || rec.video.is_some()
                || rec.thumb != "ok"
                || !seen.insert(rec.hash.clone())
            {
                continue;
            }
            if q.get(rec.hash.as_str())?.is_none() {
                out.push(rec.hash);
            }
        }
        Ok(out)
    }

    /// Store quality measures, in one write.
    pub fn set_qualities(&self, qs: &[(String, crate::best::Quality)]) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(QUALITY)?;
            for (hash, q) in qs {
                t.insert(hash.as_str(), (q.sharp, q.mean, q.clipped))?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// Every stored quality measure by content hash.
    pub fn qualities(&self) -> Result<HashMap<String, crate::best::Quality>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(QUALITY)?;
        let mut out = HashMap::new();
        for e in t.iter()? {
            let (k, v) = e?;
            let (sharp, mean, clipped) = v.value();
            out.insert(
                k.value().to_string(),
                crate::best::Quality {
                    sharp,
                    mean,
                    clipped,
                },
            );
        }
        Ok(out)
    }

    /// Hide or show again an item for one person.
    pub fn set_hidden(&self, person: &str, id: u64, hidden: bool) -> Result<()> {
        self.mark(HIDDEN, person, id, hidden)
    }

    /// Make an item one person's favourite, or not any more.
    pub fn set_fav(&self, person: &str, id: u64, fav: bool) -> Result<()> {
        self.mark(FAVS, person, id, fav)
    }

    fn mark(
        &self,
        table: TableDefinition<(&str, u64), ()>,
        person: &str,
        id: u64,
        on: bool,
    ) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(table)?;
            if on {
                t.insert((person, id), ())?;
            } else {
                t.remove((person, id))?;
            }
        }
        w.commit()?;
        Ok(())
    }

    fn marked(
        &self,
        table: TableDefinition<(&str, u64), ()>,
        person: &str,
    ) -> Result<std::collections::HashSet<u64>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(table)?;
        let mut out = std::collections::HashSet::new();
        for e in t.range((person, 0u64)..=(person, u64::MAX))? {
            out.insert(e?.0.value().1);
        }
        Ok(out)
    }

    /// Set (Some) or remove (None) the position of an item set by hand.
    /// Never written to the source file (REQ-SCAN-001).
    pub fn set_placed(&self, id: u64, placed: Option<&crate::place::Placed>) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(PLACED)?;
            match placed {
                Some(p) => {
                    t.insert(id, serde_json::to_vec(p)?.as_slice())?;
                }
                None => {
                    t.remove(id)?;
                }
            }
        }
        w.commit()?;
        // Maps and estimates read positions through the generation.
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Every position set by hand.
    pub fn placed(&self) -> Result<HashMap<u64, crate::place::Placed>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(PLACED)?;
        let mut out = HashMap::new();
        for e in t.iter()? {
            let (k, v) = e?;
            if let Ok(p) = serde_json::from_slice(v.value()) {
                out.insert(k.value(), p);
            }
        }
        Ok(out)
    }

    /// The items one person has hidden.
    pub fn hidden(&self, person: &str) -> Result<std::collections::HashSet<u64>> {
        self.marked(HIDDEN, person)
    }

    /// One person's favourites.
    pub fn favs(&self, person: &str) -> Result<std::collections::HashSet<u64>> {
        self.marked(FAVS, person)
    }

    /// Given ids in `labels` that `keep` accepts, newest first as in the
    /// timeline: for a small chosen set (favourites, an album) without
    /// reading the whole library.
    pub fn items_of(
        &self,
        ids: impl IntoIterator<Item = u64>,
        labels: &[String],
        keep: impl Fn(u64, &Record) -> bool,
    ) -> Result<Vec<Item>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let mut out = Vec::new();
        for id in ids {
            let Some(raw) = items.get(id)? else { continue };
            let rec = decode(raw.value())?;
            if labels.contains(&rec.label) && keep(id, &rec) {
                out.push(Item::of(id, rec));
            }
        }
        out.sort_by(|a, b| (&b.taken_local, b.id).cmp(&(&a.taken_local, a.id)));
        Ok(out)
    }

    /// The most recently indexed items in `labels` that `keep` accepts,
    /// newest first by when they were indexed (ids grow with every new
    /// file), at most `limit` (REQ-WEB-029).
    pub fn recent(
        &self,
        labels: &[String],
        limit: usize,
        keep: impl Fn(u64, &Record) -> bool,
    ) -> Result<Vec<(Item, u64)>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let mut out = Vec::new();
        for e in items.iter()?.rev() {
            let (k, v) = e?;
            let rec = decode(v.value())?;
            let id = k.value();
            if rec.shown() && labels.contains(&rec.label) && keep(id, &rec) {
                let added = rec.added;
                out.push((Item::of(id, rec), added));
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// A new, empty album; its id.
    pub fn create_album(&self, owner: &str, name: &str, now: u64) -> Result<u64> {
        let w = self.db.begin_write()?;
        let id;
        {
            let mut seq = w.open_table(ALBUM_SEQ)?;
            let last = seq.get("albums")?.map_or(0, |v| v.value());
            let mut t = w.open_table(ALBUMS)?;
            id = last.max(t.last()?.map_or(0, |(k, _)| k.value())) + 1;
            seq.insert("albums", id)?;
            let a = Album {
                owner: owner.to_string(),
                name: name.to_string(),
                created: now,
                shared: false,
            };
            t.insert(id, serde_json::to_vec(&a)?.as_slice())?;
        }
        w.commit()?;
        Ok(id)
    }

    pub fn album(&self, id: u64) -> Result<Option<Album>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(ALBUMS)?;
        Ok(match t.get(id)? {
            Some(v) => serde_json::from_slice(v.value()).ok(),
            None => None,
        })
    }

    /// One person's albums, newest first.
    pub fn albums(&self, owner: &str) -> Result<Vec<(u64, Album)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(ALBUMS)?;
        let mut out = Vec::new();
        for e in t.iter()?.rev() {
            let (k, v) = e?;
            if let Ok(a) = serde_json::from_slice::<Album>(v.value()) {
                if a.owner == owner {
                    out.push((k.value(), a));
                }
            }
        }
        Ok(out)
    }

    /// Rename an album.
    pub fn rename_album(&self, id: u64, name: &str) -> Result<()> {
        self.change_album(id, |a| a.name = name.to_string())
    }

    /// Share an album with the household, or stop sharing it.
    pub fn share_album(&self, id: u64, shared: bool) -> Result<()> {
        self.change_album(id, |a| a.shared = shared)
    }

    /// Change an album, read and written in one transaction, so a delete
    /// in between cannot bring it back.
    fn change_album(&self, id: u64, change: impl FnOnce(&mut Album)) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(ALBUMS)?;
            let a = match t.get(id)? {
                Some(v) => serde_json::from_slice::<Album>(v.value()).ok(),
                None => None,
            };
            if let Some(mut a) = a {
                change(&mut a);
                t.insert(id, serde_json::to_vec(&a)?.as_slice())?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// Albums others share with the household, newest first.
    pub fn shared_albums(&self, me: &str) -> Result<Vec<(u64, Album)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(ALBUMS)?;
        let mut out = Vec::new();
        for e in t.iter()?.rev() {
            let (k, v) = e?;
            if let Ok(a) = serde_json::from_slice::<Album>(v.value()) {
                if a.shared && a.owner != me {
                    out.push((k.value(), a));
                }
            }
        }
        Ok(out)
    }

    /// Remove an album and what it holds; the pictures stay.
    pub fn delete_album(&self, id: u64) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            w.open_table(ALBUMS)?.remove(id)?;
            let mut t = w.open_table(ALBUM_ITEMS)?;
            let ids: Vec<u64> = t
                .range((id, 0u64)..=(id, u64::MAX))?
                .map(|e| e.map(|(k, _)| k.value().1))
                .collect::<std::result::Result<_, _>>()?;
            for i in ids {
                t.remove((id, i))?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// Put an item into an album, or take it out.
    pub fn set_in_album(&self, album: u64, item: u64, on: bool) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(ALBUM_ITEMS)?;
            if on {
                t.insert((album, item), ())?;
            } else {
                t.remove((album, item))?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// What an album holds.
    pub fn album_items(&self, album: u64) -> Result<std::collections::HashSet<u64>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(ALBUM_ITEMS)?;
        let mut out = std::collections::HashSet::new();
        for e in t.range((album, 0u64)..=(album, u64::MAX))? {
            out.insert(e?.0.value().1);
        }
        Ok(out)
    }

    /// Live videos (by content), how many have a transcript from `model`,
    /// and how many of those have speech.
    pub fn transcript_counts(&self, labels: &[String], model: &str) -> Result<(u64, u64, u64)> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let t = r.open_table(TRANSCRIPTS)?;
        let mut seen = std::collections::HashSet::new();
        let (mut videos, mut done, mut speech) = (0, 0, 0);
        for e in items.iter()? {
            let rec = decode(e?.1.value())?;
            if !rec.shown()
                || rec.video.is_none()
                || !labels.contains(&rec.label)
                || !seen.insert(rec.hash.clone())
            {
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

    /// The text read from a picture, by content hash.
    pub fn text(&self, hash: &str) -> Result<Option<crate::ocr::Text>> {
        let fixes = self.text_fixes(hash)?;
        Ok(self
            .raw_text(hash)?
            .map(|t| crate::fixes::apply_text(t, &fixes)))
    }

    /// The text as it was read, without the house's corrections.
    pub fn raw_text(&self, hash: &str) -> Result<Option<crate::ocr::Text>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(TEXTS)?;
        match t.get(hash)? {
            Some(v) => Ok(Some(
                serde_json::from_slice(v.value()).context("corrupt text")?,
            )),
            None => Ok(None),
        }
    }

    /// The corrections of the text read in a photo.
    pub fn text_fixes(&self, hash: &str) -> Result<Vec<crate::fixes::TextFix>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(TEXT_FIXES)?;
        Ok(match t.get(hash)? {
            Some(v) => serde_json::from_slice(v.value()).unwrap_or_default(),
            None => Vec::new(),
        })
    }

    /// Every correction of text: (content hash, corrections).
    pub fn all_text_fixes(&self) -> Result<Vec<(String, Vec<crate::fixes::TextFix>)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(TEXT_FIXES)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let (k, v) = e?;
            out.push((
                k.value().to_string(),
                serde_json::from_slice(v.value()).unwrap_or_default(),
            ));
        }
        Ok(out)
    }

    /// Put a correction of a line, replacing an earlier one of that line;
    /// with `undo`, the correction number `undo` is taken out instead.
    pub fn fix_text(
        &self,
        hash: &str,
        fix: Option<crate::fixes::TextFix>,
        undo: Option<usize>,
    ) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(TEXT_FIXES)?;
            let mut v: Vec<crate::fixes::TextFix> = match t.get(hash)? {
                Some(v) => serde_json::from_slice(v.value()).unwrap_or_default(),
                None => Vec::new(),
            };
            if let Some(i) = undo.filter(|i| *i < v.len()) {
                v.remove(i);
            }
            if let Some(f) = fix {
                v.retain(|o| {
                    crate::fixes::text_about(std::slice::from_ref(o), f.bbox, &f.was).is_none()
                });
                v.push(f);
            }
            if v.is_empty() {
                t.remove(hash)?;
            } else {
                t.insert(hash, serde_json::to_vec(&v)?.as_slice())?;
            }
        }
        w.commit()?;
        Ok(())
    }

    pub fn set_text(&self, hash: &str, text: &crate::ocr::Text) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(TEXTS)?;
            t.insert(hash, serde_json::to_vec(text)?.as_slice())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Store what the look child found in a picture.
    pub fn set_look(&self, hash: &str, version: &str, look: &crate::vision::Look) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            w.open_table(TEXTS)?
                .insert(hash, serde_json::to_vec(&look.text)?.as_slice())?;
            w.open_table(OBJECTS)?
                .insert(hash, serde_json::to_vec(&look.objects)?.as_slice())?;
            let v: Vec<u8> = look.vector.iter().map(|b| *b as u8).collect();
            if v.len() == crate::embed::DIM {
                w.open_table(VECTORS)?.insert(hash, v.as_slice())?;
            }
            w.open_table(LOOKED)?.insert(hash, version)?;
        }
        w.commit()?;
        Ok(())
    }

    /// Mark a picture the look child failed on, so it is not tried again
    /// with this version.
    pub fn set_look_failed(&self, hash: &str, version: &str, reason: &str) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mark = format!("{version} failed ({reason})");
            w.open_table(LOOKED)?.insert(hash, mark.as_str())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Store the faces found in a picture.
    pub fn set_faces(&self, hash: &str, version: &str, faces: &[crate::faces::Face]) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            w.open_table(FACES)?
                .insert(hash, serde_json::to_vec(faces)?.as_slice())?;
            w.open_table(FACED)?.insert(hash, version)?;
        }
        w.commit()?;
        Ok(())
    }

    /// Mark a picture the faces child failed on.
    pub fn set_faces_failed(&self, hash: &str, version: &str, reason: &str) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mark = format!("{version} failed ({reason})");
            w.open_table(FACED)?.insert(hash, mark.as_str())?;
        }
        w.commit()?;
        Ok(())
    }

    /// The faces found in a picture, by content hash.
    pub fn faces(&self, hash: &str) -> Result<Vec<crate::faces::Face>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(FACES)?;
        match t.get(hash)? {
            Some(v) => Ok(serde_json::from_slice(v.value()).unwrap_or_default()),
            None => Ok(Vec::new()),
        }
    }

    /// Every stored face: (content hash, faces).
    pub fn all_faces(&self) -> Result<Vec<(String, Vec<crate::faces::Face>)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(FACES)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let (k, v) = e?;
            if let Ok(f) = serde_json::from_slice::<Vec<crate::faces::Face>>(v.value()) {
                if !f.is_empty() {
                    out.push((k.value().to_string(), f));
                }
            }
        }
        Ok(out)
    }

    /// Give a new name to the household's people; returns its id.
    pub fn create_person(&self, name: &str, by: &str, now: u64) -> Result<u64> {
        let w = self.db.begin_write()?;
        let id;
        {
            let mut seq = w.open_table(PEOPLE_SEQ)?;
            let mut t = w.open_table(PEOPLE)?;
            let last = seq.get("people")?.map_or(0, |v| v.value());
            id = last.max(t.last()?.map_or(0, |(k, _)| k.value())) + 1;
            seq.insert("people", id)?;
            let p = Person {
                name: name.to_string(),
                by: by.to_string(),
                created: now,
                hidden: false,
                birthday: None,
                portrait: None,
            };
            t.insert(id, serde_json::to_vec(&p)?.as_slice())?;
        }
        w.commit()?;
        Ok(id)
    }

    /// Every named person, by id.
    pub fn people(&self) -> Result<Vec<(u64, Person)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(PEOPLE)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let (k, v) = e?;
            if let Ok(p) = serde_json::from_slice(v.value()) {
                out.push((k.value(), p));
            }
        }
        Ok(out)
    }

    pub fn person(&self, id: u64) -> Result<Option<Person>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(PEOPLE)?;
        Ok(match t.get(id)? {
            Some(v) => serde_json::from_slice(v.value()).ok(),
            None => None,
        })
    }

    pub fn rename_person(&self, id: u64, name: &str) -> Result<bool> {
        let w = self.db.begin_write()?;
        let found;
        {
            let mut t = w.open_table(PEOPLE)?;
            let p: Option<Person> = t
                .get(id)?
                .and_then(|v| serde_json::from_slice(v.value()).ok());
            found = p.is_some();
            if let Some(mut p) = p {
                p.name = name.to_string();
                t.insert(id, serde_json::to_vec(&p)?.as_slice())?;
            }
        }
        w.commit()?;
        Ok(found)
    }

    /// Hide a person or show them again; false when there is no such person.
    pub fn set_person_hidden(&self, id: u64, hidden: bool) -> Result<bool> {
        let w = self.db.begin_write()?;
        let found;
        {
            let mut t = w.open_table(PEOPLE)?;
            let p: Option<Person> = t
                .get(id)?
                .and_then(|v| serde_json::from_slice(v.value()).ok());
            found = p.is_some();
            if let Some(mut p) = p {
                p.hidden = hidden;
                t.insert(id, serde_json::to_vec(&p)?.as_slice())?;
            }
        }
        w.commit()?;
        Ok(found)
    }

    /// Set or clear a person's birthday; false when there is no such person.
    pub fn set_birthday(&self, id: u64, birthday: Option<&str>) -> Result<bool> {
        self.change_person(id, |p| p.birthday = birthday.map(str::to_string))
    }

    /// Choose the face that stands for a person (REQ-FACE-008).
    pub fn set_portrait(&self, id: u64, hash: &str, face: u32) -> Result<bool> {
        self.change_person(id, |p| p.portrait = Some((hash.to_string(), face)))
    }

    fn change_person(&self, id: u64, f: impl FnOnce(&mut Person)) -> Result<bool> {
        let w = self.db.begin_write()?;
        let found;
        {
            let mut t = w.open_table(PEOPLE)?;
            let p: Option<Person> = t
                .get(id)?
                .and_then(|v| serde_json::from_slice(v.value()).ok());
            found = p.is_some();
            if let Some(mut p) = p {
                f(&mut p);
                t.insert(id, serde_json::to_vec(&p)?.as_slice())?;
            }
        }
        w.commit()?;
        Ok(found)
    }

    /// Name faces: each becomes `person`'s, and an earlier "not them" for
    /// it is forgotten.
    pub fn name_faces(&self, faces: &[(String, u32)], person: u64) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut named = w.open_table(FACE_PERSON)?;
            let mut not = w.open_table(FACE_NOT)?;
            for (h, i) in faces {
                named.insert((h.as_str(), *i), person)?;
                not.remove((h.as_str(), *i, person))?;
            }
        }
        w.commit()?;
        Ok(())
    }

    /// "This face is not `person`": its name is taken off, and it is never
    /// given that name again by itself.
    pub fn not_person(&self, hash: &str, face: u32, person: u64) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut named = w.open_table(FACE_PERSON)?;
            if named.get((hash, face))?.map(|v| v.value()) == Some(person) {
                named.remove((hash, face))?;
            }
            w.open_table(FACE_NOT)?.insert((hash, face, person), ())?;
        }
        w.commit()?;
        Ok(())
    }

    /// Put `from`'s faces under `into` and drop the name `from`.
    pub fn merge_people(&self, from: u64, into: u64) -> Result<()> {
        if from == into {
            return Ok(());
        }
        let w = self.db.begin_write()?;
        {
            let mut named = w.open_table(FACE_PERSON)?;
            let moved: Vec<(String, u32)> = named
                .iter()?
                .filter_map(|e| e.ok())
                .filter(|(_, v)| v.value() == from)
                .map(|(k, _)| (k.value().0.to_string(), k.value().1))
                .collect();
            for (h, i) in &moved {
                named.insert((h.as_str(), *i), into)?;
            }
            let mut not = w.open_table(FACE_NOT)?;
            let nots: Vec<(String, u32)> = not
                .iter()?
                .filter_map(|e| e.ok())
                .filter(|(k, _)| k.value().2 == from)
                .map(|(k, _)| (k.value().0.to_string(), k.value().1))
                .collect();
            for (h, i) in &nots {
                not.remove((h.as_str(), *i, from))?;
                if !moved.iter().any(|(mh, mi)| mh == h && mi == i) {
                    not.insert((h.as_str(), *i, into), ())?;
                }
            }
            let mut rel = w.open_table(RELATIONS)?;
            let old: Vec<crate::relations::Row> = rel
                .iter()?
                .filter_map(|e| e.ok())
                .map(|(k, v)| (k.value(), v.value()))
                .filter(|((a, b, _), _)| *a == from || *b == from)
                .collect();
            for ((a, b, k), weight) in old {
                rel.remove((a, b, k))?;
                let swap = |x: u64| if x == from { into } else { x };
                if let Some(key) = crate::relations::key(swap(a), swap(b), k) {
                    let keep = rel.get(key)?.map_or(0, |v| v.value());
                    rel.insert(key, weight.max(keep))?;
                }
            }
            w.open_table(PEOPLE)?.remove(from)?;
        }
        w.commit()?;
        Ok(())
    }

    /// Drop a name: its faces are unnamed again.
    pub fn delete_person(&self, id: u64) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut named = w.open_table(FACE_PERSON)?;
            let gone: Vec<(String, u32)> = named
                .iter()?
                .filter_map(|e| e.ok())
                .filter(|(_, v)| v.value() == id)
                .map(|(k, _)| (k.value().0.to_string(), k.value().1))
                .collect();
            for (h, i) in &gone {
                named.remove((h.as_str(), *i))?;
            }
            let mut not = w.open_table(FACE_NOT)?;
            let gone: Vec<(String, u32)> = not
                .iter()?
                .filter_map(|e| e.ok())
                .filter(|(k, _)| k.value().2 == id)
                .map(|(k, _)| (k.value().0.to_string(), k.value().1))
                .collect();
            for (h, i) in &gone {
                not.remove((h.as_str(), *i, id))?;
            }
            let mut rel = w.open_table(RELATIONS)?;
            let gone: Vec<(u64, u64, u8)> = rel
                .iter()?
                .filter_map(|e| e.ok())
                .map(|(k, _)| k.value())
                .filter(|(a, b, _)| *a == id || *b == id)
                .collect();
            for k in gone {
                rel.remove(k)?;
            }
            w.open_table(PEOPLE)?.remove(id)?;
        }
        w.commit()?;
        Ok(())
    }

    /// Set how `a` stands to `b` and how much it matters (1 to 3). A kind
    /// read from `b`'s side (child of) is stored from `a`'s; a relation
    /// to oneself is refused.
    pub fn set_relation(&self, a: u64, b: u64, kind: u8, weight: u8) -> Result<bool> {
        let Some(key) = crate::relations::key(a, b, kind) else {
            return Ok(false);
        };
        let w = self.db.begin_write()?;
        w.open_table(RELATIONS)?.insert(key, weight.clamp(1, 3))?;
        w.commit()?;
        Ok(true)
    }

    pub fn remove_relation(&self, a: u64, b: u64, kind: u8) -> Result<()> {
        let Some(key) = crate::relations::key(a, b, kind) else {
            return Ok(());
        };
        let w = self.db.begin_write()?;
        w.open_table(RELATIONS)?.remove(key)?;
        w.commit()?;
        Ok(())
    }

    /// Every relation: ((a, b, kind), weight), as stored.
    pub fn relations(&self) -> Result<Vec<crate::relations::Row>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(RELATIONS)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let (k, v) = e?;
            out.push((k.value(), v.value()));
        }
        Ok(out)
    }

    /// Every named face: ((content hash, face number), person id).
    pub fn named_faces(&self) -> Result<Vec<((String, u32), u64)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(FACE_PERSON)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let (k, v) = e?;
            out.push(((k.value().0.to_string(), k.value().1), v.value()));
        }
        Ok(out)
    }

    /// Every "not them": (content hash, face number, person id).
    pub fn not_faces(&self) -> Result<Vec<(String, u32, u64)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(FACE_NOT)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let (k, _) = e?;
            let (h, i, p) = k.value();
            out.push((h.to_string(), i, p));
        }
        Ok(out)
    }

    /// The things in a picture as the house says they are (REQ-FIX-001).
    pub fn objects(&self, hash: &str) -> Result<Vec<crate::objects::Object>> {
        Ok(self.fixes()?.apply(hash, self.raw_objects(hash)?))
    }

    /// The things the detector found in a picture, by content hash.
    pub fn raw_objects(&self, hash: &str) -> Result<Vec<crate::objects::Object>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(OBJECTS)?;
        match t.get(hash)? {
            Some(v) => Ok(serde_json::from_slice(v.value()).unwrap_or_default()),
            None => Ok(Vec::new()),
        }
    }

    /// Every correction of things and the kinds never in the house.
    pub fn fixes(&self) -> Result<crate::fixes::Fixes> {
        let r = self.db.begin_read()?;
        let t = r.open_table(THING_FIXES)?;
        let mut things = HashMap::new();
        for e in t.iter()? {
            let (k, v) = e?;
            things.insert(
                k.value().to_string(),
                serde_json::from_slice(v.value()).unwrap_or_default(),
            );
        }
        let t = r.open_table(KIND_OFF)?;
        let mut off = HashMap::new();
        for e in t.iter()? {
            let (k, v) = e?;
            if let Ok(m) = serde_json::from_slice(v.value()) {
                off.insert(k.value(), m);
            }
        }
        let t = r.open_table(OWN_KINDS)?;
        let mut own = HashMap::new();
        for e in t.iter()? {
            let (k, v) = e?;
            if let Ok(o) = serde_json::from_slice(v.value()) {
                own.insert(k.value(), o);
            }
        }
        Ok(crate::fixes::Fixes::new(things, off).with_own(own))
    }

    /// The kind of the house's own word `word` (REQ-FIX-003), used on a
    /// photo of library `label`: the one it already has under that word
    /// (case and accents aside), else a free one; the library is noted,
    /// so only those who see it are offered the word. A kind no
    /// correction uses any more is free again. None when the word is not
    /// one (`fixes::own_word`) or every own kind is in use.
    ///
    /// With `fix` (a photo's content hash and a correction), the
    /// correction is saved as this kind in the same write: a kind given
    /// out is in use before anyone else can take it back.
    pub fn own_kind(
        &self,
        word: &str,
        label: &str,
        fix: Option<(&str, crate::fixes::ThingFix)>,
    ) -> Result<Option<u8>> {
        let Some(word) = crate::fixes::own_word(word) else {
            return Ok(None);
        };
        let folded = crate::search::fold(word);
        let w = self.db.begin_write()?;
        let kind = {
            let mut t = w.open_table(OWN_KINDS)?;
            let mut have: HashMap<u8, crate::fixes::OwnWord> = HashMap::new();
            for e in t.iter()? {
                let (k, v) = e?;
                if let Ok(o) = serde_json::from_slice(v.value()) {
                    have.insert(k.value(), o);
                }
            }
            let found = have
                .iter()
                .find(|(_, o)| crate::search::fold(&o.word) == folded)
                .map(|(k, o)| (*k, o.clone()));
            let (kind, mut entry) = match found {
                Some(f) => f,
                None => {
                    let mut free =
                        (crate::fixes::OWN_FIRST..=u8::MAX).find(|k| !have.contains_key(k));
                    if free.is_none() {
                        // All taken: a word no correction uses any more
                        // gives up its kind.
                        let mut used = std::collections::HashSet::new();
                        for e in w.open_table(THING_FIXES)?.iter()? {
                            let (_, v) = e?;
                            let list: Vec<crate::fixes::ThingFix> =
                                serde_json::from_slice(v.value()).unwrap_or_default();
                            used.extend(list.iter().filter_map(|f| f.now));
                        }
                        free = (crate::fixes::OWN_FIRST..=u8::MAX).find(|k| !used.contains(k));
                    }
                    let Some(k) = free else {
                        return Ok(None);
                    };
                    (
                        k,
                        crate::fixes::OwnWord {
                            word: word.to_string(),
                            labels: Vec::new(),
                        },
                    )
                }
            };
            if !entry.labels.iter().any(|l| l == label) {
                entry.labels.push(label.to_string());
            }
            t.insert(kind, serde_json::to_vec(&entry)?.as_slice())?;
            kind
        };
        if let Some((hash, mut f)) = fix {
            f.now = Some(kind);
            put_thing_fix(&w, hash, Some(f), None)?;
        }
        w.commit()?;
        Ok(Some(kind))
    }

    /// Put a word about a thing found, replacing an earlier word about the
    /// same find; with `undo`, the word number `undo` is taken out instead.
    pub fn fix_thing(
        &self,
        hash: &str,
        fix: Option<crate::fixes::ThingFix>,
        undo: Option<usize>,
    ) -> Result<()> {
        let w = self.db.begin_write()?;
        put_thing_fix(&w, hash, fix, undo)?;
        w.commit()?;
        Ok(())
    }

    /// Mark a kind as never in the house, or take the mark off.
    pub fn set_kind_off(&self, kind: u8, mark: Option<&crate::fixes::Mark>) -> Result<()> {
        let w = self.db.begin_write()?;
        {
            let mut t = w.open_table(KIND_OFF)?;
            match mark {
                Some(m) => {
                    t.insert(kind, serde_json::to_vec(m)?.as_slice())?;
                }
                None => {
                    t.remove(kind)?;
                }
            }
        }
        w.commit()?;
        Ok(())
    }

    /// The things found in the live photos of `labels` that `keep` lets
    /// through, by kind: (kind, photos, the id of the photo with the
    /// surest find), most photos first. Each content counts once.
    pub fn things(
        &self,
        labels: &[String],
        keep: impl Fn(u64) -> bool,
    ) -> Result<Vec<(u8, u64, u64)>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let objects = r.open_table(OBJECTS)?;
        let fixes = self.fixes()?;
        let mut seen = std::collections::HashSet::new();
        // kind -> (photos, best score, its id)
        let mut by: HashMap<u8, (u64, f32, u64)> = HashMap::new();
        for e in items.iter()? {
            let (k, v) = e?;
            let rec = decode(v.value())?;
            let id = k.value();
            if rec.missing
                || rec.video.is_some()
                || !labels.contains(&rec.label)
                || !keep(id)
                || !seen.insert(rec.hash.clone())
            {
                continue;
            }
            let Some(raw) = objects.get(rec.hash.as_str())? else {
                continue;
            };
            let found = fixes.apply(
                &rec.hash,
                serde_json::from_slice(raw.value()).unwrap_or_default(),
            );
            let mut kinds: Vec<(u8, f32)> = Vec::new();
            for o in found {
                match kinds.iter_mut().find(|(k, _)| *k == o.kind) {
                    Some((_, s)) => *s = s.max(o.score),
                    None => kinds.push((o.kind, o.score)),
                }
            }
            for (kind, score) in kinds {
                let t = by.entry(kind).or_insert((0, 0.0, id));
                t.0 += 1;
                if score > t.1 {
                    (t.1, t.2) = (score, id);
                }
            }
        }
        let mut out: Vec<(u8, u64, u64)> =
            by.into_iter().map(|(k, (n, _, id))| (k, n, id)).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Every stored picture vector: (content hash, signed bytes).
    pub fn embeddings(&self) -> Result<Vec<(String, Vec<i8>)>> {
        let r = self.db.begin_read()?;
        let t = r.open_table(VECTORS)?;
        let mut out = Vec::new();
        for e in t.iter()? {
            let (k, v) = e?;
            out.push((
                k.value().to_string(),
                v.value().iter().map(|b| *b as i8).collect(),
            ));
        }
        Ok(out)
    }

    /// Live photos (not videos) with a thumbnail whose content was not
    /// looked at with `version` yet, newest first: (id, record).
    pub fn photos_to_look(&self, version: &str) -> Result<Vec<(u64, Record)>> {
        self.photos_not_done(LOOKED, version)
    }

    /// Like `photos_to_look`, for the faces pass.
    /// Photos in `labels` (one per content) and how many of them were
    /// looked at for faces with `version`, failures included.
    pub fn faces_counts(&self, labels: &[String], version: &str) -> Result<(u64, u64)> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let faced = r.open_table(FACED)?;
        let mut seen = std::collections::HashSet::new();
        let (mut photos, mut done) = (0, 0);
        for e in items.iter()? {
            let rec = decode(e?.1.value())?;
            if rec.missing
                || rec.video.is_some()
                || rec.thumb != "ok"
                || !labels.contains(&rec.label)
                || !seen.insert(rec.hash.clone())
            {
                continue;
            }
            photos += 1;
            if faced
                .get(rec.hash.as_str())?
                .is_some_and(|v| v.value().split(' ').next() == Some(version))
            {
                done += 1;
            }
        }
        Ok((photos, done))
    }

    pub fn photos_for_faces(&self, version: &str) -> Result<Vec<(u64, Record)>> {
        self.photos_not_done(FACED, version)
    }

    fn photos_not_done(
        &self,
        done: TableDefinition<&str, &str>,
        version: &str,
    ) -> Result<Vec<(u64, Record)>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let t = r.open_table(done)?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for e in items.iter()? {
            let (k, v) = e?;
            let rec = decode(v.value())?;
            if rec.missing
                || rec.video.is_some()
                || rec.thumb != "ok"
                || !seen.insert(rec.hash.clone())
            {
                continue;
            }
            let done = match t.get(rec.hash.as_str())? {
                Some(v) => v.value().split(' ').next() == Some(version),
                None => false,
            };
            if !done {
                out.push((k.value(), rec));
            }
        }
        out.sort_by(|a, b| b.1.taken_local.cmp(&a.1.taken_local));
        Ok(out)
    }

    /// Live photos (by content) in `labels`, how many were looked at with
    /// `version`, how many of those hold text, and how many things.
    pub fn look_counts(&self, labels: &[String], version: &str) -> Result<(u64, u64, u64, u64)> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let looked = r.open_table(LOOKED)?;
        let texts = r.open_table(TEXTS)?;
        let objects = r.open_table(OBJECTS)?;
        let mut seen = std::collections::HashSet::new();
        let (mut photos, mut done, mut with_text, mut with_things) = (0, 0, 0, 0);
        for e in items.iter()? {
            let rec = decode(e?.1.value())?;
            if rec.missing
                || rec.video.is_some()
                || rec.thumb != "ok"
                || !labels.contains(&rec.label)
                || !seen.insert(rec.hash.clone())
            {
                continue;
            }
            photos += 1;
            let h = rec.hash.as_str();
            if looked
                .get(h)?
                .is_some_and(|v| v.value().split(' ').next() == Some(version))
            {
                done += 1;
                if let Some(v) = texts.get(h)? {
                    if serde_json::from_slice::<crate::ocr::Text>(v.value())
                        .is_ok_and(|t| t.state == "ok")
                    {
                        with_text += 1;
                    }
                }
                if let Some(v) = objects.get(h)? {
                    if serde_json::from_slice::<Vec<crate::objects::Object>>(v.value())
                        .is_ok_and(|o| !o.is_empty())
                    {
                        with_things += 1;
                    }
                }
            }
        }
        Ok((photos, done, with_text, with_things))
    }

    /// Live videos whose content has no transcript from `model` yet,
    /// oldest id first: (id, record).
    /// Join every Live Photo with its clip and free clips whose photo is
    /// gone (REQ-MEDIA-005). A clip leaves the timeline; it comes back as a
    /// video of its own when it is no longer paired. Returns the pairs.
    pub fn pair_live(&self) -> Result<usize> {
        let all = self.all()?;
        let pairs = crate::live::pairs(all.iter().map(|(id, r)| (*id, r)));
        let clip_of: HashMap<u64, u64> = pairs.iter().map(|(p, c)| (*c, *p)).collect();
        {
            let b = self.batch()?;
            for (id, rec) in &all {
                let live = pairs.get(id).copied();
                let live_of = clip_of.get(id).copied();
                if rec.live == live && rec.live_of == live_of {
                    continue;
                }
                let mut new = rec.clone();
                new.live = live;
                new.live_of = live_of;
                b.store(*id, &new)?;
                if !rec.missing {
                    match (rec.live_of.is_some(), live_of.is_some()) {
                        (false, true) => b.unlink(*id, rec)?,
                        (true, false) => b.link(*id, &new)?,
                        _ => {}
                    }
                }
            }
            b.commit()?;
        }
        Ok(pairs.len())
    }

    pub fn videos_to_transcribe(&self, model: &str) -> Result<Vec<(u64, Record)>> {
        let r = self.db.begin_read()?;
        let items = r.open_table(ITEMS)?;
        let t = r.open_table(TRANSCRIPTS)?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for e in items.iter()? {
            let (k, v) = e?;
            let rec = decode(v.value())?;
            if !rec.shown() || rec.video.is_none() || !seen.insert(rec.hash.clone()) {
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
        // A Live Photo's pairing stays until the next pairing looks again.
        let mut paired = (None, None);
        let mut added = now_secs();
        let id = match existing {
            Some(id) => {
                if let Some(old) = self.load(id)? {
                    if !old.missing {
                        self.unlink(id, &old)?;
                    }
                    paired = (old.live, old.live_of);
                    added = old.added;
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
            live: paired.0,
            live_of: paired.1,
            added,
        };
        self.store(id, &rec)?;
        if rec.live_of.is_some() {
            return Ok(());
        }
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
        } else if rec.live_of.is_some() {
            Ok(())
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

/// Put a word about a thing found within the write `w` (see
/// `Index::fix_thing`).
fn put_thing_fix(
    w: &redb::WriteTransaction,
    hash: &str,
    fix: Option<crate::fixes::ThingFix>,
    undo: Option<usize>,
) -> Result<()> {
    let mut t = w.open_table(THING_FIXES)?;
    let mut v: Vec<crate::fixes::ThingFix> = match t.get(hash)? {
        Some(v) => serde_json::from_slice(v.value()).unwrap_or_default(),
        None => Vec::new(),
    };
    if let Some(i) = undo.filter(|i| *i < v.len()) {
        v.remove(i);
    }
    if let Some(f) = fix {
        crate::fixes::put(&mut v, f);
    }
    if v.is_empty() {
        t.remove(hash)?;
    } else {
        t.insert(hash, serde_json::to_vec(&v)?.as_slice())?;
    }
    Ok(())
}
