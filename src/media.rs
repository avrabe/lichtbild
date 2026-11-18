//! File classification, metadata (capture date and position with their
//! source, SC-004) and thumbnails under decoder limits (SC-005, SSC-005).
//! Decoding is pure Rust (DD-005); video posters come from src/video.rs.

use std::io::Cursor;
use std::path::Path;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};

use crate::source::Skip;

/// What a file is, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    Media(Format),
    Skip(Skip),
}

/// The image formats lichtbild decodes. HEIC goes through heic-rs (pure
/// Rust, no unsafe, DD-010); the others through the image crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Gif,
    Heic,
    /// ISO-BMFF video: MP4, M4V, 3GP.
    Mp4,
    /// QuickTime video.
    Mov,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "jpeg",
            Format::Png => "png",
            Format::Webp => "webp",
            Format::Gif => "gif",
            Format::Heic => "heic",
            Format::Mp4 => "mp4",
            Format::Mov => "mov",
        }
    }
    /// Formats a browser cannot be relied on to display get a JPEG
    /// preview; for a video it is the poster.
    pub fn needs_preview(self) -> bool {
        matches!(self, Format::Heic | Format::Mp4 | Format::Mov)
    }
    pub fn is_video(self) -> bool {
        matches!(self, Format::Mp4 | Format::Mov)
    }
}

pub fn classify(rel: &Path) -> Class {
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    if name.starts_with('.') || matches!(lower.as_str(), "thumbs.db" | "desktop.ini") {
        return Class::Skip(Skip::HiddenOrSystem);
    }
    let ext = rel
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => Class::Media(Format::Jpeg),
        "png" => Class::Media(Format::Png),
        "webp" => Class::Media(Format::Webp),
        "gif" => Class::Media(Format::Gif),
        "heic" | "heif" => Class::Media(Format::Heic),
        "avif" => Class::Skip(Skip::NotYetSupported("avif".into())),
        "mp4" | "m4v" | "3gp" => Class::Media(Format::Mp4),
        "mov" => Class::Media(Format::Mov),
        "mkv" | "avi" | "webm" => Class::Skip(Skip::NotYetSupported("video".into())),
        "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "raf" => {
            Class::Skip(Skip::NotYetSupported("raw".into()))
        }
        "" => Class::Skip(Skip::Unsupported("(none)".into())),
        other => Class::Skip(Skip::Unsupported(sanitize_ext(other))),
    }
}

/// Keep report keys short and free of anything that could carry a name.
fn sanitize_ext(e: &str) -> String {
    let s: String = e
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if s.is_empty() {
        "(other)".into()
    } else {
        s
    }
}

/// Where a capture date came from, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSource {
    ExifOriginal,
    ExifOther,
    FileName,
    FileTime,
    /// The container's local time with its offset (Apple keys).
    Video,
    /// The container's UTC time (`mvhd`); the local offset is unknown.
    VideoUtc,
}

impl DateSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DateSource::ExifOriginal => "exif",
            DateSource::ExifOther => "exif-other",
            DateSource::FileName => "file-name",
            DateSource::FileTime => "file-time",
            DateSource::Video => "video",
            DateSource::VideoUtc => "video-utc",
        }
    }
    /// Dates not taken from EXIF are marked on the timeline (REQ-META-001).
    pub fn is_weak(s: &str) -> bool {
        !matches!(s, "exif" | "exif-other" | "video" | "video-utc")
    }
}

/// Everything the index stores about an image.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// Camera wall-clock time, "YYYY-MM-DDTHH:MM:SS".
    pub taken_local: String,
    /// "+02:00" when known.
    pub taken_offset: Option<String>,
    pub date_source: DateSource,
    pub position: Option<(f64, f64)>,
    pub hash: String,
    /// Length and codec, for a video.
    pub video: Option<crate::video::VideoMeta>,
}

