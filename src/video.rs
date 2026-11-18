//! Video files: metadata and a poster frame, pure Rust (REQ-MEDIA-001,
//! DD-016). Only the `moov` index and one keyframe are read, never the
//! whole file; both reads are capped (SSC-005).
//!
//! - Container: a small ISO-BMFF box walk finds `moov` and the video
//!   track; mp4-atom decodes `tkhd`, `mvhd` and `mdia` (sample tables and
//!   codec configuration).
//! - Date and position: the Apple `com.apple.quicktime.*` keys in
//!   `moov/meta` (iPhone), `moov/udta/©xyz` (Android), and `mvhd` time.
//! - Poster: the first sync sample, decoded with rust_h264 or rust_h265
//!   under a pixel limit, rotated by the track matrix.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use image::{DynamicImage, RgbImage};
use mp4_atom::{Codec, Decode, Mdia, Mvhd, Stbl, StszSamples, Tkhd};
use serde::{Deserialize, Serialize};

use crate::media::{name_date, DateSource, ImageInfo};
use crate::source::Skip;

/// Largest `moov` box read into memory.
const MOOV_MAX: u64 = 64 * 1024 * 1024;
/// Largest keyframe read into memory.
const SAMPLE_MAX: u64 = 64 * 1024 * 1024;

/// What the index keeps about a video besides the common fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoMeta {
    pub duration_ms: u64,
    /// "h264", "hevc", or the sample entry's four characters.
    pub codec: String,
}

/// The parts of the container the poster needs.
pub struct Track {
    pub meta: VideoMeta,
    /// Displayed size, after rotation.
    pub width: u32,
    pub height: u32,
    rotation: u16,
    config: Config,
    keyframe: Option<(u64, u32)>,
}

enum Config {
    H264 {
        length_size: u8,
        params: Vec<Vec<u8>>,
    },
    Hevc {
        length_size: u8,
        params: Vec<Vec<u8>>,
    },
    Other,
}

fn bad(what: &str) -> Skip {
    Skip::DecodeError(what.into())
}

/// One box: its type, the whole box (header included) and its body.
struct BoxRef<'a> {
    kind: [u8; 4],
    whole: &'a [u8],
    body: &'a [u8],
}

/// The boxes directly inside `buf`; stops at the first malformed one.
fn children(buf: &[u8]) -> Vec<BoxRef<'_>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while buf.len().saturating_sub(at) >= 8 {
        let size32 = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
        let kind = [buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]];
        let (size, header) = match size32 {
            0 => (buf.len() - at, 8),
            1 => {
                let Some(b) = buf.get(at + 8..at + 16) else {
                    break;
                };
                let s = u64::from_be_bytes(b.try_into().expect("8 bytes"));
                match usize::try_from(s) {
                    Ok(s) => (s, 16),
                    Err(_) => break,
                }
            }
            s => (s as usize, 8),
        };
        if size < header || size > buf.len() - at {
            break;
        }
        out.push(BoxRef {
            kind,
            whole: &buf[at..at + size],
            body: &buf[at + header..at + size],
        });
        at += size;
    }
    out
}

/// Read the top-level `moov` box body, seeking past everything else.
fn read_moov(f: &mut File, len: u64) -> Result<Vec<u8>, Skip> {
    let mut pos = 0u64;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos)).map_err(|_| Skip::Unreadable)?;
        let mut h = [0u8; 8];
        f.read_exact(&mut h).map_err(|_| Skip::Unreadable)?;
        let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
        let (size, header) = match size32 {
            0 => (len - pos, 8),
            1 => {
                let mut l = [0u8; 8];
                f.read_exact(&mut l).map_err(|_| bad("mp4"))?;
                (u64::from_be_bytes(l), 16)
            }
            s => (u64::from(s), 8),
        };
        if size < header || size > len - pos {
            return Err(bad("mp4"));
        }
        if &h[4..8] == b"moov" {
            let body = size - header;
            if body > MOOV_MAX {
                return Err(Skip::OverLimit);
            }
            let mut buf = vec![0u8; body as usize];
            f.read_exact(&mut buf).map_err(|_| Skip::Unreadable)?;
            return Ok(buf);
        }
        pos += size;
    }
    Err(bad("mp4"))
}

