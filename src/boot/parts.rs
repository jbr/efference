//! The session's runtime parts: toolbox, preamble, provider client, spotter, and
//! triage. Each is built from resolved config and capabilities and shared
//! by every way a session boots.

use std::path::Path;

use super::{describe_window, mcp_mount};
use crate::config::LoadedConfig;
use crate::config::env_table::EnvPolicy;
use crate::curation::spotter::MountedSpotter;
use crate::curation::triage::Triager;
use crate::inference::SessionInference;
use crate::inference::api::{
    Client, Endpoint, Error, ModelCapabilities, TokenPrices, UNKNOWN_OUTPUT_CAP,
};
use crate::tools::Toolbox;

/// What [`mount_toolbox`] needs from the boot so far.
pub(super) struct ToolboxInputs<'a> {
    /// The session's working directory: where bash and MCP servers run.
    pub cwd: &'a Path,
    pub scratch: &'a Path,
    pub loaded: &'a LoadedConfig,
    pub env_policy: &'a EnvPolicy,
    pub capabilities: &'a ModelCapabilities,
    pub endpoint: &'a Endpoint,
    pub host: Option<std::sync::Arc<crate::daemon::Host>>,
    /// Where MCP grants are read and written: the store the session's credentials
    /// came from ([`SessionInference::secrets`]).
    pub secrets: &'a std::sync::Arc<crate::platform::secrets::Secrets>,
    pub session_id: &'a str,
    pub reauth: &'a [String],
    pub authorization: &'a mcp_mount::AuthorizationHandling,
}

/// The toolbox, and the preamble section naming what did not mount.
pub(super) struct MountedTools {
    pub toolbox: Toolbox,
    pub unmounted_section: Option<String>,
}

/// Mount the config-declared connected providers (docs/tools.md) before the
/// first projection, so the session's tools[] is fixed from turn one — and
/// before the system prompt is built, because what did *not* mount is part
/// of what the session must know. A server that answers `401` is authorized
/// here, which is the one point in the boot that waits for a human
/// (docs/mcp-auth.md).
pub(super) async fn mount_toolbox(inputs: ToolboxInputs<'_>) -> Result<MountedTools, Error> {
    let ToolboxInputs {
        cwd,
        scratch,
        loaded,
        env_policy,
        capabilities,
        endpoint,
        host,
        secrets,
        session_id,
        reauth,
        authorization,
    } = inputs;
    let mut toolbox = Toolbox::bash_and_mcps(cwd.to_path_buf(), Some(scratch.to_path_buf()))?
        .with_bash_limits(loaded.role.bash)
        .with_child_env(env_policy.child_env())
        // The image gate (docs/tools.md): what the boot capability lookup said
        // about this model's image input, so an image-producing tool refuses
        // clearly instead of attaching bytes the model cannot read.
        .with_image_input(capabilities.supports_image_input);
    // Credentials live in the user's secret store (docs/secrets.md): an
    // authorization belongs to the operator and the server, not to one session.
    let mut store =
        crate::tools::mcp::auth::store::KeychainStore::new(std::sync::Arc::clone(secrets));
    if let Some(host) = host {
        // A hosted session refreshes under the host's locks, so the host's
        // sessions share one refresh of a server's grant (docs/mcp-auth.md).
        store = store.with_refresh_locks(host.mcp_refresh_locks());
        // The in-session `sessions` provider (docs/tools.md): a session created
        // on a host reaches its project's sessions directly, with its own id as
        // the caller. Mounted before the first projection so `tools[]` is fixed
        // from turn one.
        if let Some(server) = host.session_control_server(session_id) {
            toolbox.mount_control_server(server);
        }
        // The `lsp` built-ins (docs/tools.md), bound at the working directory. Binding
        // starts the project's language servers, so it waits on `enabled` rather than
        // leaving the tools to the filter below.
        if crate::tools::lsp::enabled(&loaded.role) {
            let lsp = crate::tools::lsp::Lsp::bind(
                std::sync::Arc::clone(host.language_servers()),
                session_id,
                cwd.to_path_buf(),
            )
            .await?;
            toolbox.mount_lsp(lsp);
        }
    }
    let store: std::sync::Arc<dyn crate::tools::mcp::auth::store::TokenStore> =
        std::sync::Arc::new(store);
    let unmounted = mcp_mount::mount_all(
        &mut toolbox,
        &loaded.role.mcp_servers,
        cwd,
        &store,
        reauth,
        authorization,
    )
    .await?;
    // The `[tools]` table's `enabled` (docs/tools.md): efference's own tools are mounted
    // only when enabled, a declared server's unless an entry turns tools off. Applied once,
    // here, so `tools[]` is fixed from turn one.
    toolbox.retain_enabled(|name| loaded.role.tool_enabled(name));
    let unmounted_section = crate::tools::mcp::unmounted_section(&unmounted);
    // The frozen toolbox against the dialect's name rule (docs/tools.md): a name the
    // request would reject fails the boot here, by its display name, instead of the
    // first turn failing with a 400 that names nothing.
    // Boot's endpoint is the session's main model, which resolution already
    // refused for a provider that serves no turns (`src/inference/options.rs`).
    let dialect = endpoint
        .provider
        .inference_dialect()
        .expect("the main model's provider serves turns");
    let illegal = toolbox.illegal_names(dialect);
    if !illegal.is_empty() {
        // Each problem rides the returned error: the create response is where
        // the operator reads it, and no print repeats it.
        return Err(format!(
            "{} mounted tool name{} the {} dialect rejects: {}",
            illegal.len(),
            if illegal.len() == 1 { "" } else { "s" },
            dialect,
            illegal.join("; ")
        )
        .into());
    }
    Ok(MountedTools {
        toolbox,
        unmounted_section,
    })
}

