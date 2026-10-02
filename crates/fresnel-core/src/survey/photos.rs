//! Site photos: import, downscaled copies, and their files.
//!
//! Each imported photo becomes three files in one [`FileStore`] (prefix
//! `photo-`), written and referenced together:
//!
//! * the original, byte for byte, kept as evidence (with all its metadata,
//!   GPS position included, if the camera recorded one);
//! * a report copy, at most [`REPORT_SIDE`] px on the long side, JPEG;
//! * a thumbnail, at most [`THUMB_SIDE`] px, JPEG.
//!
//! Both copies are decoded pixels re-encoded from scratch with the EXIF
//! orientation applied, so they carry no metadata at all: a location in the
//! original never reaches a report. Formats are sniffed from the content.
//! HEIC/HEIF (and AVIF) are refused with a hint to export as JPEG: decoding
//! them needs C libraries (libheif, libde265) Fresnel doesn't ship.
//!
//! Decoding is bounded twice: the file size ([`MAX_PHOTO_BYTES`]) and the
//! decoder's own limits (image side, pixel count, allocation), so a small
//! file claiming a huge size can't exhaust memory.

use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use exif::{In, Tag, Value};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits, RgbImage};
use serde::{Deserialize, Serialize};

use super::filestore::{FileFormat, FileStore, NewFile, StoreSpec};
use super::floorplan::{JPEG, PNG};
use crate::error::{Result, WifiError};

pub const MAX_PHOTO_BYTES: usize = 25 * 1024 * 1024;
/// Long side of the copy used in reports.
pub const REPORT_SIDE: u32 = 1600;
/// Long side of the thumbnail shown in the app.
pub const THUMB_SIDE: u32 = 256;
/// Decoder limits: no side above this ...
const MAX_SIDE: u32 = 16_384;
/// ... no more pixels than this (a 108 MP phone photo is fine) ...
const MAX_PIXELS: u64 = 120_000_000;
/// ... and no more memory than this for the decoded image.
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;
const REPORT_QUALITY: u8 = 85;
const THUMB_QUALITY: u8 = 80;

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

static WEBP: FileFormat = FileFormat {
    mime: "image/webp",
    extension: "webp",
    sniff: is_webp,
};

const SPEC: StoreSpec = StoreSpec {
    prefix: "photo-",
    max_bytes: MAX_PHOTO_BYTES,
    formats: &[&JPEG, &PNG, &WEBP],
    label: "photo",
    unsupported: "unsupported photo format; use JPEG, PNG or WebP",
};

/// ISO base media files (`....ftyp`): HEIC/HEIF and AVIF photos.
fn is_isobmff_image(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp"
}

/// The three files of an imported photo and what was read from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredPhoto {
    /// The original as imported.
    pub file: String,
    /// JPEG, at most [`REPORT_SIDE`] px, no metadata.
    pub report_file: String,
    /// JPEG, at most [`THUMB_SIDE`] px, no metadata.
    pub thumb_file: String,
    /// The original's size in px, upright (EXIF orientation applied).
    pub width: u32,
    pub height: u32,
    /// EXIF `DateTimeOriginal` as `YYYY-MM-DDTHH:MM:SS`, plus the UTC offset
    /// when the camera recorded one (`OffsetTimeOriginal`); without it the
    /// time is the camera's local clock, zone unknown.
    pub taken_at: Option<String>,
    /// The original's EXIF has a GPS position. (XMP isn't inspected.)
    pub had_gps: bool,
}

/// A decoded photo's downscaled copies and metadata, before storing.
#[derive(Debug, Clone)]
pub struct ProcessedPhoto {
    pub report_jpeg: Vec<u8>,
    pub thumb_jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub taken_at: Option<String>,
    pub had_gps: bool,
}

pub struct PhotoStore {
    files: FileStore,
}