/// Rotation in degrees clockwise from the track matrix (16.16 fixed
/// point); anything but the four right angles counts as none.
fn rotation(a: i32, b: i32, c: i32, d: i32) -> u16 {
    const ONE: i32 = 1 << 16;
    match (a, b, c, d) {
        (0, ONE, n, 0) if n == -ONE => 90,
        (n, 0, 0, m) if n == -ONE && m == -ONE => 180,
        (0, n, ONE, 0) if n == -ONE => 270,
        _ => 0,
    }
}

/// File offset and size of sample `n` (1-based).
fn sample_location(stbl: &Stbl, n: u32) -> Option<(u64, u32)> {
    let size_of = |i: u32| -> Option<u32> {
        match &stbl.stsz.samples {
            StszSamples::Identical { count, size } => (i >= 1 && i <= *count).then_some(*size),
            StszSamples::Different { sizes } => sizes.get(i.checked_sub(1)? as usize).copied(),
        }
    };
    let chunk_offset = |c: u32| -> Option<u64> {
        let i = c.checked_sub(1)? as usize;
        match (&stbl.co64, &stbl.stco) {
            (Some(co), _) => co.entries.get(i).copied(),
            (None, Some(st)) => st.entries.get(i).map(|&o| u64::from(o)),
            _ => None,
        }
    };
    let entries = &stbl.stsc.entries;
    let mut first_sample = 1u32;
    for (k, e) in entries.iter().enumerate() {
        if e.samples_per_chunk == 0 {
            return None;
        }
        let next_chunk = entries.get(k + 1).map(|x| x.first_chunk);
        let chunks = match next_chunk {
            Some(nc) => nc.checked_sub(e.first_chunk)?,
            None => u32::MAX,
        };
        let samples = chunks.saturating_mul(e.samples_per_chunk);
        if n < first_sample.saturating_add(samples) {
            let idx = n - first_sample;
            let chunk = e.first_chunk + idx / e.samples_per_chunk;
            let first_in_chunk = n - idx % e.samples_per_chunk;
            let mut off = chunk_offset(chunk)?;
            for s in first_in_chunk..n {
                off = off.checked_add(u64::from(size_of(s)?))?;
            }
            return Some((off, size_of(n)?));
        }
        first_sample = first_sample.checked_add(samples)?;
    }
    None
}

