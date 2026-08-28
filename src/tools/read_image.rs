//! `read_image`: pull an on-disk image into context as a `tool_result` image
//! block (docs/tools.md). The thin tool half — reading, sizing knobs, and the
//! result header — over [`crate::vision`], which owns decode/size/encode.
//!
//! Pull-based on purpose: an image enters the transcript only when the model
//! asks, at the resolution it asks for, and the header reports the realized
//! visual-token cost so the next ask is better calibrated.

use serde_json::{Value, json};
use std::path::PathBuf;

use crate::log::{RawResult, ResultImage};
use crate::vision::{
    self, Crop, DEFAULT_LONG_EDGE, HIGH_RES_MAX_EDGE, HIGH_RES_MAX_TOKENS, resized_size,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

pub const NAME: &str = "read_image";

/// One 28-px patch is the smallest meaningful image; a lower `long_edge` is
/// clamped up to it rather than erroring.
const MIN_LONG_EDGE: u32 = 28;

pub struct ReadImage {
    /// Session cwd, for resolving a relative `path` — the same base the fs
    /// tools resolve against.
    pub cwd: PathBuf,
}

impl ReadImage {
    pub fn definition() -> Value {
        json!({
            "name": NAME,
            "description": "Read an image file into context as an actual image you can see. \
                Two limits bound the realized size, whichever binds first: `long_edge`, \
                and the provider's visual-token budget of 4784 tokens (about 1930×1930). \
                The result header reports the realized size, the visual-token cost, and \
                which of the two bound. Cost scales with area: doubling long_edge roughly \
                quadruples the cost. Body text is usually legible at the default; small \
                icons and dense UI chrome may need a re-read at a larger long_edge (up to \
                2576). When the budget bound, no larger long_edge can help — pass `region` \
                to read a rectangle of the source instead: the crop happens before the \
                downscale, so a region that fits the budget is read at 1:1 source \
                resolution, which is sharper than any whole-frame read and usually \
                cheaper. Sources are re-encoded to lossless webp (jpeg stays jpeg) \
                whenever cropped, downscaled, or in a non-API format; an image is passed \
                through byte-identical only when it is already JPEG/PNG/GIF/WebP and \
                needs neither.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the image file."
                    },
                    "long_edge": {
                        "type": "integer",
                        "description": "Cap on the longer edge in pixels (default 1568, max 2576). \
                            Smaller is cheaper; larger resolves finer detail, up to the \
                            visual-token budget."
                    },
                    "region": {
                        "type": "object",
                        "description": "Read only this rectangle of the source, in source pixels \
                            (the header reports the source dimensions to address into, and echoes \
                            the region's origin so coordinates in the crop can be offset back to \
                            the full frame). Cropped before the long_edge/budget downscale. A \
                            region extending past the frame is clamped and the header says so.",
                        "properties": {
                            "x": { "type": "integer", "description": "Left edge, in source pixels." },
                            "y": { "type": "integer", "description": "Top edge, in source pixels." },
                            "w": { "type": "integer", "description": "Width, in source pixels." },
                            "h": { "type": "integer", "description": "Height, in source pixels." }
                        },
                        "required": ["x", "y", "w", "h"]
                    }
                },
                "required": ["path"]
            }
        })
    }

    pub fn run(&self, input: &Value) -> RawResult {
        let Some(path) = input.get("path").and_then(Value::as_str) else {
            return error("read_image requires a string `path`");
        };
        let long_edge = match input.get("long_edge") {
            None => DEFAULT_LONG_EDGE,
            Some(v) => match v.as_u64() {
                Some(n) => (n as u32).clamp(MIN_LONG_EDGE, HIGH_RES_MAX_EDGE),
                None => return error("`long_edge` must be an integer"),
            },
        };
        let region = match parse_region(input) {
            Ok(region) => region,
            Err(message) => return error(message),
        };
        let absolute = crate::policy::absolutize(path, &self.cwd);
        let bytes = match std::fs::read(&absolute) {
            Ok(bytes) => bytes,
            Err(e) => return error(format!("could not read {}: {e}", absolute.display())),
        };
        let prepared = match vision::prepare_region(&bytes, region, long_edge, HIGH_RES_MAX_TOKENS)
        {
            Ok(prepared) => prepared,
            Err(e) => return error(format!("could not prepare {}: {e}", absolute.display())),
        };

        let mut content = format!(
            "{}×{} {}, {} visual tokens",
            prepared.width,
            prepared.height,
            prepared.media_type,
            prepared.visual_tokens()
        );

        // The parenthetical is composed from up to two clauses: what rectangle
        // was read, and — when the realized size is smaller than what was read
        // — which limit bound it, since that is what says whether a re-read at
        // a larger `long_edge` can gain anything (docs/tools.md).
        let mut clauses: Vec<String> = Vec::new();
        if let Some(crop) = prepared.crop {
            let mut clause = format!(
                "region {}×{} at {},{} of {}×{}",
                crop.width,
                crop.height,
                crop.x,
                crop.y,
                prepared.source_width,
                prepared.source_height
            );
            if region != Some(crop) {
                clause.push_str(", clamped to the frame");
            }
            clauses.push(clause);
        }
        let (read_w, read_h) = prepared.pre_scale_size();
        if (read_w, read_h) != (prepared.width, prepared.height) {
            let scale = prepared.width as f64 / read_w as f64;
            // With a region clause present, the pre-scale dimensions are already
            // stated there.
            let from = match prepared.crop {
                Some(_) => String::new(),
                None => format!(" from {read_w}×{read_h}"),
            };
            let escalation = match prepared.crop {
                Some(_) => "read a smaller region for more detail",
                None => "read a region for more detail",
            };
            clauses.push(match constraint_that_bound(read_w, read_h, long_edge) {
                Bound::LongEdge => format!(
                    "downscaled {scale:.2}×{from}; re-read with a larger long_edge for more detail"
                ),
                Bound::TokenBudget => format!(
                    "downscaled {scale:.2}×{from} by the visual-token budget \
                     ({HIGH_RES_MAX_TOKENS} tokens); this is the maximum resolution for \
                     this rectangle, no long_edge beats the cap — {escalation}"
                ),
                Bound::EdgeCeiling => format!(
                    "downscaled {scale:.2}×{from} by the {HIGH_RES_MAX_EDGE}-px long_edge \
                     ceiling; this is the maximum resolution for this rectangle — {escalation}"
                ),
            });
        } else if prepared.crop.is_some() {
            clauses.push("read 1:1".to_string());
        }
        if !clauses.is_empty() {
            content.push_str(&format!(" ({})", clauses.join("; ")));
        }

        RawResult {
            content,
            is_error: false,
            images: vec![ResultImage {
                media_type: prepared.media_type.to_string(),
                data: STANDARD.encode(&prepared.data),
                width: prepared.width,
                height: prepared.height,
            }],
            content_elided: None,
        }
    }
}