impl PhotoStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            files: FileStore::new(dir, SPEC),
        }
    }

    pub fn dir(&self) -> &Path {
        self.files.dir()
    }

    /// Validate and process a photo, store its three files, then let
    /// `reference` record them (a database row), without garbage collection
    /// in between. If `reference` fails, the files are deleted. CPU-heavy:
    /// call from a blocking thread. Decoding happens before the store's
    /// lock is taken, so a slow photo doesn't hold up other imports' GC.
    pub fn import<T>(
        &self,
        bytes: &[u8],
        reference: impl FnOnce(&StoredPhoto) -> Result<T>,
    ) -> Result<T> {
        if is_isobmff_image(bytes) {
            return Err(WifiError::InvalidInput(
                "HEIC/HEIF (and AVIF) photos aren't supported. Export the photo as JPEG \
                 (on an iPhone: Settings › Camera › Formats › Most Compatible, or share it \
                 as JPEG) and add it again."
                    .into(),
            ));
        }
        let format = self.files.check(bytes)?;
        let processed = process(bytes, format)?;
        let files = [
            NewFile { suffix: "", bytes },
            NewFile {
                suffix: "-report",
                bytes: &processed.report_jpeg,
            },
            NewFile {
                suffix: "-thumb",
                bytes: &processed.thumb_jpeg,
            },
        ];
        self.files.import(&files, |stored| {
            let photo = StoredPhoto {
                file: stored[0].name.clone(),
                report_file: stored[1].name.clone(),
                thumb_file: stored[2].name.clone(),
                width: processed.width,
                height: processed.height,
                taken_at: processed.taken_at.clone(),
                had_gps: processed.had_gps,
            };
            Ok((reference(&photo)?, Vec::new()))
        })
    }

    pub fn read(&self, file: &str) -> Result<Vec<u8>> {
        self.files.read(file)
    }

    /// Delete photo files no row references (see
    /// [`FileStore::collect_garbage`]); `referenced` must list all three
    /// files of every photo row.
    pub fn collect_garbage(
        &self,
        referenced: impl FnOnce() -> Result<HashSet<String>>,
    ) -> Result<usize> {
        self.files.collect_garbage(referenced)
    }
}

/// Decode, read EXIF, orient and downscale. `format` is the sniffed one.
pub fn process(bytes: &[u8], format: &FileFormat) -> Result<ProcessedPhoto> {
    let image_format = match format.extension {
        "jpg" => ImageFormat::Jpeg,
        "png" => ImageFormat::Png,
        "webp" => ImageFormat::WebP,
        other => {
            return Err(WifiError::InvalidInput(format!(
                "unsupported photo format ({other})"
            )))
        }
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image_format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(image_error)?;
    let (w, h) = decoder.dimensions();
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(too_large(w, h));
    }
    let exif_chunk = decoder.exif_metadata().map_err(image_error)?;
    let exif = exif_chunk.as_deref().and_then(parse_exif);
    let orientation = exif_chunk
        .as_deref()
        .map(strip_exif_header)
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);

    let image = DynamicImage::from_decoder(decoder).map_err(image_error)?;
    // Downscale before orienting (cheaper; a square bound makes the order
    // irrelevant). A fast box filter first takes huge photos to twice the
    // target, then a proper filter does the rest.
    let image = if image.width().max(image.height()) > REPORT_SIDE * 2 {
        image.thumbnail(REPORT_SIDE * 2, REPORT_SIDE * 2)
    } else {
        image
    };
    let mut report = if image.width().max(image.height()) > REPORT_SIDE {
        image.resize(REPORT_SIDE, REPORT_SIDE, FilterType::Lanczos3)
    } else {
        image
    };
    report.apply_orientation(orientation);
    let report = flatten(report);
    let thumb = DynamicImage::ImageRgb8(report.clone()).resize(
        THUMB_SIDE,
        THUMB_SIDE,
        FilterType::Triangle,
    );

    let swaps = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let (width, height) = if swaps { (h, w) } else { (w, h) };
    Ok(ProcessedPhoto {
        report_jpeg: encode_jpeg(&report, REPORT_QUALITY)?,
        thumb_jpeg: encode_jpeg(&thumb.to_rgb8(), THUMB_QUALITY)?,
        width,
        height,
        taken_at: exif.as_ref().and_then(taken_at),
        had_gps: exif.as_ref().is_some_and(has_gps),
    })
}