/// The video track of a `moov` body.
pub fn track(moov: &[u8]) -> Result<Track, Skip> {
    let boxes = children(moov);
    let mvhd = boxes
        .iter()
        .find(|b| &b.kind == b"mvhd")
        .and_then(|b| Mvhd::decode(&mut &b.whole[..]).ok())
        .ok_or_else(|| bad("mp4"))?;
    let duration_ms = if mvhd.timescale == 0 {
        0
    } else {
        mvhd.duration.saturating_mul(1000) / u64::from(mvhd.timescale)
    };
    for t in boxes.iter().filter(|b| &b.kind == b"trak") {
        let parts = children(t.body);
        let Some(mdia) = parts
            .iter()
            .find(|b| &b.kind == b"mdia")
            .and_then(|b| Mdia::decode(&mut &b.whole[..]).ok())
        else {
            continue;
        };
        if mdia.hdlr.handler.as_ref() != b"vide" {
            continue;
        }
        let tkhd = parts
            .iter()
            .find(|b| &b.kind == b"tkhd")
            .and_then(|b| Tkhd::decode(&mut &b.whole[..]).ok());
        let stbl = &mdia.minf.stbl;
        let (codec, w, h, config) = match stbl.stsd.codecs.first() {
            Some(Codec::Avc1(a)) => {
                let mut params = a.avcc.sequence_parameter_sets.clone();
                params.extend(a.avcc.picture_parameter_sets.iter().cloned());
                let c = Config::H264 {
                    length_size: a.avcc.length_size,
                    params,
                };
                ("h264".to_string(), a.visual.width, a.visual.height, c)
            }
            Some(Codec::Hvc1(v)) => (
                "hevc".to_string(),
                v.visual.width,
                v.visual.height,
                hevc_config(&v.hvcc),
            ),
            Some(Codec::Hev1(v)) => (
                "hevc".to_string(),
                v.visual.width,
                v.visual.height,
                hevc_config(&v.hvcc),
            ),
            Some(Codec::Unknown(cc, _)) => (fourcc_name(cc.as_ref()), 0, 0, Config::Other),
            Some(other) => (codec_name(other), 0, 0, Config::Other),
            None => ("none".to_string(), 0, 0, Config::Other),
        };
        let rot = tkhd.as_ref().map_or(0, |t| {
            rotation(t.matrix.a, t.matrix.b, t.matrix.c, t.matrix.d)
        });
        // Codecs lichtbild does not decode: the track header's size, which
        // the pixel limit and the tile need as well.
        let (w, h) = match (w, h, &tkhd) {
            (0, _, Some(t)) | (_, 0, Some(t)) => {
                (u32::from(t.width.integer()), u32::from(t.height.integer()))
            }
            _ => (u32::from(w), u32::from(h)),
        };
        let (width, height) = if rot % 180 == 90 { (h, w) } else { (w, h) };
        let first_sync = stbl
            .stss
            .as_ref()
            .and_then(|s| s.entries.first().copied())
            .unwrap_or(1);
        return Ok(Track {
            meta: VideoMeta { duration_ms, codec },
            width,
            height,
            rotation: rot,
            config,
            keyframe: sample_location(stbl, first_sync),
        });
    }
    Err(bad("mp4"))
}

fn hevc_config(h: &mp4_atom::Hvcc) -> Config {
    // VPS, SPS, PPS in that order, whatever order the arrays come in.
    let mut arrays: Vec<_> = h.arrays.iter().collect();
    arrays.sort_by_key(|a| a.nal_unit_type);
    Config::Hevc {
        length_size: h.length_size_minus_one + 1,
        params: arrays
            .iter()
            .flat_map(|a| a.nalus.iter().cloned())
            .collect(),
    }
}

/// The four characters of a sample entry, safe for a report key.
fn fourcc_name(b: &[u8]) -> String {
    let s: String = b
        .iter()
        .map(|&c| c as char)
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if s.is_empty() {
        "other".into()
    } else {
        s.to_ascii_lowercase()
    }
}

fn codec_name(c: &Codec) -> String {
    match c {
        Codec::Avc3(_) => "avc3",
        Codec::Vp08(_) => "vp8",
        Codec::Vp09(_) => "vp9",
        Codec::Av01(_) => "av1",
        Codec::Jpeg(_) => "mjpeg",
        _ => "other",
    }
    .into()
}

/// Parse an ISO 6709 string such as "+10.5200+020.2700+100.000/".
pub fn iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_end_matches('/');
    let mut cuts = s
        .char_indices()
        .filter(|&(i, c)| i > 0 && (c == '+' || c == '-'))
        .map(|(i, _)| i);
    let lat_end = cuts.next()?;
    let lon_end = cuts.next().unwrap_or(s.len());
    let lat: f64 = s[..lat_end].parse().ok()?;
    let lon: f64 = s[lat_end..lon_end].parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// The Apple `mdta` keys and their string values, from a `meta` box body