/// Which limit produced the realized size for a `read_w × read_h` rectangle
/// asked for at `long_edge` — the difference the header reports, because only
/// `LongEdge` leaves a larger `long_edge` worth asking for.
#[derive(Debug, PartialEq, Eq)]
enum Bound {
    /// The requested `long_edge`: raising it yields a bigger image.
    LongEdge,
    /// The visual-token budget: no `long_edge` can beat it.
    TokenBudget,
    /// The tier's own 2576-px edge ceiling, with tokens to spare (a very wide
    /// or very tall rectangle).
    EdgeCeiling,
}

/// Derive [`Bound`] by re-running the sizing rule with one limit relaxed at a
/// time: if the size at the maximum `long_edge` is the size we got, the request's
/// own `long_edge` was not what bound; then dropping the token cap tells the two
/// ceiling limits apart.
fn constraint_that_bound(read_w: u32, read_h: u32, long_edge: u32) -> Bound {
    let realized = resized_size(read_w, read_h, long_edge, HIGH_RES_MAX_TOKENS);
    let at_max_edge = resized_size(read_w, read_h, HIGH_RES_MAX_EDGE, HIGH_RES_MAX_TOKENS);
    if at_max_edge != realized {
        Bound::LongEdge
    } else if resized_size(read_w, read_h, HIGH_RES_MAX_EDGE, u32::MAX) != at_max_edge {
        Bound::TokenBudget
    } else {
        Bound::EdgeCeiling
    }
}

/// Read the optional `region` object. Shape failures are the model's request
/// being incoherent rather than the frame disagreeing with it, so they are
/// errors here; the geometry against the actual frame is settled in
/// [`vision::Crop::clamped_to`] (docs/tools.md).
fn parse_region(input: &Value) -> Result<Option<Crop>, String> {
    let Some(region) = input.get("region") else {
        return Ok(None);
    };
    let Some(object) = region.as_object() else {
        return Err("`region` must be an object with integer `x`, `y`, `w`, `h`".to_string());
    };
    let field = |name: &str| match object.get(name).and_then(Value::as_i64) {
        None => Err(format!("`region.{name}` must be an integer")),
        Some(n) if n < 0 => Err(format!(
            "`region.{name}` must not be negative — regions are in source pixels"
        )),
        // Beyond u32 is beyond any frame; clamping keeps it a geometry question
        // that `clamped_to` answers against the real dimensions.
        Some(n) => Ok(n.min(u32::MAX as i64) as u32),
    };
    let (x, y, width, height) = (field("x")?, field("y")?, field("w")?, field("h")?);
    if width == 0 || height == 0 {
        return Err("`region` has zero area: `w` and `h` must be at least 1".to_string());
    }
    Ok(Some(Crop {
        x,
        y,
        width,
        height,
    }))
}

fn error(content: impl Into<String>) -> RawResult {
    RawResult {
        content: content.into(),
        is_error: true,
        images: Vec::new(),
        content_elided: None,
    }
}

#[cfg(test)]
mod test;