/// Read dimensions, EXIF and hash from the bytes of an image file.
pub fn inspect(
    bytes: &[u8],
    format: Format,
    rel: &Path,
    mtime_ns: i64,
    max_pixels: u64,
) -> Result<ImageInfo, Skip> {
    let bad = || Skip::DecodeError(format.name().into());
    let (width, height) = if format == Format::Heic {
        // Displayed size, after rotation; reads the container, no pixels.
        let p = heic_rs::probe(bytes).map_err(|_| bad())?;
        (p.width, p.height)
    } else {
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| bad())?
            .into_dimensions()
            .map_err(|_| bad())?
    };
    if width == 0 || height == 0 {
        return Err(bad());
    }
    if u64::from(width) * u64::from(height) > max_pixels {
        return Err(Skip::OverLimit);
    }
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok();
    let (taken_local, taken_offset, date_source) = exif
        .as_ref()
        .and_then(exif_date)
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
    let position = exif.as_ref().and_then(exif_position);
    Ok(ImageInfo {
        width,
        height,
        taken_local,
        taken_offset,
        date_source,
        position,
        hash: blake3::hash(bytes).to_hex().to_string(),
        video: None,
    })
}

/// Camera, lens and exposure from EXIF, for the viewer's info panel
/// (REQ-WEB-008). Read on demand from the original; nothing is stored.
pub fn exif_details(bytes: &[u8]) -> Vec<(&'static str, String)> {
    use exif::Tag;
    let Ok(e) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) else {
        return Vec::new();
    };
    let txt = |t: Tag| exif_ascii(&e, t).filter(|s| !s.is_empty());
    let val = |t: Tag| {
        e.get_field(t, exif::In::PRIMARY)
            .map(|f| f.display_value().with_unit(&e).to_string())
            .filter(|s| !s.is_empty())
    };
    let mut out = Vec::new();
    let camera = match (txt(Tag::Make), txt(Tag::Model)) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (a, b) => a.or(b),
    };
    if let Some(c) = camera {
        out.push(("Camera", c));
    }
    if let Some(l) = txt(Tag::LensModel) {
        out.push(("Lens", l));
    }
    let exposure: Vec<String> = [
        val(Tag::ExposureTime),
        val(Tag::FNumber),
        val(Tag::PhotographicSensitivity).map(|v| format!("ISO {v}")),
        val(Tag::FocalLength),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !exposure.is_empty() {
        out.push(("Exposure", exposure.join(" · ")));
    }
    out
}

fn exif_ascii(e: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let f = e.get_field(tag, exif::In::PRIMARY)?;
    match &f.value {
        exif::Value::Ascii(v) => v
            .first()
            .map(|b| String::from_utf8_lossy(b).trim().to_string()),
        _ => None,
    }
}

fn parse_exif_datetime(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok()
}

fn valid_offset(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let ok = b.len() == 6
        && (b[0] == b'+' || b[0] == b'-')
        && b[1..3].iter().all(u8::is_ascii_digit)
        && b[3] == b':'
        && b[4..6].iter().all(u8::is_ascii_digit);
    ok.then(|| s.to_string())
}

fn exif_date(e: &exif::Exif) -> Option<(String, Option<String>, DateSource)> {
    use exif::Tag;
    let candidates = [
        (
            Tag::DateTimeOriginal,
            Tag::OffsetTimeOriginal,
            DateSource::ExifOriginal,
        ),
        (
            Tag::DateTimeDigitized,
            Tag::OffsetTimeDigitized,
            DateSource::ExifOther,
        ),
        (Tag::DateTime, Tag::OffsetTime, DateSource::ExifOther),
    ];
    candidates.iter().find_map(|(dt, off, src)| {
        let d = parse_exif_datetime(&exif_ascii(e, *dt)?)?;
        let o = exif_ascii(e, *off).and_then(|s| valid_offset(&s));
        Some((d.format("%Y-%m-%dT%H:%M:%S").to_string(), o, *src))
    })
}