/// The boot preamble: the frozen system prompt, and the pieces it was built from.
pub(super) struct FrozenPreamble {
    pub identity: String,
    pub body: String,
    pub resolved: crate::record::log::context::Resolved,
    pub context_block: Option<String>,
}

/// Build the system prompt once, here at startup, and freeze it into the cached
/// prefix — it must stay byte-stable for the whole session or it would invalidate
/// the cache every turn (docs/log-and-projection.md). Everything it folds in is resolved
/// once at boot: the `[context]` block (files read + exec sections run now, frozen —
/// docs/context.md; orientation rides here as a config exec entry), the role's
/// brief, and the config note.
///
/// `inputs` carries what the boot already knows; the `[context]` block and the role
/// section come from `loaded`, resolved here.
pub(super) fn freeze_preamble(
    loaded: &LoadedConfig,
    env_policy: &EnvPolicy,
    project: &Path,
    inputs: crate::config::env::PromptInputs<'_>,
) -> Result<FrozenPreamble, Error> {
    let role_section = loaded.role.preamble_section();
    // The [context] preamble builder (docs/context.md): resolve the operator's listed
    // files from the project directory and fold the rendered block into the frozen
    // prefix. Fail closed — configured context that can't be read aborts startup rather
    // than silently vanishing.
    let mut resolved = match crate::config::context::resolve_with_child_env(
        &loaded.role.context,
        project,
        &env_policy.child_env(),
    ) {
        Ok(resolved) => resolved,
        Err(e) => {
            // The failure rides the returned error: the create response is
            // where the operator reads it, and no print repeats it.
            return Err(format!("could not resolve [context] entries: {e}").into());
        }
    };
    // Mask the resolved sections *before* rendering (docs/env.md). The preamble is
    // the one string that both rides the request's `system` field and is
    // snapshotted into the log, so masking it at the log's choke point alone would
    // leave the wire copy unfiltered and the two copies no longer byte-identical —
    // which is the whole point of the `Preamble` entry (docs/context.md). A
    // `[context]` exec that prints the environment is the concrete case.
    env_policy.mask_context(&mut resolved);
    let context_block = resolved.render();
    let crate::config::env::SystemPrompt { identity, body } =
        crate::config::env::system_prompt_parts(
            inputs
                .with_context_block(context_block.as_deref())
                .with_role_section(role_section.as_deref()),
        );
    Ok(FrozenPreamble {
        identity: env_policy.mask(&identity),
        body: env_policy.mask(&body),
        resolved,
        context_block,
    })
}

/// The boot-time request settings the agent client carries.
pub(super) struct ClientSettings {
    pub temperature: Option<u8>,
    pub upstream: Vec<String>,
    pub effort: Option<String>,
}

/// The agent client, with the upstream pin, stream ceiling, effort, and the
/// model's capabilities applied.
pub(super) fn build_client(
    endpoint: &Endpoint,
    model: &str,
    system: String,
    settings: ClientSettings,
    loaded: &LoadedConfig,
    capabilities: &ModelCapabilities,
) -> Client {
    let ClientSettings {
        temperature,
        upstream,
        effort,
    } = settings;
    // The upstream pin rides the agent client, which the triage shares — so
    // turns and gates route alike. It deliberately does **not** reach the spotter:
    // endpoint slugs are per-model, the spotter runs a different (Haiku-band)
    // model, and an order naming endpoints that do not serve it would fail
    // outright with fallbacks disabled.
    let upstream = crate::inference::api::Upstream::pinned(upstream);
    if let Some(pin) = &upstream {
        log::debug!("upstream: {} (fallbacks disabled)", pin.order.join(", "));
    }
    let mut client = Client::new(endpoint.clone(), model, system)
        .with_temperature(temperature)
        .with_upstream(upstream.clone())
        // `[transport] stream-ceiling` (docs/transport.md): the bound on a stream
        // that stays open without finalizing. It rides the agent client, which
        // the triage shares, so both stream surfaces are bounded alike.
        .with_stream_ceiling(loaded.role.transport.stream_ceiling)
        // The effort level rides the agent client, so the turn and the triage send
        // one envelope (docs/triage.md). The spotter's client is built
        // separately and never carries it; the keepalive omits it by construction.
        .with_effort(effort)
        .with_thinking(capabilities.thinking)
        // Who places cache-write boundaries (docs/transport.md): the Responses
        // request reads this to decide whether to send explicit markers.
        .with_prompt_cache(capabilities.prompt_cache);
    // The output budget follows the model's reported cap (a reasoning model's
    // thinking bills against it; the 4096 default starves a long think). No
    // report → the default stands.
    // An unreported cap still needs a number, because Messages requires
    // `max_tokens` (`UNKNOWN_OUTPUT_CAP`, docs/transport.md). Erring high is
    // deliberate and is made safe by the retry that reads the ceiling out of the
    // provider's refusal — the `Client`'s own small default would quietly cost a
    // mandatory-reasoning model its entire response.
    client = client.with_max_output(capabilities.max_output_tokens.unwrap_or(UNKNOWN_OUTPUT_CAP));
    // The window the cap is reserved against: each request sends the cap
    // shrunk to what the window spares after the prompt (`api/reservation.rs`).
    client = client.with_context_window(capabilities.context_length);
    client
}