/// (QuickTime writes it without the full-box header ISO uses).
fn apple_keys(meta_body: &[u8]) -> Vec<(String, String)> {
    let body = match meta_body.get(4..8) {
        Some(b"hdlr") => meta_body,
        _ => meta_body.get(4..).unwrap_or_default(),
    };
    let kids = children(body);
    let mut names = Vec::new();
    if let Some(k) = kids.iter().find(|b| &b.kind == b"keys") {
        let mut at = 8usize; // version, flags, entry count
        while let Some(h) = k.body.get(at..at + 8) {
            let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
            let Some(name) = size
                .checked_sub(8)
                .and_then(|n| k.body.get(at + 8..at + 8 + n))
            else {
                break;
            };
            names.push(String::from_utf8_lossy(name).into_owned());
            at += size;
        }
    }
    let mut out = Vec::new();
    if let Some(ilst) = kids.iter().find(|b| &b.kind == b"ilst") {
        for item in children(ilst.body) {
            let idx = u32::from_be_bytes(item.kind) as usize;
            let Some(name) = idx.checked_sub(1).and_then(|i| names.get(i)) else {
                continue;
            };
            // data box: type (4), locale (4), value; type 1 is UTF-8.
            if let Some(d) = children(item.body).into_iter().find(|b| &b.kind == b"data") {
                if d.body.get(..4) == Some(&[0, 0, 0, 1]) {
                    if let Some(v) = d.body.get(8..) {
                        out.push((name.clone(), String::from_utf8_lossy(v).into_owned()));
                    }
                }
            }
        }
    }
    out
}

/// Camera wall-clock time, its UTC offset, and where it came from.
type Taken = (String, Option<String>, DateSource);

/// Date and position from the `moov` metadata: Apple keys first, then
/// `©xyz`, then the `mvhd` time (UTC; 0 means unset).
fn metadata(moov: &[u8]) -> (Option<Taken>, Option<(f64, f64)>) {
    let boxes = children(moov);
    let udta = boxes.iter().find(|b| &b.kind == b"udta");
    let udta_kids = udta.map(|u| children(u.body)).unwrap_or_default();
    // iPhones write moov/meta; some tools moov/udta/meta.
    let keys: Vec<(String, String)> = boxes
        .iter()
        .chain(udta_kids.iter())
        .filter(|b| &b.kind == b"meta")
        .flat_map(|m| apple_keys(m.body))
        .collect();
    let key = |k: &str| keys.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim());
    let fmt = |t: &chrono::NaiveDateTime| t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let date = key("com.apple.quicktime.creationdate")
        .and_then(|s| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z").ok())
        .map(|t| {
            (
                fmt(&t.naive_local()),
                Some(t.format("%:z").to_string()),
                DateSource::Video,
            )
        })
        .or_else(|| {
            let mvhd = boxes
                .iter()
                .find(|b| &b.kind == b"mvhd")
                .and_then(|b| Mvhd::decode(&mut &b.whole[..]).ok())?;
            // Seconds since 1904-01-01; zero or before 1971 means unset.
            let unix = i64::try_from(mvhd.creation_time).ok()? - 2_082_844_800;
            let t = DateTime::<Utc>::from_timestamp(unix, 0).filter(|_| unix > 365 * 86_400)?;
            Some((
                fmt(&t.naive_utc()),
                Some("+00:00".into()),
                DateSource::VideoUtc,
            ))
        });
    let position = key("com.apple.quicktime.location.ISO6709")
        .and_then(iso6709)
        .or_else(|| {
            let xyz = udta_kids.iter().find(|b| &b.kind == b"\xa9xyz")?;
            // 2-byte length and 2-byte language before the text.
            let xyz = xyz.body.get(4..)?;
            iso6709(&String::from_utf8_lossy(xyz))
        });
    (date, position)
}

