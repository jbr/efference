//! The session's assembled tool table and the dispatch of one call through it
//! (docs/tools.md).

use super::*;

impl Toolbox {
    pub fn new(providers: Vec<Provider>) -> Self {
        Self {
            providers,
            child_env: crate::env_table::ChildEnv::default(),
            image_input: true,
        }
    }

    /// Record whether this session's model accepts images
    /// (`ModelCapabilities::supports_image_input`). Separate from construction
    /// for the same reason as [`Self::with_bash_limits`]: only a served session
    /// has run the boot capability lookup.
    pub fn with_image_input(mut self, image_input: bool) -> Self {
        self.image_input = image_input;
        self
    }

    /// Whether image-producing tools attach their images (docs/tools.md).
    pub fn image_input(&self) -> bool {
        self.image_input
    }

    /// Close the image gate mid-session: the client learned that the endpoint
    /// refuses image input (docs/transport.md). The gate does not shape `tools[]`,
    /// so this moves nothing in the cached prefix.
    pub fn refuse_images(&mut self) {
        self.image_input = false;
    }

    /// Just bash — no MCP mounts, for tests and the dev views.
    pub fn bash_only() -> Self {
        Self::new(vec![Provider::Bash(Bash::default())])
    }

    /// Apply the `[env]` table's spawn policy to the mounted `bash` provider
    /// (docs/env.md). Separate from construction for the same reason as
    /// [`with_bash_limits`](Self::with_bash_limits): only a served session has a
    /// config, and the dev views never spawn anything.
    pub fn with_child_env(mut self, child_env: crate::env_table::ChildEnv) -> Self {
        for provider in &mut self.providers {
            if let Provider::Bash(bash) = provider {
                bash.child_env = child_env.clone();
            }
        }
        self.child_env = child_env;
        self
    }

    /// Apply the config's foreground-lifetime limits to the mounted `bash` provider
    /// (docs/supervisor.md). Separate from construction because only a served session
    /// has a config — the dev views (`wire`, `spotter`, `triage`) build a toolbox to read
    /// tool *definitions* and never run a command, so they keep the defaults.
    pub fn with_bash_limits(mut self, limits: crate::config::BashLimits) -> Self {
        for provider in &mut self.providers {
            if let Provider::Bash(bash) = provider {
                bash.background_after = limits.background_after;
                bash.background_after_max = limits.background_after_max;
            }
        }
        self
    }

    /// Bash plus the in-process MCP servers — fs-mcp (read / search / write /
    /// list) and semantic-edit-mcp (preview / retarget / persist tree-sitter
    /// edits) — each with its session cwd seeded to `cwd` and an in-memory session
    /// store (no shared `~/.ai-tools` writes). The agent gets real file +
    /// structured-edit tools that lean on the fork instead of piping to
    /// `head`/`grep` (docs/triage.md, docs/tools.md). `scratch` is the
    /// session scratch dir bash exports as `$TMPDIR` (docs/tools.md); `None` for
    /// contexts that never execute (the `wire` dev view).
    pub fn bash_and_mcps(cwd: PathBuf, scratch: Option<PathBuf>) -> anyhow::Result<Self> {
        Ok(Self::new(vec![
            Provider::Bash(Bash {
                cwd: cwd.clone(),
                tmpdir: scratch,
                ..Bash::default()
            }),
            Provider::ReadImage(ReadImage { cwd: cwd.clone() }),
            // fs-mcp curated down to the tools whose structured, fork-friendly
            // output genuinely beats the shell, rooted at the session's cwd.
            // `move`/`delete` are dropped: they
            // earn nothing over bash's `mv`/`rm` — even Haiku reaches for those —
            // so they aren't worth the tool-doc tokens (docs/tools.md taxonomy).
            // `list` survives because it is gitignore-aware (skips `target/`,
            // `.git/`), which `ls`/`find` are not.
            Provider::Fs(Box::new(McpServer::from_state(
                FsTools::new(cwd.clone()),
                &["move", "delete"],
            ))),
            // semantic-edit-mcp, whole, rooted at the session's cwd.
            Provider::SemanticEdit(Box::new(McpServer::from_state(
                SemanticEditTools::new(cwd)?,
                &[],
            ))),
        ]))
    }

