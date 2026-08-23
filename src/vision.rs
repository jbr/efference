//! Image preparation for model consumption (docs/tools.md): decode → size to
//! the model's limits → encode, returning the realized bytes, their dimensions,
//! and the visual-token maths the model needs to choose resolution well.
//!
//! Sizing is the provider's own rule, ported from the vision docs' reference
//! implementation ([`resized_size`]): the constraint is a **token budget**, not
//! just a long edge — an image can fit both edge limits and still be resized.
//! Encoding happens exactly once, here, at tool-run time: the produced bytes
//! land in the log entry and are projected verbatim thereafter
//! (docs/log-and-projection.md), so codec-version drift can never move wire
//! bytes.

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};

/// The high-resolution tier's limits (Claude ≥ 4.7): max long edge, max visual
/// tokens. Hardcoded until provider awareness supplies the session model's own
/// tier; a standard-tier provider downscales further server-side, so
/// overshooting costs bytes, never correctness.
pub const HIGH_RES_MAX_EDGE: u32 = 2576;
pub const HIGH_RES_MAX_TOKENS: u32 = 4784;

/// Visual tokens for an image at `width × height`: one per 28×28-pixel patch,
/// `⌈w/28⌉ × ⌈h/28⌉`.
pub fn visual_tokens(width: u32, height: u32) -> u32 {
    width.div_ceil(28) * height.div_ceil(28)
}

/// The size the provider resizes an image to before padding: the largest
/// aspect-preserving size where each edge, rounded up to a whole 28-px patch,
/// fits `max_edge`, and the patch count fits `max_tokens`. An image already
/// inside both limits returns unchanged.
///
/// Tier limits: 2576 / 4784 (high-resolution, Claude ≥ 4.7), 1568 / 1568
/// (standard). A caller imposing a tighter long edge passes it as `max_edge`.
pub fn resized_size(width: u32, height: u32, max_edge: u32, max_tokens: u32) -> (u32, u32) {
    let fits = |w: u32, h: u32| {
        w.div_ceil(28) * 28 <= max_edge
            && h.div_ceil(28) * 28 <= max_edge
            && visual_tokens(w, h) <= max_tokens
    };
    if fits(width, height) {
        return (width, height);
    }
    if height > width {
        let (h, w) = resized_size(height, width, max_edge, max_tokens);
        return (w, h);
    }
    // Binary search along the long edge for the largest fitting size.
    let aspect = width as f64 / height as f64;
    let short = |w: u32| ((w as f64 / aspect).round() as u32).max(1);
    let (mut lo, mut hi) = (1, width); // lo always fits; hi never fits
    while lo + 1 < hi {
        let mid = lo.midpoint(hi);
        if fits(mid, short(mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo, short(lo))
}

/// A prepared image: bytes ready for a wire `image` block, at their final
/// dimensions.
#[derive(Debug)]
pub struct Prepared {
    /// The encoded bytes — the original file verbatim when nothing needed to
    /// change, a re-encode otherwise.
    pub data: Vec<u8>,
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
    /// The dimensions as decoded, before any downscale — what a result header
    /// cites so the model knows more detail exists to re-request.
    pub source_width: u32,
    pub source_height: u32,
}

impl Prepared {
    pub fn visual_tokens(&self) -> u32 {
        visual_tokens(self.width, self.height)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unrecognized image format")]
    Unrecognized,
    #[error("decoding failed: {0}")]
    Decode(image::ImageError),
    #[error("encoding failed: {0}")]
    Encode(image::ImageError),
}

/// Original bytes above this size are re-encoded even when the dimensions
/// already fit: the API caps a base64 image at 10 MB, and a passthrough is a
/// convenience, not a contract.
const PASSTHROUGH_BYTE_CAP: usize = 5 * 1024 * 1024;

/// Is this a format the API accepts as-is (JPEG / PNG / GIF / WebP)?
fn api_native(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

/// Decode `bytes`, size to [`resized_size`]`(max_edge, max_tokens)`, and encode.
///
/// - Already-fitting bytes in an API-native format pass through **verbatim**
///   (no recompression generation loss; an animated GIF keeps its animation —
///   the provider reads the first frame).
/// - Anything resized or non-native is re-encoded: JPEG stays JPEG (quality 85
///   — recompressing a photo as lossless would balloon it), everything else
///   becomes lossless WebP (screenshots and UI compress well losslessly, and
///   the pure-Rust WebP encoder is lossless-only by design).
pub fn prepare(bytes: &[u8], max_edge: u32, max_tokens: u32) -> Result<Prepared, Error> {
    let format = image::guess_format(bytes).map_err(|_| Error::Unrecognized)?;
    let decoded = image::load_from_memory(bytes).map_err(Error::Decode)?;
    let (width, height) = (decoded.width(), decoded.height());
    let (target_w, target_h) = resized_size(width, height, max_edge, max_tokens);

    if (target_w, target_h) == (width, height)
        && bytes.len() <= PASSTHROUGH_BYTE_CAP
        && let Some(media_type) = api_native(format)
    {
        return Ok(Prepared {
            data: bytes.to_vec(),
            media_type,
            width,
            height,
            source_width: width,
            source_height: height,
        });
    }

    let sized = if (target_w, target_h) == (width, height) {
        decoded
    } else {
        decoded.resize_exact(target_w, target_h, FilterType::Lanczos3)
    };

    let mut data = Vec::new();
    let media_type = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(sized.to_rgb8())
            .write_with_encoder(JpegEncoder::new_with_quality(&mut data, 85))
            .map_err(Error::Encode)?;
        "image/jpeg"
    } else {
        let sized = if sized.color().has_alpha() {
            DynamicImage::ImageRgba8(sized.to_rgba8())
        } else {
            DynamicImage::ImageRgb8(sized.to_rgb8())
        };
        sized
            .write_with_encoder(WebPEncoder::new_lossless(&mut data))
            .map_err(Error::Encode)?;
        "image/webp"
    };

    Ok(Prepared {
        data,
        media_type,
        width: target_w,
        height: target_h,
        source_width: width,
        source_height: height,
    })
}

#[cfg(test)]
mod test;
