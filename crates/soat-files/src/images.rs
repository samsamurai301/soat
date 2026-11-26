//! The image pipeline: JPEG and PNG uploads get smaller copies at
//! [`WIDTHS`] (only those below the original), so pages can offer a
//! `srcset` and phones load less. Decoding is bounded (dimensions and
//! memory), runs off the async threads, and a file that does not decode
//! is still kept, just without sizes.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};
use soat_core::media::MediaType;

/// The widths made, in pixels.
pub const WIDTHS: [u32; 4] = [320, 640, 1280, 1920];
/// Largest image decoded, per side.
const MAX_SIDE: u32 = 12_000;
/// Most memory one decode may take.
const MAX_ALLOC: u64 = 512 * 1024 * 1024;

/// An image's size and its smaller copies, encoded like the original.
pub(crate) struct Sized {
    pub width: u32,
    pub height: u32,
    pub copies: Vec<(u32, Vec<u8>)>,
}

/// The storage key of the copy `width` pixels wide of the file at `key`.
pub(crate) fn size_key(key: &str, width: i32) -> String {
    match key.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/{width}w-{name}"),
        None => format!("{width}w-{key}"),
    }
}

/// Whether files of `kind` get sizes.
pub(crate) fn sizable(kind: MediaType) -> bool {
    matches!(kind.mime, "image/jpeg" | "image/png")
}

/// Decodes `bytes` and makes the smaller copies; `None` when it is no
/// image this pipeline reads (or too large to).
pub(crate) fn make(bytes: &[u8], kind: MediaType) -> Option<Sized> {
    let format = match kind.mime {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        _ => return None,
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let (width, height) = (img.width(), img.height());
    let mut copies = Vec::new();
    for w in WIDTHS.into_iter().filter(|w| *w < width) {
        let h = ((u64::from(height) * u64::from(w)) / u64::from(width)).max(1) as u32;
        let small = img.resize_exact(w, h, FilterType::Lanczos3);
        let mut out = Vec::new();
        let ok = match format {
            ImageFormat::Jpeg => small
                .to_rgb8()
                .write_with_encoder(JpegEncoder::new_with_quality(&mut out, 82))
                .is_ok(),
            _ => small
                .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
                .is_ok(),
        };
        if ok {
            copies.push((w, out));
        }
    }
    Some(Sized {
        width,
        height,
        copies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_pixel(w, h, Rgb([200, 30, 30]));
        let mut out = Vec::new();
        img.write_with_encoder(JpegEncoder::new_with_quality(&mut out, 90))
            .unwrap();
        out
    }

    #[test]
    fn makes_the_widths_below_the_original() {
        let kind = soat_core::media::detect(&jpeg(1000, 500)).unwrap();
        let s = make(&jpeg(1000, 500), kind).unwrap();
        assert_eq!((s.width, s.height), (1000, 500));
        let widths: Vec<u32> = s.copies.iter().map(|(w, _)| *w).collect();
        assert_eq!(widths, vec![320, 640]);
        let small = image::load_from_memory(&s.copies[0].1).unwrap();
        assert_eq!((small.width(), small.height()), (320, 160));
    }

    #[test]
    fn keeps_what_does_not_decode() {
        let bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR not really a picture";
        let kind = soat_core::media::detect(bytes).unwrap();
        assert!(make(bytes, kind).is_none());
        assert_eq!(size_key("t/file_1/a.jpg", 320), "t/file_1/320w-a.jpg");
    }
}