    /// Mount one user-declared connected provider (docs/tools.md): connect,
    /// handshake, snapshot its tool table, and add it to the mounted set.
    /// Namespacing (`<server>__<tool>`) keeps its names disjoint from every
    /// other provider's, so the unique-names invariant holds by construction.
    /// Call before the first projection — the mounted set must be fixed for
    /// the session (docs/log-and-projection.md).
    pub async fn mount_connected(
        &mut self,
        name: &str,
        config: &crate::config::McpTransport,
        cwd: &std::path::Path,
        authorization: Option<&str>,
    ) -> anyhow::Result<()> {
        let provider =
            crate::mcp::ConnectedProvider::mount(name, config, cwd, authorization, &self.child_env)
                .await?;
        self.providers.push(Provider::Connected(Box::new(provider)));
        Ok(())
    }

    /// Mount the session-control provider (docs/tools.md): the session reaches
    /// its project's other sessions through `host`, in-process, under the
    /// `sessions__<verb>` wire names. `caller` is the session's own id.
    /// `settle` is hidden from the mounted set, like fs's `delete`.
    pub fn mount_sessions(&mut self, host: std::sync::Arc<crate::daemon::Host>, caller: String) {
        let state = crate::control_tools::Control::new(
            crate::control_tools::Executor::host(host, caller),
            false,
        );
        self.providers.push(Provider::Sessions(Box::new(
            McpServer::from_state_namespaced(state, &["settle"], "sessions"),
        )));
    }

    /// The session scratch dir the bash provider exports as `$TMPDIR`, if any.
    /// The daemon reads it here to seed the path expander and the background
    /// spawn env — one source of truth: the value the toolbox was built with
    /// (docs/tools.md).
    pub fn scratch_dir(&self) -> Option<&std::path::Path> {
        self.providers.iter().find_map(|p| match p {
            Provider::Bash(b) => b.tmpdir.as_deref(),
            _ => None,
        })
    }

    /// Bind a `sessions__approve` call to the leaf action its target holds
    /// (docs/permissions.md). The loop asks the mounted sessions provider, which
    /// holds the in-process host and the caller's identity; a target that is not
    /// the pending gate, a refused target, or a cyclic chain is an error and the
    /// loop refuses before classification.
    pub fn resolve_bound_action(
        &self,
        call: &ToolCall,
    ) -> anyhow::Result<crate::control_tools::BoundAction> {
        let sessions = self
            .providers
            .iter()
            .find_map(|p| match p {
                Provider::Sessions(s) => Some(s),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("the sessions provider is not mounted"))?;
        sessions.resolve_bound_action(call)
    }

    /// All tool definitions, in mount order — the request's `tools[]`, each carrying
    /// the injected [`INTENT`] property (docs/tools.md). One injection site for every
    /// backend: a tool the harness mounts and a tool a connected server advertises
    /// get the same field on the same terms.
    pub fn definitions(&self) -> Vec<Value> {
        self.providers
            .iter()
            .flat_map(Provider::definitions)
            .map(with_intent)
            .collect()
    }

    /// Every advertised name the given dialect's request would reject, each with why
    /// (docs/tools.md). Names are checked as they reach the wire — the namespaced
    /// `<server>__<tool>` form — because the MCP spec lets a server's own tool name
    /// carry characters and lengths the request alphabet does not, and the namespace
    /// prefix counts against the ceiling. Empty means every name is legal.
    pub fn illegal_names(&self, dialect: crate::api::WireDialect) -> Vec<String> {
        self.names()
            .iter()
            .filter_map(|name| {
                dialect
                    .tool_name_problem(name)
                    .map(|why| format!("{} {why}", crate::mcp::display_name(name)))
            })
            .collect()
    }

    /// The names of every mounted tool, in mount order.
    pub fn names(&self) -> Vec<String> {
        self.definitions()
            .iter()
            .filter_map(|d| d.get("name")?.as_str().map(str::to_string))
            .collect()
    }

    /// What a call routed here under no mounted name gets back — the toolbox's own
    /// names. The loop short-circuits a name no *turn* tool owns before dispatch
    /// (docs/permissions.md), so this path answers the narrower question: the name
    /// reached a toolbox that does not own it.
    fn unknown_tool_message(&self, name: &str) -> String {
        unknown_tool_message(name, &self.names())
    }

    /// The spotter's brief for a structured tool (docs/permissions.md): what the
    /// mount snapshotted about it, so the rung that judges an unfamiliar tool sees
    /// more than a name and a JSON blob. `None` for bash and for a name no backend
    /// owns.
    pub fn brief(&self, name: &str) -> Option<ToolBrief<'_>> {
        self.providers.iter().find_map(|p| p.brief(name))
    }