/// RGB with any transparency composited onto white (JPEG has no alpha, and
/// dropping it would turn transparent areas black).
fn flatten(image: DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let a = u32::from(a);
        let over_white = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        image::Rgb([over_white(r), over_white(g), over_white(b)])
    })
}

fn encode_jpeg(image: &RgbImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(image)
        .map_err(|e| WifiError::Backend(format!("cannot encode JPEG: {e}")))?;
    Ok(out)
}

fn too_large(w: u32, h: u32) -> WifiError {
    WifiError::InvalidInput(format!(
        "the photo is {w} × {h} px; photos can be at most {MAX_SIDE} px on a side and {} MP. \
         Use the camera's normal resolution or scale it down.",
        MAX_PIXELS / 1_000_000
    ))
}

fn image_error(e: ImageError) -> WifiError {
    match e {
        ImageError::Limits(e) => WifiError::InvalidInput(format!(
            "the photo is too large to process ({e}); photos can be at most {MAX_SIDE} px on a \
             side and {} MP",
            MAX_PIXELS / 1_000_000
        )),
        ImageError::Decoding(e) => {
            WifiError::InvalidInput(format!("the photo could not be read: {e}"))
        }
        ImageError::Unsupported(e) => {
            WifiError::InvalidInput(format!("this kind of photo isn't supported: {e}"))
        }
        e => WifiError::Backend(format!("cannot process the photo: {e}")),
    }
}

/// Some writers (WebP in particular) keep JPEG's `Exif\0\0` marker in front
/// of the TIFF data.
fn strip_exif_header(chunk: &[u8]) -> &[u8] {
    chunk.strip_prefix(b"Exif\0\0").unwrap_or(chunk)
}

/// Lenient: a damaged EXIF block only loses the fields it can't parse.
fn parse_exif(chunk: &[u8]) -> Option<exif::Exif> {
    let mut reader = exif::Reader::new();
    reader.continue_on_error(true);
    match reader.read_raw(strip_exif_header(chunk).to_vec()) {
        Ok(exif) => Some(exif),
        Err(e) => e.distill_partial_result(|_| {}).ok(),
    }
}

fn ascii(exif: &exif::Exif, tag: Tag) -> Option<&[u8]> {
    match &exif.get_field(tag, In::PRIMARY)?.value {
        Value::Ascii(v) => v.first().map(Vec::as_slice),
        _ => None,
    }
}

fn taken_at(exif: &exif::Exif) -> Option<String> {
    let mut dt = exif::DateTime::from_ascii(ascii(exif, Tag::DateTimeOriginal)?).ok()?;
    NaiveDate::from_ymd_opt(i32::from(dt.year), dt.month.into(), dt.day.into())?.and_hms_opt(
        dt.hour.into(),
        dt.minute.into(),
        dt.second.into(),
    )?;
    let mut text = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
    );
    if let Some(offset) = ascii(exif, Tag::OffsetTimeOriginal) {
        if dt.parse_offset(offset).is_ok() {
            if let Some(minutes) = dt.offset {
                let sign = if minutes < 0 { '-' } else { '+' };
                let m = minutes.unsigned_abs();
                text.push_str(&format!("{sign}{:02}:{:02}", m / 60, m % 60));
            }
        }
    }
    Some(text)
}

fn has_gps(exif: &exif::Exif) -> bool {
    exif.get_field(Tag::GPSLatitude, In::PRIMARY).is_some()
        || exif.get_field(Tag::GPSLongitude, In::PRIMARY).is_some()
}

#[cfg(test)]
pub(crate) mod tests {
    use exif::experimental::Writer;
    use exif::{Field, Rational};
    use image::codecs::png::PngEncoder;
    use image::{ImageEncoder, Rgb, Rgba, RgbaImage};

    use super::*;

