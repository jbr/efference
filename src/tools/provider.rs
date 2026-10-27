//! What each tool backend answers: its definitions, the names it serves, and
//! the dispatch of one call (docs/tools.md).

use super::*;

impl Provider {
    /// This backend's wire tool definitions (`tools[]` entries).
    pub(super) fn definitions(&self) -> Vec<Value> {
        match self {
            Provider::Bash(_) => vec![Bash::definition()],
            Provider::ReadImage(_) => vec![ReadImage::definition()],
            Provider::Fs(s) => s.definitions.clone(),
            Provider::SemanticEdit(s) => s.definitions.clone(),
            Provider::Sessions(s) => s.definitions.clone(),
            Provider::Connected(c) => c.definitions(),
        }
    }

    /// Drop the tools `enabled` refuses ([`Toolbox::retain_enabled`]); whether any remain.
    pub(super) fn retain_enabled(&mut self, enabled: &impl Fn(&str) -> bool) -> bool {
        match self {
            Provider::Bash(_) => enabled(bash::NAME),
            Provider::ReadImage(_) => enabled(read_image::NAME),
            Provider::Fs(s) => s.retain_enabled(enabled),
            Provider::SemanticEdit(s) => s.retain_enabled(enabled),
            Provider::Sessions(s) => s.retain_enabled(enabled),
            Provider::Connected(c) => c.retain_enabled(enabled),
        }
    }

    /// Whether this backend owns the named tool.
    pub(super) fn handles(&self, name: &str) -> bool {
        match self {
            Provider::Bash(_) => name == bash::NAME,
            Provider::ReadImage(_) => name == read_image::NAME,
            Provider::Fs(s) => s.handles(name),
            Provider::SemanticEdit(s) => s.handles(name),
            Provider::Sessions(s) => s.handles(name),
            Provider::Connected(c) => c.handles(name),
        }
    }