/// Inspect a video under its root: container, metadata and content hash.
/// Returns the common fields and the track for the poster.
pub fn inspect(
    f: &mut File,
    rel: &Path,
    mtime_ns: i64,
    max_pixels: u64,
) -> Result<(ImageInfo, Track), Skip> {
    let len = f.metadata().map_err(|_| Skip::Unreadable)?.len();
    let moov = read_moov(f, len)?;
    let tr = track(&moov)?;
    if u64::from(tr.width) * u64::from(tr.height) > max_pixels {
        return Err(Skip::OverLimit);
    }
    let (date, position) = metadata(&moov);
    let (taken_local, taken_offset, date_source) = date
        .or_else(|| {
            name_date(rel).map(|d| {
                (
                    d.format("%Y-%m-%dT%H:%M:%S").to_string(),
                    None,
                    DateSource::FileName,
                )
            })
        })
        .unwrap_or_else(|| {
            let t = DateTime::<Utc>::from_timestamp_nanos(mtime_ns);
            (
                t.format("%Y-%m-%dT%H:%M:%S").to_string(),
                Some("+00:00".into()),
                DateSource::FileTime,
            )
        });
    f.seek(SeekFrom::Start(0)).map_err(|_| Skip::Unreadable)?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(&mut *f)
        .map_err(|_| Skip::Unreadable)?;
    crate::source::drop_cache(f);
    Ok((
        ImageInfo {
            width: tr.width,
            height: tr.height,
            taken_local,
            taken_offset,
            date_source,
            position,
            hash: hasher.finalize().to_hex().to_string(),
            video: Some(tr.meta.clone()),
        },
        tr,
    ))
}

/// Length-prefix (4 bytes) the parameter sets and re-prefix the sample's
/// NAL units, so either decoder's length-prefixed parser takes them.
fn with_params(params: &[Vec<u8>], sample: &[u8], length_size: u8) -> Option<Vec<u8>> {
    let ls = usize::from(length_size);
    if !(1..=4).contains(&ls) {
        return None;
    }
    let mut out = Vec::with_capacity(sample.len() + 256);
    for p in params {
        out.extend_from_slice(&u32::try_from(p.len()).ok()?.to_be_bytes());
        out.extend_from_slice(p);
    }
    let mut at = 0usize;
    while at + ls <= sample.len() {
        let n = sample[at..at + ls]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        at += ls;
        let nal = sample.get(at..at.checked_add(n)?)?;
        out.extend_from_slice(&u32::try_from(n).ok()?.to_be_bytes());
        out.extend_from_slice(nal);
        at += n;
    }
    Some(out)
}

/// Planar YUV at any depth up to 16 bits.
struct Planes {
    w: usize,
    h: usize,
    cw: usize,
    ch: usize,
    depth: u8,
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
}