    /// One mounted tool's full definition, addressed by its wire name — the
    /// answer `GET /v1/tools/:name` serves (docs/control-api.md). Unlike
    /// [`Toolbox::brief`], this has no holes: `Bash` and `ReadImage` answer
    /// here even though the spotter's brief skips them for reasons that are
    /// about the spotter, not about the definition existing. `None` when no
    /// mounted provider owns `name`.
    pub fn tool_entry(&self, name: &str) -> Option<ToolEntry> {
        self.providers.iter().find_map(|p| p.tool_entry(name))
    }

    /// Every mounted tool's full definition, keyed by wire name — what the
    /// daemon snapshots into `Session` before the toolbox moves into the loop
    /// (docs/control-api.md), since the mounted set is fixed for the session
    /// and there is nothing to reconcile later (docs/log-and-projection.md).
    pub fn tool_catalog(&self) -> std::collections::HashMap<String, ToolEntry> {
        self.names()
            .into_iter()
            .filter_map(|name| {
                let entry = self.tool_entry(&name)?;
                Some((name, entry))
            })
            .collect()
    }

    /// Route a call to the backend that handles its name. An unknown name comes
    /// back as an error result (never a panic) so the assistant `tool_use` still
    /// gets exactly one paired `tool_result` — the API rejects an orphan.
    pub async fn run(
        &self,
        call: &ToolCall,
        abort: impl Future<Output = Interrupt> + Send,
        on_spawn: impl FnOnce(Option<u32>) + Send,
        on_output: impl FnMut(&[u8]) + Send,
        on_notify: impl FnMut(Notification) + Send,
    ) -> Outcome {
        // A model that reports no image input is told so *before* the read
        // (docs/tools.md): `read_image` exists only to produce an image, so it
        // has no useful degraded form, and decoding a file to throw the pixels
        // away spends the work to arrive at the same answer.
        if !self.image_input && call.name == read_image::NAME {
            return RawResult::error(NO_IMAGE_INPUT_REFUSAL).into();
        }
        let outcome = match self.providers.iter().find(|p| p.handles(&call.name)) {
            Some(provider) => {
                let call = provider.without_intent(call);
                provider
                    .run(&call, abort, on_spawn, on_output, on_notify)
                    .await
            }
            None => RawResult::error(self.unknown_tool_message(&call.name)).into(),
        };
        strip_unreadable_images(outcome, self.image_input)
    }
}

/// What a call that would have produced an image gets back when the session's
/// model reports no image input (docs/tools.md). Addressed to the model, and
/// says what *would* work — the sentence a model reads after a refusal decides
/// whether it retries the same call.
const NO_IMAGE_INPUT_REFUSAL: &str = "this session's model takes no image input, so an image \
     cannot be added to the transcript. read the file's text some other way, or ask the operator \
     to serve a model that takes images.";

/// Drop realized images from a result the model cannot read, replacing the bytes
/// with a line saying why (docs/tools.md). An MCP tool call is not *about* its
/// image the way `read_image` is — a screenshot tool's text half is often the
/// useful part — so the call still succeeds and `is_error` stays false; what
/// changes is that the log and the wire carry no bytes nothing will look at, and
/// the model is told rather than left to infer from silence.
pub(super) fn strip_unreadable_images(outcome: Outcome, image_input: bool) -> Outcome {
    if image_input {
        return outcome;
    }
    let Outcome::Done(mut result) = outcome else {
        return outcome;
    };
    if result.images.is_empty() {
        return Outcome::Done(result);
    }
    let dropped = result.images.len();
    result.images.clear();
    result.content.push_str(&format!(
        "\n\n[{dropped} image block(s) withheld: this session's model takes no image input]"
    ));
    Outcome::Done(result)
}
