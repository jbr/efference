use super::{Control, Executor, ImageResult};
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Fetch an image the session actually saw: the bytes stored on a logged tool
/// result, not a re-read of the file they came from, which may since have changed.
/// `seq` is the log entry whose result carried the image — `log` reports it. Over
/// MCP the image comes back as an image content block; at a terminal it is written
/// to a file.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "image")]
pub struct Image {
    /// The `seq` of the log entry whose result carried the image.
    pub seq: u64,
    /// Which image on that result, when it carried several.
    #[serde(default)]
    #[arg(default_value_t = 0)]
    pub idx: usize,
    /// Where to write the bytes. Command-line only; over MCP the image is returned
    /// inline and nothing is written. Defaults to `image-<seq>-<idx>.<ext>` in the
    /// current directory.
    #[serde(skip)]
    #[schemars(skip)]
    #[arg(long, short)]
    pub out: Option<std::path::PathBuf>,
    /// Read from this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Image {
    fn title() -> Option<&'static str> {
        Some("Fetch a result image")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            // Read-only against the session. The command-line path writes the bytes
            // to a file the caller named, which is the caller's own act.
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Image {
    type Output = ImageResult;

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<ImageResult> {
        let cli = control.is_cli();
        let bytes = match &mut control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref())?;
                state.block_on(client.log_image(self.seq, self.idx))?
            }
            Executor::Host(state) => {
                let session = state.resolve(self.session.as_deref())?;
                session
                    .image_bytes(self.seq, self.idx)
                    .map(|(_media_type, bytes)| bytes)
                    .ok_or_else(|| anyhow!("no image at seq {} index {}", self.seq, self.idx))?
            }
        };
        // The encoding comes from the bytes rather than from a media type the caller
        // would have to re-request: the image was realized into one of a handful of
        // encodings, and guessing wrong names a file no viewer opens.
        let format = image::guess_format(&bytes).ok();
        if !cli {
            return Ok(ImageResult::inline(&bytes, format));
        }
        let extension = format
            .and_then(|f| f.extensions_str().first().copied())
            .unwrap_or("bin");
        let path = self.out.unwrap_or_else(|| {
            std::path::PathBuf::from(format!("image-{}-{}.{extension}", self.seq, self.idx))
        });
        std::fs::write(&path, &bytes)?;
        Ok(ImageResult::Wrote {
            path: path.display().to_string(),
            bytes: bytes.len(),
        })
    }
}