/// Limited-range YUV to RGB; BT.709 for HD and up, BT.601 below.
fn to_rgb(p: &Planes) -> Option<RgbImage> {
    if p.w == 0 || p.h == 0 || p.cw == 0 || p.ch == 0 {
        return None;
    }
    if p.y.len() < p.w * p.h || p.u.len() < p.cw * p.ch || p.v.len() < p.cw * p.ch {
        return None;
    }
    let shift = p.depth.saturating_sub(8);
    let (kr, kb) = if p.h >= 720 {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    let kg = 1.0 - kr - kb;
    let (sx, sy) = (p.w.div_ceil(p.cw), p.h.div_ceil(p.ch));
    let mut img = RgbImage::new(u32::try_from(p.w).ok()?, u32::try_from(p.h).ok()?);
    for (x, yy, px) in img.enumerate_pixels_mut() {
        let (x, yy) = (x as usize, yy as usize);
        let c = (yy / sy).min(p.ch - 1) * p.cw + (x / sx).min(p.cw - 1);
        let y = f32::from(p.y[yy * p.w + x] >> shift);
        let cb = f32::from(p.u[c] >> shift) - 128.0;
        let cr = f32::from(p.v[c] >> shift) - 128.0;
        let l = (y - 16.0) * 255.0 / 219.0;
        let s = 255.0 / 224.0;
        let r = l + 2.0 * (1.0 - kr) * cr * s;
        let b = l + 2.0 * (1.0 - kb) * cb * s;
        let g = (l - kr * r - kb * b) / kg;
        let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        *px = image::Rgb([q(r), q(g), q(b)]);
    }
    Some(img)
}

fn decode_h264(data: &[u8], max_pixels: u64) -> Result<Planes, Skip> {
    use rust_h264::decoder::OrderedDecoder;
    use rust_h264::nal::{parse_avcc, parse_sps_info, NalUnitType};
    let e = || bad("h264");
    let nals = parse_avcc(data, 4);
    for n in nals.iter().filter(|n| n.nal_unit_type == NalUnitType::Sps) {
        let s = parse_sps_info(n).map_err(|_| e())?;
        if u64::from(s.coded_width) * u64::from(s.coded_height) > max_pixels {
            return Err(Skip::OverLimit);
        }
    }
    let mut d = OrderedDecoder::new();
    let mut frames = Vec::new();
    for n in &nals {
        frames.extend(d.decode_nal(n).map_err(|_| e())?);
    }
    frames.extend(d.flush());
    let f = frames.into_iter().next().ok_or_else(e)?;
    let (cw, ch) = (f.chroma_width() as usize, f.chroma_height() as usize);
    let plane = |p: &[u8], n: usize| (0..n).map(|i| f.sample(p, i)).collect::<Vec<u16>>();
    let (w, h) = (f.width as usize, f.height as usize);
    Ok(Planes {
        w,
        h,
        cw,
        ch,
        depth: f.bit_depth,
        y: plane(&f.y, w * h),
        u: plane(&f.u, cw * ch),
        v: plane(&f.v, cw * ch),
    })
}

fn decode_hevc(data: &[u8], max_pixels: u64) -> Result<Planes, Skip> {
    use rust_h265::{parse_hvcc, DecodeLimits, Decoder, PixelData};
    let e = || bad("hevc");
    let limits = DecodeLimits {
        max_picture_pixels: max_pixels,
        max_picture_memory_bytes: max_pixels.saturating_mul(24).max(64 << 20),
    };
    let mut d = Decoder::with_limits(limits).with_display_order();
    let mut first = None;
    for n in parse_hvcc(data, 4) {
        if let Err(err) = d.decode_nal(&n) {
            return Err(match err {
                rust_h265::DecodeError::ResourceLimit(..) => Skip::OverLimit,
                _ => e(),
            });
        }
        if first.is_none() {
            first = d.next_output();
        }
    }
    let f = first.or_else(|| d.flush()).ok_or_else(e)?;
    let (w, h) = (f.width as usize, f.height as usize);
    let (cw, ch) = match f.chroma_format_idc {
        1 => (w.div_ceil(2), h.div_ceil(2)),
        3 => (w, h),
        _ => return Err(e()),
    };
    let wide = |p: &PixelData| match p {
        PixelData::U8(v) => v.iter().map(|&s| u16::from(s)).collect(),
        PixelData::U16(v) => v.clone(),
    };
    Ok(Planes {
        w,
        h,
        cw,
        ch,
        depth: f.bit_depth,
        y: wide(&f.y),
        u: wide(&f.u),
        v: wide(&f.v),
    })
}

/// How long the decoder child may run before it is killed.
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);
/// Header of a keyframe job on the child's stdin.
const JOB_MAGIC: &[u8; 4] = b"LBK1";

