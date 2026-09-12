//! Validate static Codex attachments before exposing them to a browser decoder.
//! Call on the bounded blocking pool, never on an async transport worker.
use image::{ImageDecoder, ImageFormat, Limits};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::{Mutex, OnceLock};

const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RASTER_BYTES: u64 = 32 * 1024 * 1024;
type ValidationCache = VecDeque<([u8; 32], &'static str)>;
static VALIDATED: OnceLock<Mutex<ValidationCache>> = OnceLock::new();

fn remember(cache: &mut ValidationCache, digest: [u8; 32], mime: &'static str) {
    if cache.iter().any(|(key, _)| *key == digest) {
        return;
    }
    if cache.len() >= 128 {
        cache.pop_front();
    }
    cache.push_back((digest, mime));
}

pub(crate) fn validate(bytes: &[u8]) -> Result<&'static str, String> {
    let invalid = || {
        "Invalid, animated or oversized image. Use static PNG, JPEG or WebP up to 4096 px per side and 32 MiB decoded.".to_string()
    };
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid());
    }
    // Cache only successful full validations, never paths or mutable metadata.
    // Store no image/raster buffers; 128 small digest records is the hard bound.
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let cache = VALIDATED.get_or_init(Default::default);
    if let Ok(cache) = cache.lock() {
        if let Some((_, mime)) = cache.iter().find(|(key, _)| *key == digest) {
            return Ok(*mime);
        }
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let reader = Cursor::new(bytes);
    let (mime, mut decoder): (_, Box<dyn ImageDecoder + '_>) = match image::guess_format(bytes)
        .map_err(|_| invalid())?
    {
        ImageFormat::Png => {
            let decoder = image::codecs::png::PngDecoder::with_limits(reader, limits.clone())
                .map_err(|_| invalid())?;
            if decoder.is_apng().map_err(|_| invalid())? {
                return Err(invalid());
            }
            ("image/png", Box::new(decoder))
        }
        ImageFormat::Jpeg => (
            "image/jpeg",
            Box::new(image::codecs::jpeg::JpegDecoder::new(reader).map_err(|_| invalid())?),
        ),
        ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(reader).map_err(|_| invalid())?;
            if decoder.has_animation() {
                return Err(invalid());
            }
            ("image/webp", Box::new(decoder))
        }
        _ => return Err(invalid()),
    };
    decoder.set_limits(limits).map_err(|_| invalid())?;
    let (width, height) = decoder.dimensions();
    // Account for the browser's RGBA raster even when the source is grayscale.
    if u64::from(width) * u64::from(height) * 4 > MAX_RASTER_BYTES
        || decoder.total_bytes() > MAX_RASTER_BYTES
    {
        return Err(invalid());
    }
    let mut pixels = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut pixels).map_err(|_| invalid())?;
    if let Ok(mut cache) = cache.lock() {
        remember(&mut cache, digest, mime);
    }
    Ok(mime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    pub(crate) const PNG: &[u8] = include_bytes!("../icons/32x32.png");

    #[test]
    fn validation_cache_has_a_hard_capacity_without_retaining_pixels() {
        let mut cache = ValidationCache::new();
        for i in 0..=128 {
            remember(&mut cache, [i as u8; 32], "image/png");
        }
        assert_eq!(cache.len(), 128);
        assert!(!cache.iter().any(|(key, _)| *key == [0; 32]));
        remember(&mut cache, [128; 32], "image/png");
        assert_eq!(cache.len(), 128);
    }

    #[test]
    fn rejects_truncated_corrupt_and_large_raster_images() {
        assert_eq!(validate(PNG).unwrap(), "image/png");
        for bytes in [
            b"\x89PNG\r\n\x1a\n".as_slice(),
            &PNG[..PNG.len() / 2],
            b"not an image",
        ] {
            assert!(validate(bytes).is_err());
        }
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(&vec![0; 4097], 4097, 1, image::ExtendedColorType::L8)
            .unwrap();
        assert!(encoded.len() < MAX_FILE_BYTES);
        assert!(
            validate(&encoded).is_err(),
            "compressed size must not bypass raster limits"
        );
        // Same dimensions are safe in grayscale source bytes but not browser RGBA.
        encoded.clear();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(
                &vec![0; 3000 * 3000],
                3000,
                3000,
                image::ExtendedColorType::L8,
            )
            .unwrap();
        assert!(validate(&encoded).is_err());
    }

    #[test]
    fn static_jpeg_and_webp_are_fully_decoded() {
        let pixels = [100u8; 12];
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .write_image(&pixels, 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert_eq!(validate(&jpeg).unwrap(), "image/jpeg");
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .write_image(&pixels, 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert_eq!(validate(&webp).unwrap(), "image/webp");
        assert!(validate(&webp[..webp.len() / 2]).is_err());
        assert!(validate(b"GIF89a").is_err());
    }

    #[test]
    #[ignore = "synthetic 4K image timing/RSS diagnostic; run explicitly"]
    fn image_validation_measurements() {
        let pixels: Vec<u8> = (0..3840 * 2160 * 3).map(|i| (i % 251) as u8).collect();
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(&pixels, 3840, 2160, image::ExtendedColorType::Rgb8)
            .unwrap();
        let rejected_oversize_bytes = encoded.len();
        assert!(rejected_oversize_bytes > MAX_FILE_BYTES);
        assert!(validate(&encoded).is_err());
        encoded.clear();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut encoded,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::NoFilter,
        )
        .write_image(&pixels, 3840, 2160, image::ExtendedColorType::Rgb8)
        .unwrap();
        drop(pixels);
        assert!(encoded.len() <= MAX_FILE_BYTES);
        let cold = std::time::Instant::now();
        assert_eq!(validate(&encoded).unwrap(), "image/png");
        let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
        let mut times = Vec::new();
        for _ in 0..20 {
            let started = std::time::Instant::now();
            assert_eq!(validate(&encoded).unwrap(), "image/png");
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({"width":3840,"height":2160,"rejectedOversizeBytes":rejected_oversize_bytes,"encodedBytes":encoded.len(),"coldMs":cold_ms,"runs":20,"warmMedianMs":times[10],"warmP95Ms":times[18],"rgbaRasterBytes":3840*2160*4})
        );
    }
}