    /// A JPEG of `w`×`h` px: left half red, right half blue.
    pub(crate) fn test_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, _| {
            if x < w / 2 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 255])
            }
        });
        encode_jpeg(&img, 95).unwrap()
    }

    /// EXIF (TIFF) data with the given orientation, optional GPS and time.
    fn exif_block(orientation: u16, gps: bool, taken: Option<(&str, &str)>) -> Vec<u8> {
        let mut fields = vec![Field {
            tag: Tag::Orientation,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![orientation]),
        }];
        if gps {
            let dms = |d| {
                Value::Rational(vec![
                    Rational::from((d, 1)),
                    Rational::from((0, 1)),
                    Rational::from((0, 1)),
                ])
            };
            fields.push(Field {
                tag: Tag::GPSLatitude,
                ifd_num: In::PRIMARY,
                value: dms(52),
            });
            fields.push(Field {
                tag: Tag::GPSLongitude,
                ifd_num: In::PRIMARY,
                value: dms(4),
            });
        }
        if let Some((dt, offset)) = taken {
            fields.push(Field {
                tag: Tag::DateTimeOriginal,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![dt.as_bytes().to_vec()]),
            });
            fields.push(Field {
                tag: Tag::OffsetTimeOriginal,
                ifd_num: In::PRIMARY,
                value: Value::Ascii(vec![offset.as_bytes().to_vec()]),
            });
        }
        let mut writer = Writer::new();
        for f in &fields {
            writer.push_field(f);
        }
        let mut buf = Cursor::new(Vec::new());
        writer.write(&mut buf, false).unwrap();
        buf.into_inner()
    }

    /// Insert an APP1 EXIF segment right after the JPEG's SOI marker.
    fn with_exif(jpeg: &[u8], tiff: &[u8]) -> Vec<u8> {
        let len = (2 + 6 + tiff.len()) as u16;
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(tiff);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn decode(jpeg: &[u8]) -> RgbImage {
        image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg)
            .unwrap()
            .to_rgb8()
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn sniffs_photo_formats() {
        assert!(is_webp(b"RIFF\x10\0\0\0WEBPVP8 "));
        assert!(!is_webp(b"RIFF\x10\0\0\0WAVEfmt "));
        assert!(is_isobmff_image(b"\0\0\0\x18ftypheic\0\0\0\0"));
        assert!(is_isobmff_image(b"\0\0\0\x1cftypavif\0\0\0\0"));
        assert!(!is_isobmff_image(&test_jpeg(4, 4)));
    }

    #[test]
    fn applies_orientation_and_strips_metadata() {
        let tiff = exif_block(6, true, Some(("2026:09:30 14:05:09", "+02:00")));
        let jpeg = with_exif(&test_jpeg(400, 200), &tiff);
        let p = process(&jpeg, &JPEG).unwrap();
        // Orientation 6 = rotate 90° clockwise: 400×200 becomes 200×400 and
        // the red left half ends up on top.
        assert_eq!((p.width, p.height), (200, 400));
        let report = decode(&p.report_jpeg);
        assert_eq!(report.dimensions(), (200, 400));
        let top = report.get_pixel(100, 50).0;
        let bottom = report.get_pixel(100, 350).0;
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
        assert_eq!(decode(&p.thumb_jpeg).dimensions(), (128, 256));

        assert!(p.had_gps);
        assert_eq!(p.taken_at.as_deref(), Some("2026-09-30T14:05:09+02:00"));
        // Re-encoded from pixels: no EXIF (so no GPS) in either copy.
        for copy in [&p.report_jpeg, &p.thumb_jpeg] {
            assert!(!contains(copy, b"Exif"));
            assert!(parse_exif(copy).is_none() || !has_gps(&parse_exif(copy).unwrap()));
        }
    }

    #[test]
    fn downscales_large_photos_and_keeps_small_ones() {
        let p = process(&test_jpeg(4000, 1000), &JPEG).unwrap();
        assert_eq!((p.width, p.height), (4000, 1000));
        assert_eq!(decode(&p.report_jpeg).dimensions(), (1600, 400));
        assert_eq!(decode(&p.thumb_jpeg).dimensions(), (256, 64));
        assert!(!p.had_gps);
        assert_eq!(p.taken_at, None);

        let p = process(&test_jpeg(300, 200), &JPEG).unwrap();
        assert_eq!(decode(&p.report_jpeg).dimensions(), (300, 200));

        // Without an offset the time stays zone-less.
        let tiff = exif_block(1, false, Some(("2026:01:02 03:04:05", "bogus")));
        let p = process(&with_exif(&test_jpeg(64, 64), &tiff), &JPEG).unwrap();
        assert_eq!(p.taken_at.as_deref(), Some("2026-01-02T03:04:05"));
        assert!(!p.had_gps);
    }

    #[test]
    fn transparent_png_goes_onto_white() {
        let img = RgbaImage::from_pixel(32, 16, Rgba([0, 0, 0, 0]));
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(img.as_raw(), 32, 16, image::ExtendedColorType::Rgba8)
            .unwrap();
        let p = process(&png, &PNG).unwrap();
        let px = decode(&p.report_jpeg).get_pixel(5, 5).0;
        assert!(px.iter().all(|&c| c > 245), "{px:?}");
    }

    #[test]
    fn decoder_limits_refuse_huge_images() {
        // A tiny file that decodes to 1 × 20 000 px.
        let img = RgbImage::from_pixel(1, MAX_SIDE + 1, Rgb([1, 2, 3]));
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(
                img.as_raw(),
                1,
                MAX_SIDE + 1,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        let err = process(&png, &PNG).unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
        assert!(err.to_string().contains("too large"), "{err}");

        let err = process(b"\xFF\xD8\xFF\xE0garbage", &JPEG).unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    fn temp_store(name: &str) -> (PathBuf, PhotoStore) {
        let dir =
            std::env::temp_dir().join(format!("fresnel-photos-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), PhotoStore::new(dir))
    }

    #[test]
    fn import_stores_three_files_and_refuses_heic() {
        let (dir, store) = temp_store("import");
        let original = test_jpeg(100, 50);
        let photo = store.import(&original, |p| Ok(p.clone())).unwrap();
        assert_eq!(store.read(&photo.file).unwrap(), original);
        let stem = photo.file.trim_end_matches(".jpg");
        assert_eq!(photo.report_file, format!("{stem}-report.jpg"));
        assert_eq!(photo.thumb_file, format!("{stem}-thumb.jpg"));
        assert!(store.read(&photo.thumb_file).is_ok());

        let err = store
            .import(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic", |_| Ok(()))
            .unwrap_err();
        assert!(
            err.to_string().contains("export the photo as JPEG")
                || err.to_string().contains("Export the photo as JPEG"),
            "{err}"
        );
        let err = store.import(b"GIF89a....", |_| Ok(())).unwrap_err();
        assert!(err.to_string().contains("JPEG, PNG or WebP"), "{err}");
        let big = vec![0xFFu8; MAX_PHOTO_BYTES + 1];
        let err = store.import(&big, |_| Ok(())).unwrap_err();
        assert!(err.to_string().contains("limit"), "{err}");

        // Reference fails: none of the three files stay.
        let mut attempted = None;
        let err = store.import(&original, |p| -> Result<()> {
            attempted = Some(p.clone());
            Err(WifiError::InvalidInput("point is gone".into()))
        });
        assert!(err.is_err());
        let a = attempted.unwrap();
        for f in [&a.file, &a.report_file, &a.thumb_file] {
            assert!(store.read(f).is_err());
        }

        // GC keeps all three files of a referenced photo.
        let referenced: HashSet<String> = [
            photo.file.clone(),
            photo.report_file.clone(),
            photo.thumb_file.clone(),
        ]
        .into();
        let other = store.import(&original, |p| Ok(p.clone())).unwrap();
        assert_eq!(store.collect_garbage(|| Ok(referenced)).unwrap(), 3);
        assert!(store.read(&photo.report_file).is_ok());
        assert!(store.read(&other.file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