/// Decode the track's first keyframe into an upright image. The decoders
/// contain `unsafe` code, so they run in a child process (`exe
/// decode-keyframe`) under a wall-clock timeout and CPU and memory limits;
/// a crash or hang costs this one poster (DD-016, SSC-005).
pub fn poster(f: &mut File, tr: &Track, max_pixels: u64, exe: &Path) -> Result<DynamicImage, Skip> {
    let codec = tr.meta.codec.as_str();
    let (offset, size) = tr.keyframe.ok_or_else(|| bad(codec))?;
    if u64::from(size) > SAMPLE_MAX {
        return Err(Skip::OverLimit);
    }
    f.seek(SeekFrom::Start(offset))
        .map_err(|_| Skip::Unreadable)?;
    let mut sample = vec![0u8; size as usize];
    f.read_exact(&mut sample).map_err(|_| bad(codec))?;
    crate::source::drop_cache(f);
    let (kind, data) = match &tr.config {
        Config::H264 {
            length_size,
            params,
        } => (1u8, with_params(params, &sample, *length_size)),
        Config::Hevc {
            length_size,
            params,
        } => (2u8, with_params(params, &sample, *length_size)),
        Config::Other => return Err(bad(codec)),
    };
    let data = data.ok_or_else(|| bad(codec))?;
    drop(sample);
    // The child's limits follow this video's frame, not the global pixel
    // limit: twice the display size covers coded padding (16 or 64 px).
    let frame = u64::from(tr.width) * u64::from(tr.height);
    let max_pixels = frame.saturating_mul(2).max(1 << 20).min(max_pixels);
    let mut job = Vec::with_capacity(data.len() + 21);
    job.extend_from_slice(JOB_MAGIC);
    job.push(kind);
    job.extend_from_slice(&max_pixels.to_be_bytes());
    job.extend_from_slice(&(data.len() as u64).to_be_bytes());
    job.extend_from_slice(&data);
    drop(data);
    let rgb = run_child(exe, job, max_pixels, CHILD_TIMEOUT).map_err(|_| bad(codec))?;
    let img = DynamicImage::ImageRgb8(rgb);
    Ok(match tr.rotation {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    })
}

/// Run `exe decode-keyframe` on `job`; its stdout is width and height
/// (u32 big-endian) and the RGB pixels. Killed after `timeout`.
fn run_child(exe: &Path, job: Vec<u8>, max_pixels: u64, timeout: Duration) -> Result<RgbImage, ()> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .arg("decode-keyframe")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let mut stdin = child.stdin.take().ok_or(())?;
    let mut stdout = child.stdout.take().ok_or(())?;
    let writer = std::thread::spawn(move || {
        let _ = std::io::Write::write_all(&mut stdin, &job);
    });
    let cap = max_pixels.saturating_mul(3).saturating_add(8);
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let r = (&mut stdout).take(cap + 1).read_to_end(&mut out);
        r.map(|_| out)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                // Our own child, by its handle.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let _ = writer.join();
    let out = reader.join().map_err(|_| ())?.map_err(|_| ())?;
    if !status.is_some_and(|s| s.success()) || out.len() as u64 > cap || out.len() < 8 {
        return Err(());
    }
    let w = u32::from_be_bytes(out[0..4].try_into().expect("4 bytes"));
    let h = u32::from_be_bytes(out[4..8].try_into().expect("4 bytes"));
    if u64::from(w) * u64::from(h) > max_pixels {
        return Err(());
    }
    RgbImage::from_raw(w, h, out[8..].to_vec()).ok_or(())
}

/// The `decode-keyframe` child: read a job from stdin, write the decoded
/// frame to stdout. Limits its own CPU time and, on Linux, its address
/// space before decoding anything.
pub fn keyframe_child() -> Result<(), String> {
    let mut head = [0u8; 21];
    let mut stdin = std::io::stdin().lock();
    stdin.read_exact(&mut head).map_err(|e| e.to_string())?;
    if &head[..4] != JOB_MAGIC {
        return Err("not a keyframe job".into());
    }
    let kind = head[4];
    let max_pixels = u64::from_be_bytes(head[5..13].try_into().expect("8 bytes"));
    let len = u64::from_be_bytes(head[13..21].try_into().expect("8 bytes"));
    if len > SAMPLE_MAX + (1 << 20) {
        return Err("job too large".into());
    }
    limit_self(max_pixels);
    let mut data = vec![0u8; len as usize];
    stdin.read_exact(&mut data).map_err(|e| e.to_string())?;
    let planes = match kind {
        1 => decode_h264(&data, max_pixels),
        2 => decode_hevc(&data, max_pixels),
        _ => return Err("unknown codec".into()),
    }
    .map_err(|s| s.to_string())?;
    drop(data);
    let rgb = to_rgb(&planes).ok_or("bad frame")?;
    let mut out = std::io::stdout().lock();
    let w = std::io::Write::write_all(&mut out, &rgb.width().to_be_bytes())
        .and_then(|_| std::io::Write::write_all(&mut out, &rgb.height().to_be_bytes()))
        .and_then(|_| std::io::Write::write_all(&mut out, rgb.as_raw()))
        .and_then(|_| std::io::Write::flush(&mut out));
    w.map_err(|e| e.to_string())
}

