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
use crate::vision::{self, HIGH_RES_MAX_EDGE, HIGH_RES_MAX_TOKENS};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

pub const NAME: &str = "read_image";

/// The default long-edge cap. Deliberately below the high-resolution tier's
/// native 2576: at 1568 a screenshot stays broadly legible at roughly a third
/// of the native-cap token cost, and the escalation path is stated in the tool
/// description — re-read larger when fine detail matters.
const DEFAULT_LONG_EDGE: u32 = 1568;

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
                The image is downscaled to fit `long_edge` before encoding, and the result \
                header reports the realized size and visual-token cost. Cost scales with \
                area: doubling long_edge roughly quadruples the cost. Body text is \
                usually legible at the default; small icons and dense UI chrome may need \
                a re-read at a larger long_edge (up to 2576). Sources are re-encoded to \
                lossless webp (jpeg stays jpeg) whenever downscaled or in a non-API \
                format; an image is passed through byte-identical only when it is \
                already JPEG/PNG/GIF/WebP and needs no downscale.",
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
                            Smaller is cheaper; larger resolves finer detail."
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
        let absolute = crate::policy::absolutize(path, &self.cwd);
        let bytes = match std::fs::read(&absolute) {
            Ok(bytes) => bytes,
            Err(e) => return error(format!("could not read {}: {e}", absolute.display())),
        };
        let prepared = match vision::prepare(&bytes, long_edge, HIGH_RES_MAX_TOKENS) {
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
        if (prepared.source_width, prepared.source_height) != (prepared.width, prepared.height) {
            content.push_str(&format!(
                " (downscaled {:.2}× from {}×{}; re-read with a larger long_edge for more detail)",
                prepared.width as f64 / prepared.source_width as f64,
                prepared.source_width,
                prepared.source_height
            ));
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
        }
    }
}

fn error(content: impl Into<String>) -> RawResult {
    RawResult {
        content: content.into(),
        is_error: true,
        images: Vec::new(),
    }
}

#[cfg(test)]
mod test;