/// The independent permission rung (docs/permissions.md) has its own connection
/// and model. It keeps a distinct sticky key even when it shares the main
/// connection.
pub(super) async fn mount_spotter(
    inference: &SessionInference,
    main_endpoint: &Endpoint,
    scratch: &Path,
    directories: &crate::config::policy::Directories,
) -> Result<MountedSpotter, Error> {
    let (spotter_selection, mut spotter_endpoint) = inference.spotter();
    let spotter_model = spotter_selection.model.clone();
    log::debug!(
        "spotter: {}/{} ({})",
        spotter_selection.connection,
        spotter_selection.model,
        spotter_selection.provider
    );
    spotter_endpoint.session_id = main_endpoint.session_id.clone();
    // The spotter's model is validated against *its* provider's metadata source,
    // not the main model's: a hosted spotter may sit on a different connection and
    // dialect. The lookup is the existence check that fails boot instead of the
    // first permission decision, and its thinking answer is the one capability the
    // spotter's client applies: its forced tool call sends thinking off, and how
    // off is spelled is the model's (docs/transport.md). It sends no effort or
    // temperature.
    //
    // A decision model has no such metadata: there is no context window, output
    // cap, or cache policy behind a decisions request (docs/permissions.md). The
    // provider's own statement about the model says which kind it is, and for a
    // decision model that lookup doubles as the existence check.
    let kind = match spotter_endpoint.spotter_kind(&spotter_model).await {
        Ok(kind) => kind,
        Err(error) => {
            return Err(format!(
                "spotter model {spotter_model:?} on {}: {error}",
                spotter_endpoint.provider
            )
            .into());
        }
    };
    let thinking = match spotter_endpoint
        .spotter_thinking(&spotter_model, kind)
        .await
    {
        Ok(thinking) => thinking,
        Err(error) => {
            return Err(format!(
                "spotter model {spotter_model:?} on {}: {error}",
                spotter_endpoint.provider
            )
            .into());
        }
    };
    // Which rung the model is (docs/permissions.md): a decision model reaches a
    // different API and answers differently, so the choice is made here, once.
    let spotter = MountedSpotter::mount(
        spotter_endpoint.for_spotter(),
        &spotter_model,
        kind,
        Some(scratch.to_path_buf()),
        directories.clone(),
    )
    .with_thinking(thinking);
    log::debug!("spotter rung: {}", spotter.kind());
    Ok(spotter)
}

/// The model-driven fork (docs/triage.md): every tool result is triaged and the
/// canonical wire keeps only what the model retains.
///
/// Sharing is explicit (docs/triage.md): an omitted triage selection
/// uses the main client; a configured selection always gets its own endpoint
/// and retry state, even when it names the same native model.
pub(super) async fn mount_triager(
    inference: &SessionInference,
    main_endpoint: &Endpoint,
    model: &str,
    context_block: Option<&str>,
    env_policy: &EnvPolicy,
    loaded: &LoadedConfig,
) -> Result<(Triager, Option<TokenPrices>), Error> {
    Ok(match inference.triage() {
        Some((selection, mut triage_endpoint, triage)) => {
            triage_endpoint.session_id = main_endpoint.session_id.clone();
            let system = env_policy.mask(&crate::config::env::triage_system_prompt(
                &triage.model,
                model,
                context_block,
            ));
            let (triager, caps) = Triager::for_triage_model(
                &triage_endpoint,
                triage,
                system,
                loaded.role.transport.stream_ceiling,
            )
            .await
            .map_err(|error| format!("triage model: {error}"))?;
            log::debug!(
                "triage: {}/{} ({}) · context {} · thinking {} · tool choice {}",
                selection.connection,
                selection.model,
                selection.provider,
                describe_window(caps.context_length),
                match (&triage.effort, triage.thinking) {
                    (Some(effort), _) => format!("on, effort {effort}"),
                    (None, true) => "on".into(),
                    (None, false) => "off".into(),
                },
                if triage.auto_choice { "auto" } else { "forced" }
            );
            (triager, caps.prices)
        }
        None => {
            log::debug!("triage: {model} (shared main client)");
            (Triager::shared(), None)
        }
    })
}