fn exif_position(e: &exif::Exif) -> Option<(f64, f64)> {
    use exif::Tag;
    let coord = |tag: Tag, rtag: Tag, neg: u8| -> Option<f64> {
        let f = e.get_field(tag, exif::In::PRIMARY)?;
        let v = match &f.value {
            exif::Value::Rational(v) if v.len() == 3 => v,
            _ => return None,
        };
        if v.iter().any(|r| r.denom == 0) {
            return None;
        }
        let deg = v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0;
        let r = exif_ascii(e, rtag)?;
        Some(if r.as_bytes().first() == Some(&neg) {
            -deg
        } else {
            deg
        })
    };
    let lat = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S')?;
    let lon = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W')?;
    // SC-004: 0,0 is how a missing fix often appears; store it as missing.
    if !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
        || (lat == 0.0 && lon == 0.0)
    {
        return None;
    }
    Some((lat, lon))
}

/// A date in the file name: 8 digits YYYYMMDD (or YYYY-MM-DD), optionally
/// followed by a separator and 6 digits HHMMSS (or HH.MM.SS / HH-MM-SS).
/// Works on bytes and never slices a string, so any file name is safe.
pub fn name_date(rel: &Path) -> Option<NaiveDateTime> {
    let stem = rel.file_stem()?.as_encoded_bytes();
    let digit = |i: usize| stem.get(i).is_some_and(u8::is_ascii_digit);
    let num = |from: usize, len: usize| -> Option<u32> {
        let mut n = 0u32;
        for i in from..from + len {
            let c = *stem.get(i)?;
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + u32::from(c - b'0');
        }
        Some(n)
    };
    for start in 0..stem.len() {
        if !digit(start) || (start > 0 && digit(start - 1)) {
            continue;
        }
        // YYYYMMDD or YYYY-MM-DD
        let (y, m, d, mut i) = if (0..8).all(|k| digit(start + k)) {
            (
                num(start, 4)?,
                num(start + 4, 2)?,
                num(start + 6, 2)?,
                start + 8,
            )
        } else if stem.get(start + 4) == Some(&b'-')
            && stem.get(start + 7) == Some(&b'-')
            && [0, 1, 2, 3, 5, 6, 8, 9].iter().all(|k| digit(start + k))
        {
            (
                num(start, 4)?,
                num(start + 5, 2)?,
                num(start + 8, 2)?,
                start + 10,
            )
        } else {
            continue;
        };
        if !(1990..=2100).contains(&y) {
            continue;
        }
        let Some(date) = NaiveDate::from_ymd_opt(y as i32, m, d) else {
            continue;
        };
        while matches!(stem.get(i), Some(b'_' | b'-' | b' ' | b'T')) {
            i += 1;
        }
        // HHMMSS, or HH.MM.SS / HH-MM-SS / HH:MM:SS
        let time = if (0..6).all(|k| digit(i + k)) {
            Some((num(i, 2)?, num(i + 2, 2)?, num(i + 4, 2)?))
        } else if matches!(stem.get(i + 2), Some(b'.' | b'-' | b':'))
            && stem.get(i + 5) == stem.get(i + 2)
            && [0, 1, 3, 4, 6, 7].iter().all(|k| digit(i + k))
        {
            Some((num(i, 2)?, num(i + 3, 2)?, num(i + 6, 2)?))
        } else {
            None
        };
        if let Some(dt) = time.and_then(|(h, mi, se)| date.and_hms_opt(h, mi, se)) {
            return Some(dt);
        }
        return date.and_hms_opt(12, 0, 0);
    }
    None
}