    /// The spotter's brief for a tool this backend owns. `Bash` has none: a bash
    /// move is judged as its command text, not as a tool schema
    /// (`spotter::render_move`).
    pub(super) fn brief(&self, name: &str) -> Option<ToolBrief<'_>> {
        match self {
            Provider::Bash(_) => None,
            // No brief yet either: nearly every call clears on a path grant, and
            // the spotter's structured-move render shows the input verbatim.
            Provider::ReadImage(_) => None,
            Provider::Fs(s) => s.schema(name).map(|definition| ToolBrief {
                server: None,
                definition,
            }),
            Provider::SemanticEdit(s) => s.schema(name).map(|definition| ToolBrief {
                server: None,
                definition,
            }),
            Provider::Sessions(s) => s.schema(name).map(|definition| ToolBrief {
                server: None,
                definition,
            }),
            Provider::Connected(c) => c.brief(name),
        }
    }

    /// The call as the *tool* should see it: [`INTENT`] removed, since the harness
    /// put it there and the tool's schema never asked for it (docs/tools.md).
    /// Borrows unless there is something to remove — the common shape is one clone
    /// of a small `Value` per call.
    ///
    /// A tool that declares its own `intent` keeps the field: it is the tool's, and
    /// [`with_intent`] left that schema alone, so nothing was injected to remove.
    pub(super) fn without_intent<'a>(&self, call: &'a ToolCall) -> Cow<'a, ToolCall> {
        let declares_own = match self {
            Provider::Bash(_) => declares_own_intent(&Bash::definition()["input_schema"]),
            Provider::ReadImage(_) => declares_own_intent(&ReadImage::definition()["input_schema"]),
            Provider::Fs(s) => s
                .schema(&call.name)
                .is_some_and(|d| declares_own_intent(&d.input_schema)),
            Provider::SemanticEdit(s) => s
                .schema(&call.name)
                .is_some_and(|d| declares_own_intent(&d.input_schema)),
            Provider::Sessions(s) => s
                .schema(&call.name)
                .is_some_and(|d| declares_own_intent(&d.input_schema)),
            Provider::Connected(c) => c
                .brief(&call.name)
                .is_some_and(|b| declares_own_intent(&b.definition.input_schema)),
        };
        if declares_own || call.input.get(INTENT).is_none() {
            return Cow::Borrowed(call);
        }
        let mut stripped = call.clone();
        if let Some(object) = stripped.input.as_object_mut() {
            object.remove(INTENT);
        }
        Cow::Owned(stripped)
    }

    /// This backend's full definition for `name`, for `GET /v1/tools/:name`
    /// (docs/control-api.md) — the entry [`Toolbox::tool_entry`] serves.
    /// Unlike [`Self::brief`], `Bash` and `ReadImage` answer: their wire
    /// `Value` carries a `description` and `input_schema`, just not the
    /// `annotations`/`title` the Anthropic tool shape has no slot for.
    pub(super) fn tool_entry(&self, name: &str) -> Option<ToolEntry> {
        // Every provider here is mounted through `Toolbox`, so its wire entry
        // went through `with_intent` in `Toolbox::definitions` — the served
        // schema does too (`inject_intent: true`).
        match self {
            Provider::Bash(_) if name == bash::NAME => {
                Some(ToolEntry::from_raw(name, &Bash::definition(), None, true))
            }
            Provider::Bash(_) => None,
            Provider::ReadImage(_) if name == read_image::NAME => Some(ToolEntry::from_raw(
                name,
                &ReadImage::definition(),
                None,
                true,
            )),
            Provider::ReadImage(_) => None,
            Provider::Fs(s) => Some(ToolEntry::from_definition(s.schema(name)?, None, true)),
            Provider::SemanticEdit(s) => {
                Some(ToolEntry::from_definition(s.schema(name)?, None, true))
            }
            Provider::Sessions(s) => {
                // `from_definition` carries the bare tool name; the route echoes
                // the wire name the caller looked up (docs/control-api.md).
                let mut entry = ToolEntry::from_definition(s.schema(name)?, None, true);
                entry.name = name.to_string();
                Some(entry)
            }
            Provider::Connected(c) => {
                let brief = c.brief(name)?;
                Some(ToolEntry::from_definition(
                    brief.definition,
                    brief.server.map(str::to_string),
                    true,
                ))
            }
        }
    }

    /// Run a call this backend handles. `abort` is the loop's interrupt future,
    /// resolving with the operator's intent (kill or detach); backends that
    /// can't be interrupted ignore it. `on_spawn` fires with the child's pid when
    /// the backend spawns an OS process (only bash does — the daemon registers it
    /// in the process monitor's process namespace, docs/process-monitor.md). `on_output` is
    /// the live-stream sink (docs/README.md): the long-running, interruptible
    /// bash tool streams its output and a connected server streams its progress
    /// ticks; the rest resolve whole as [`Outcome::Done`]. `on_notify` is the
    /// inbox handle (docs/inbox.md) — a connected server's
    /// `notifications/message` lines are enqueued through it.
    pub(super) async fn run(
        &self,
        call: &ToolCall,
        abort: impl Future<Output = Interrupt> + Send,
        on_spawn: impl FnOnce(Option<u32>) + Send,
        on_output: impl FnMut(&[u8]) + Send,
        on_notify: impl FnMut(Notification) + Send,
    ) -> Outcome {
        match self {
            Provider::Bash(b) => b.run(&call.input, abort, on_spawn, on_output).await,
            // Fast, so these ignore `abort`, spawn no process, and stream
            // nothing; their result lands whole as one committed entry. fs and
            // semantic-edit run their bodies on tokio's blocking pool.
            Provider::Fs(s) => s.run_call(&call.name, &call.input).await.into(),
            Provider::SemanticEdit(s) => s.run_call(&call.name, &call.input).await.into(),
            // `create` and `archive` await real work on this runtime (docs/tools.md).
            Provider::Sessions(s) => s.run_call(&call.name, &call.input).await.into(),
            Provider::ReadImage(r) => r.run(&call.input).into(),
            // Async over a wire, but no OS process of the *call's* own: the
            // server's progress ticks stream as live output and its messages
            // reach the inbox, while the result itself lands whole. Races
            // `abort` internally (Kill abandons the call and cancels it on the
            // wire; Detach is bash-only and keeps waiting).
            Provider::Connected(c) => c.run(call, abort, on_output, on_notify).await,
        }
    }
}
