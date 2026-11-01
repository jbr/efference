//! The Anthropic-Messages encoder: semantic turns → the `messages` array
//! (docs/log-and-projection.md).
//!
//! Everything dialect-specific about the Messages wire lives here: user-part
//! coalescing into one message, the three image placements and their interposed
//! turn, and the breakpoint carrier. The byte pins in `test.rs` fix this encoder's
//! output; a change here that moves them moves the production cache.

use super::semantic::{UserPart, WireTurn};
use super::wire::{
    ApiBlock, ApiMessage, BreakpointPlacement, CacheControl, ImageSource, ResultBlock, Role,
    place_trailing_cache_breakpoint,
};
use super::{ImagePlacement, messages_block_to_api};
use crate::record::log::{AssistantOutput, ResultImage};

/// Encode semantic turns onto the Messages wire. A [`WireTurn::Breakpoint`] marks the
/// tail block of everything encoded so far, per `breakpoint`.
pub fn encode_messages(
    turns: &[WireTurn],
    placement: ImagePlacement,
    breakpoint: BreakpointPlacement,
) -> Vec<ApiMessage> {
    let mut messages = Vec::new();
    for turn in turns {
        match turn {
            WireTurn::User(parts) => encode_user(parts, placement, breakpoint, &mut messages),
            WireTurn::Assistant(AssistantOutput::Messages(blocks)) => messages.push(ApiMessage {
                role: Role::Assistant,
                content: blocks.iter().map(messages_block_to_api).collect(),
            }),
            // Responses and Completions output have their own request dialects and
            // never enter the Messages wire (docs/transport.md). Unreachable while
            // provider routing pins one dialect per session.
            WireTurn::Assistant(
                AssistantOutput::Responses(_) | AssistantOutput::Completions(_),
            ) => {}
            WireTurn::Breakpoint => place_trailing_cache_breakpoint(&mut messages, breakpoint),
        }
    }
    messages
}

fn encode_user(
    parts: &[UserPart],
    placement: ImagePlacement,
    breakpoint: BreakpointPlacement,
    messages: &mut Vec<ApiMessage>,
) {
    let mut content: Vec<ApiBlock> = Vec::new();
    // Images held out of the user message under `AfterInterposedTurn`, with the
    // `seq` of each result they came from for the interposed turn's text. Empty
    // under every other placement, so those wires are byte-identical to before
    // this arm existed.
    let mut deferred: Vec<ApiBlock> = Vec::new();
    let mut deferred_seqs: Vec<u64> = Vec::new();
    for part in parts {
        match part {
            UserPart::Text(text) => content.push(ApiBlock::Text {
                text: text.clone(),
                cache_control: None,
            }),
            UserPart::ToolResult {
                call_id,
                seq,
                content: text,
                is_error,
                images,
            } => {
                let (blocks, held) =
                    tool_result_blocks(&call_id.0, text.clone(), *is_error, images, placement);
                content.extend(blocks);
                if !held.is_empty() {
                    deferred_seqs.extend(*seq);
                    deferred.extend(held);
                }
            }
            UserPart::Breakpoint => {
                if let Some(block) = content.last_mut()
                    && breakpoint != BreakpointPlacement::None
                {
                    *block.cache_control_mut() = Some(CacheControl::Ephemeral);
                }
            }
        }
    }
    if !content.is_empty() {
        messages.push(ApiMessage {
            role: Role::User,
            content,
        });
    }
    // Deferred images close the turn as `assistant` + `user`, never alone: the
    // interposed turn is what the placement exists for, and a user message
    // following it keeps role alternation intact.
    if !deferred.is_empty() {
        messages.push(ApiMessage {
            role: Role::Assistant,
            content: vec![ApiBlock::Text {
                text: interposed_turn(&deferred_seqs, deferred.len()),
                cache_control: None,
            }],
        });
        messages.push(ApiMessage {
            role: Role::User,
            content: deferred,
        });
    }
}

/// The assistant turn [`ImagePlacement::AfterInterposedTurn`] interposes ahead of
/// its images. Written from the deferred images' own `seq` handles, so it stays a
/// pure function of the log — and so the handle is what binds image to result,
/// which their adjacency does not do in this placement
/// (docs/log-and-projection.md).
fn interposed_turn(seqs: &[u64], images: usize) -> String {
    let handles: Vec<String> = seqs.iter().map(|seq| format!("[{seq}]")).collect();
    let noun = if images == 1 {
        "the image"
    } else {
        "the images"
    };
    format!("…receiving {noun} from {}…", handles.join(", "))
}

/// The wire blocks for one rendered result under `placement`: the blocks that
/// go in the user message now, and the image blocks held for the interposed
/// turn (non-empty only under `AfterInterposedTurn`).
fn tool_result_blocks(
    tool_use_id: &str,
    content: String,
    is_error: bool,
    images: &[ResultImage],
    placement: ImagePlacement,
) -> (Vec<ApiBlock>, Vec<ApiBlock>) {
    let source = |image: &ResultImage| ImageSource::Base64 {
        media_type: image.media_type.clone(),
        data: image.data.clone(),
    };
    match placement {
        ImagePlacement::InToolResult => {
            let content = if images.is_empty() {
                content.into()
            } else {
                let mut blocks = vec![ResultBlock::Text { text: content }];
                blocks.extend(images.iter().map(|image| ResultBlock::Image {
                    source: source(image),
                }));
                super::wire::ToolResultContent::Blocks(blocks)
            };
            (
                vec![ApiBlock::ToolResult {
                    tool_use_id: tool_use_id.to_string(),
                    content,
                    is_error,
                    cache_control: None,
                }],
                Vec::new(),
            )
        }
        ImagePlacement::FollowingUserBlock => {
            let mut blocks = vec![ApiBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.into(),
                is_error,
                cache_control: None,
            }];
            blocks.extend(images.iter().map(|image| ApiBlock::Image {
                source: source(image),
                cache_control: None,
            }));
            (blocks, Vec::new())
        }
        ImagePlacement::AfterInterposedTurn => (
            vec![ApiBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.into(),
                is_error,
                cache_control: None,
            }],
            images
                .iter()
                .map(|image| ApiBlock::Image {
                    source: source(image),
                    cache_control: None,
                })
                .collect(),
        ),
    }
}
