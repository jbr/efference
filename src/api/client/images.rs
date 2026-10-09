//! Image input learned from the endpoint's refusal (docs/transport.md). A model
//! whose metadata says nothing about images is sent them; a 400 on a request that
//! carries image blocks is retried once with every image replaced by a text
//! placeholder, and when that retry succeeds the client withholds images from then
//! on. Error body text is not read: the retry is the diagnosis.

use std::sync::atomic::Ordering;

use super::*;
use crate::project::UserPart;

/// What stands in for the image blocks of one result once the client withholds
/// them. Deterministic, so every request after the lesson projects the same bytes.
fn withheld_note(count: usize) -> String {
    format!("\n\n[{count} image block(s) withheld: the endpoint refused image input]")
}

impl Client {
    /// The refusal this client, or a clone of it, learned that the endpoint
    /// refuses image input from — `None` while images are sent. Read by the agent
    /// loop, which closes the tool layer's image gate and records the lesson
    /// (docs/tools.md).
    pub fn images_refused(&self) -> Option<&str> {
        self.images_refused.get().map(String::as_str)
    }

    /// Withhold images from every later request, citing `refusal`: what a resumed
    /// session restores from its log, so a lesson recorded before a restart is not
    /// paid for twice. A second call keeps the first refusal.
    pub fn refuse_images(&self, refusal: impl Into<String>) {
        let _ = self.images_refused.set(refusal.into());
    }

    /// [`Self::send_request`], with image refusal learned on the way
    /// (docs/transport.md). The recovery runs only while no request carrying images
    /// has succeeded on this client: once one has, the model reads images, a later
    /// 400 has another cause, and stripping the history's images would move a
    /// prefix the provider has already cached.
    pub(super) async fn send(
        &self,
        turns: &[WireTurn],
        tools: &[Value],
        tool_choice: ToolChoice,
        cap: Option<u32>,
        mut on_delta: impl FnMut(Streamed),
    ) -> Result<Completion, Error> {
        if self.images_refused().is_some() {
            return self
                .send_request(&withhold_images(turns), tools, tool_choice, cap, on_delta)
                .await;
        }
        if !carries_images(turns) {
            return self
                .send_request(turns, tools, tool_choice, cap, on_delta)
                .await;
        }
        let refusal = match self
            .send_request(turns, tools, tool_choice, cap, &mut on_delta)
            .await
        {
            Err(e)
                if error_status(&e) == Some(400)
                    && !self.images_accepted.load(Ordering::Relaxed) =>
            {
                e
            }
            answered => {
                if answered.is_ok() {
                    self.images_accepted.store(true, Ordering::Relaxed);
                }
                return answered;
            }
        };
        match self
            .send_request(&withhold_images(turns), tools, tool_choice, cap, on_delta)
            .await
        {
            Ok(completion) => {
                log::warn!(
                    "{} refused a request carrying images ({refusal}); this client \
                     withholds images from here on",
                    self.model
                );
                self.refuse_images(refusal.to_string());
                Ok(completion)
            }
            Err(_) => Err(refusal),
        }
    }
}

/// Whether any tool result in `turns` carries an image block.
fn carries_images(turns: &[WireTurn]) -> bool {
    turns.iter().any(|turn| match turn {
        WireTurn::User(parts) => parts
            .iter()
            .any(|part| matches!(part, UserPart::ToolResult { images, .. } if !images.is_empty())),
        _ => false,
    })
}

/// `turns` with each result's image blocks replaced by [`withheld_note`].
fn withhold_images(turns: &[WireTurn]) -> Vec<WireTurn> {
    let mut turns = turns.to_vec();
    for turn in &mut turns {
        let WireTurn::User(parts) = turn else {
            continue;
        };
        for part in parts {
            if let UserPart::ToolResult {
                content, images, ..
            } = part
                && !images.is_empty()
            {
                content.push_str(&withheld_note(images.len()));
                images.clear();
            }
        }
    }
    turns
}