/// CPU seconds for one keyframe, and on Linux the address space: the
/// frame, the decoder's picture state and the binary, with room to spare.
fn limit_self(max_pixels: u64) {
    let set = |res, v: u64| {
        let lim = libc::rlimit {
            rlim_cur: v as libc::rlim_t,
            rlim_max: v as libc::rlim_t,
        };
        // SAFETY: setrlimit(2) on this process with a valid struct.
        unsafe { libc::setrlimit(res, &lim) };
    };
    set(libc::RLIMIT_CPU, CHILD_TIMEOUT.as_secs());
    #[cfg(target_os = "linux")]
    set(
        libc::RLIMIT_AS,
        max_pixels.saturating_mul(48).saturating_add(1 << 30),
    );
    #[cfg(not(target_os = "linux"))]
    let _ = max_pixels;
}

#[cfg(test)]
mod tests {
    use super::*;

    // A decoder that hangs is killed at the timeout, not waited for.
    #[test]
    #[allow(clippy::disallowed_methods)] // writes a stand-in decoder script
    fn a_hanging_decoder_is_killed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("hang");
        std::fs::write(&exe, "#!/bin/sh\nexec sleep 20\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t = Instant::now();
        let r = run_child(&exe, b"LBK1".to_vec(), 100, Duration::from_millis(300));
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn iso6709_strings() {
        assert_eq!(iso6709("+10.5200+020.2700/"), Some((10.52, 20.27)));
        assert_eq!(
            iso6709("-33.8500+151.2100+012.000/"),
            Some((-33.85, 151.21))
        );
        assert_eq!(iso6709("+95.0+020.0/"), None);
        assert_eq!(iso6709("garbage"), None);
    }

    #[test]
    fn matrix_rotation() {
        const ONE: i32 = 1 << 16;
        assert_eq!(rotation(ONE, 0, 0, ONE), 0);
        assert_eq!(rotation(0, ONE, -ONE, 0), 90);
        assert_eq!(rotation(-ONE, 0, 0, -ONE), 180);
        assert_eq!(rotation(0, -ONE, ONE, 0), 270);
    }

    #[test]
    fn malformed_boxes_stop_the_walk() {
        // A box claiming more bytes than exist, and a size below the header.
        assert!(children(b"\0\0\0\x20moovxxxx").is_empty());
        assert!(children(b"\0\0\0\x04free").is_empty());
        let two = b"\0\0\0\x08free\0\0\0\x09moovX";
        let k: Vec<_> = children(two).iter().map(|b| b.kind).collect();
        assert_eq!(k, vec![*b"free", *b"moov"]);
    }

    #[test]
    fn nal_reprefixing_rejects_overruns() {
        let sample = [0u8, 3, 1, 2, 3];
        let out = with_params(&[vec![9]], &sample, 2).unwrap();
        assert_eq!(out, vec![0, 0, 0, 1, 9, 0, 0, 0, 3, 1, 2, 3]);
        assert_eq!(with_params(&[], &[0, 9, 1], 2), None);
        assert_eq!(with_params(&[], &[], 5), None);
    }
}
