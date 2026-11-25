//! Live Photos (REQ-MEDIA-005): a photo and the short clip the phone took
//! with it are one item. The clip sits next to the photo under the same
//! name (IMG_0001.HEIC and IMG_0001.MOV), lasts a few seconds, and was
//! taken at the same moment.

use std::collections::HashMap;

use chrono::NaiveDateTime;

use crate::index::Record;
use crate::media::DateSource;

/// The longest clip that counts as a Live Photo's (phones record about 3 s).
pub const MAX_CLIP_MS: u64 = 4_000;

/// How far apart the photo's and the clip's times may be.
const SLACK_S: i64 = 10;

fn ext(rel: &[u8]) -> String {
    let name = rel.rsplit(|&b| b == b'/').next().unwrap_or(rel);
    match name.iter().rposition(|&b| b == b'.') {
        Some(i) => String::from_utf8_lossy(&name[i + 1..]).to_ascii_lowercase(),
        None => String::new(),
    }
}

/// Directory and lowercased name without extension: the pairing key.
fn stem_key(rec: &Record) -> Option<(String, Vec<u8>, Vec<u8>)> {
    let rel = &rec.rel;
    let (dir, name) = match rel.iter().rposition(|&b| b == b'/') {
        Some(i) => (rel[..i].to_vec(), &rel[i + 1..]),
        None => (Vec::new(), &rel[..]),
    };
    let dot = name.iter().rposition(|&b| b == b'.')?;
    if dot == 0 {
        return None;
    }
    Some((rec.label.clone(), dir, name[..dot].to_ascii_lowercase()))
}

fn is_photo(rec: &Record) -> bool {
    rec.video.is_none() && matches!(ext(&rec.rel).as_str(), "heic" | "heif" | "jpg" | "jpeg")
}

fn is_clip(rec: &Record) -> bool {
    rec.video
        .as_ref()
        .is_some_and(|v| v.duration_ms <= MAX_CLIP_MS)
        && matches!(ext(&rec.rel).as_str(), "mov" | "mp4")
}

fn time(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s.get(..19)?, "%Y-%m-%dT%H:%M:%S").ok()
}

/// Taken at the same moment, as far as the two dates can tell. A clip's
/// date in UTC (no local offset known) may differ by whole quarter hours.
fn same_moment(photo: &Record, clip: &Record) -> bool {
    if DateSource::is_weak(&photo.date_source) || DateSource::is_weak(&clip.date_source) {
        return true;
    }
    let (Some(a), Some(b)) = (time(&photo.taken_local), time(&clip.taken_local)) else {
        return true;
    };
    let d = (a - b).num_seconds().abs();
    if d <= SLACK_S {
        return true;
    }
    let q = d % 900;
    clip.date_source == "video-utc"
        && d <= 14 * 3600 + SLACK_S
        && (q <= SLACK_S || 900 - q <= SLACK_S)
}

/// Photo id -> clip id, for every pair among live items: exactly one photo
/// and one short clip under the same name in the same folder of one library.
pub fn pairs<'a>(items: impl IntoIterator<Item = (u64, &'a Record)>) -> HashMap<u64, u64> {
    type Key = (String, Vec<u8>, Vec<u8>);
    let mut photos: HashMap<Key, Vec<(u64, &Record)>> = HashMap::new();
    let mut clips: HashMap<Key, Vec<(u64, &Record)>> = HashMap::new();
    for (id, rec) in items {
        if rec.missing {
            continue;
        }
        let Some(key) = stem_key(rec) else { continue };
        if is_photo(rec) {
            photos.entry(key).or_default().push((id, rec));
        } else if is_clip(rec) {
            clips.entry(key).or_default().push((id, rec));
        }
    }
    let mut out = HashMap::new();
    for (key, ps) in photos {
        let Some(cs) = clips.get(&key) else { continue };
        if let ([(p, prec)], [(c, crec)]) = (ps.as_slice(), cs.as_slice()) {
            if same_moment(prec, crec) {
                out.insert(*p, *c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::VideoMeta;

    fn rec(rel: &str, taken: &str, source: &str, video_ms: Option<u64>) -> Record {
        Record {
            label: "p1".into(),
            rel: rel.as_bytes().to_vec(),
            size: 1,
            mtime_ns: 0,
            hash: rel.into(),
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
            video: video_ms.map(|duration_ms| VideoMeta {
                duration_ms,
                codec: "hevc".into(),
            }),
            live: None,
            live_of: None,
            added: 0,
        }
    }

    const T: &str = "2024-07-01T10:00:00";

    #[test]
    fn a_photo_and_its_short_clip_pair() {
        let p = rec("a/IMG_0001.HEIC", T, "exif", None);
        let c = rec(
            "a/IMG_0001.MOV",
            "2024-07-01T10:00:01",
            "video",
            Some(2_900),
        );
        let got = pairs([(1, &p), (2, &c)]);
        assert_eq!(got.get(&1), Some(&2));
    }

    #[test]
    fn a_clip_in_utc_pairs_across_the_offset() {
        let p = rec("IMG_7.jpg", T, "exif", None);
        let c = rec("img_7.mov", "2024-07-01T08:00:02", "video-utc", Some(1_500));
        assert_eq!(pairs([(1, &p), (2, &c)]).len(), 1);
    }

    #[test]
    fn long_videos_other_folders_and_other_times_do_not_pair() {
        let p = rec("a/IMG_1.HEIC", T, "exif", None);
        let long = rec("a/IMG_1.MOV", T, "video", Some(MAX_CLIP_MS + 1));
        assert!(pairs([(1, &p), (2, &long)]).is_empty());
        let elsewhere = rec("b/IMG_1.MOV", T, "video", Some(2_000));
        assert!(pairs([(1, &p), (2, &elsewhere)]).is_empty());
        let later = rec("a/IMG_1.MOV", "2024-07-01T10:05:00", "video", Some(2_000));
        assert!(pairs([(1, &p), (2, &later)]).is_empty());
        let mut other = rec("a/IMG_1.MOV", T, "video", Some(2_000));
        other.label = "p2".into();
        assert!(pairs([(1, &p), (2, &other)]).is_empty());
    }

    #[test]
    fn two_candidates_are_not_guessed_between() {
        let p = rec("a/IMG_1.HEIC", T, "exif", None);
        let j = rec("a/IMG_1.JPG", T, "exif", None);
        let c = rec("a/IMG_1.MOV", T, "video", Some(2_000));
        assert!(pairs([(1, &p), (2, &j), (3, &c)]).is_empty());
    }

    #[test]
    fn a_missing_photo_frees_its_clip() {
        let mut p = rec("a/IMG_1.HEIC", T, "exif", None);
        p.missing = true;
        let c = rec("a/IMG_1.MOV", T, "video", Some(2_000));
        assert!(pairs([(1, &p), (2, &c)]).is_empty());
    }
}