/// Decode under limits and orient (EXIF orientation, or HEIF irot/imir).
pub fn decode(bytes: &[u8], format: Format, max_pixels: u64) -> Result<DynamicImage, Skip> {
    let bad = || Skip::DecodeError(format.name().into());
    if format == Format::Heic {
        let opts = heic_rs::DecodeOptions {
            layout: heic_rs::PixelLayout::Rgb8,
            max_pixels: Some(max_pixels),
            apply_transforms: true,
            ..heic_rs::DecodeOptions::default()
        };
        let img = heic_rs::decode(bytes, &opts).map_err(|_| bad())?;
        let rgb = image::RgbImage::from_raw(img.width, img.height, img.data).ok_or_else(bad)?;
        return Ok(DynamicImage::ImageRgb8(rgb));
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| bad())?;
    let side = u32::try_from(max_pixels.isqrt().max(1) * 4).unwrap_or(u32::MAX);
    let mut limits = Limits::default();
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some((max_pixels * 4).max(64 * 1024 * 1024) * 2);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| bad())?;
    let orientation = decoder.orientation().map_err(|_| bad())?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(|_| bad())?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Scale to fit `size` and encode as JPEG.
pub fn jpeg_fit(img: &DynamicImage, size: u32, quality: u8) -> Result<Vec<u8>, Skip> {
    let fitted = if img.width() > size || img.height() > size {
        img.thumbnail(size, size)
    } else {
        img.clone()
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&fitted.to_rgb8())
        .map_err(|_| Skip::DecodeError("jpeg-encode".into()))?;
    Ok(out)
}

/// Longest side of the JPEG preview shown instead of an original the browser
/// cannot display (HEIC).
pub const PREVIEW_SIZE: u32 = 2048;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn classify_by_extension() {
        assert_eq!(classify(Path::new("a/B.JPG")), Class::Media(Format::Jpeg));
        assert_eq!(classify(Path::new("x.HEIC")), Class::Media(Format::Heic));
        assert_eq!(classify(Path::new("x.MOV")), Class::Media(Format::Mov));
        assert_eq!(classify(Path::new("x.mp4")), Class::Media(Format::Mp4));
        assert_eq!(
            classify(Path::new("x.webm")),
            Class::Skip(Skip::NotYetSupported("video".into()))
        );
        assert_eq!(
            classify(Path::new(".DS_Store")),
            Class::Skip(Skip::HiddenOrSystem)
        );
        assert_eq!(
            classify(Path::new("._x.jpg")),
            Class::Skip(Skip::HiddenOrSystem)
        );
        assert_eq!(
            classify(Path::new("notes.txt")),
            Class::Skip(Skip::Unsupported("txt".into()))
        );
        assert_eq!(
            classify(Path::new("README")),
            Class::Skip(Skip::Unsupported("(none)".into()))
        );
    }

    #[test]
    fn dates_from_file_names() {
        let d = |s: &str| name_date(&PathBuf::from(s)).map(|d| d.to_string());
        assert_eq!(
            d("IMG_20200102_030405.jpg").as_deref(),
            Some("2020-01-02 03:04:05")
        );
        assert_eq!(
            d("PXL_20231231_235959123.jpg").as_deref(),
            Some("2023-12-31 23:59:59")
        );
        assert_eq!(
            d("2019-07-04 10.20.30.jpg").as_deref(),
            Some("2019-07-04 10:20:30")
        );
        assert_eq!(
            d("scan 20210315.png").as_deref(),
            Some("2021-03-15 12:00:00")
        );
        assert_eq!(d("IMG_1234.jpg"), None);
        assert_eq!(d("20201345_000000.jpg"), None);
        assert_eq!(d("123456789012.jpg"), None);
        // Multi-byte characters around and inside candidates never panic.
        assert_eq!(d("Urlaub 2023 Müller.jpg"), None);
        assert_eq!(d("2020010é.jpg"), None);
        assert_eq!(
            d("ü20200102_0304ö5.jpg").as_deref(),
            Some("2020-01-02 12:00:00")
        );
        assert_eq!(
            d("Ä2019-07-04 10.20.30ß.jpg").as_deref(),
            Some("2019-07-04 10:20:30")
        );
        assert_eq!(d("2021-0é-15.jpg"), None);
    }

    #[test]
    fn weak_dates() {
        assert!(!DateSource::is_weak("exif"));
        assert!(DateSource::is_weak("file-time"));
        assert!(DateSource::is_weak("file-name"));
    }
}
