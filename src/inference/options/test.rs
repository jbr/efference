//! Resolution tests. The properties that matter are *independence* (one bad field
//! never empties another) and *agreement* (what preview reports is what creation
//! builds), so most of these assert one field while a neighbour stays resolved.

use super::*;
use crate::env_table::EnvSnapshot;
use crate::inference::InferenceConfig;
use crate::secrets::Secrets;
use std::path::Path;
use std::sync::Arc;

fn registry(toml: &str) -> InferenceRegistry {
    let config = InferenceConfig::from_toml(toml).unwrap();
    let environment = EnvSnapshot::new(
        [
            ("ANTHROPIC_KEY".to_string(), "secret".to_string()),
            ("OPENAI_KEY".to_string(), "other".to_string()),
        ]
        .into(),
    )
    .unwrap();
    InferenceRegistry::from_config(
        config,
        Path::new("/host"),
        &environment,
        &Arc::new(Secrets::in_memory()),
    )
    .unwrap()
}

/// One anthropic connection named `main`, with no defaults. Tests that need a
/// resolved main pass it explicitly; an omitted model is the ordinary
/// no-past-pick state (one `Required` issue, other fields independent).
fn one_connection() -> InferenceRegistry {
    registry(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    )
}

/// An explicit main model on the `main` connection, for tests that need main
/// resolved while something else is under test.
fn main_options() -> SessionOptions {
    options(serde_json::json!({"connection": "main", "model": "claude-sonnet-4-5"}))
}

/// A registry with a history store in a temporary directory, so resolution
/// tests can seed past picks. The scope keys on the same directory, which stays
/// until the returned guard drops.
fn historied(
    toml: &str,
) -> (
    InferenceRegistry,
    crate::history::History,
    crate::history::Scope,
    tempfile::TempDir,
) {
    let dir = crate::testing::temp_dir("resolve");
    let store = crate::history::History::at(dir.path().join("state.db"));
    let registry = registry(toml);
    let scope = crate::history::Scope {
        cwd: dir.path().to_path_buf(),
        config_key: crate::history::BUILTIN_CONFIG_KEY.to_string(),
    };
    (registry, store, scope, dir)
}

fn options(json: serde_json::Value) -> SessionOptions {
    serde_json::from_value(json).expect("a session options body")
}

fn codes(resolved: &ResolvedOptions) -> Vec<(&str, IssueCode)> {
    resolved
        .issues
        .iter()
        .map(|issue| (issue.field.as_ref(), issue.code))
        .collect()
}

/// **The effective triage is what creation would build.** An omitted triage
/// shares the main client, so reporting `shared` here describes the session
/// the host creates — and creation agrees, with no client of its own.
#[test]
fn an_omitted_triage_shares_main() {
    let registry = one_connection();
    let resolved = registry.resolve_options(&main_options(), None, None);
    assert_eq!(resolved.issues, vec![]);
    assert_eq!(
        resolved.effective.triage.value,
        Some(EffectiveTriage::Shared)
    );
    assert_eq!(
        resolved.effective.triage.source,
        Some(SelectionSource::SharedMain)
    );
    // And creation agrees: the same resolution, prepared, has no triage client.
    assert!(registry.prepare(resolved).unwrap().triage().is_none());
}

/// With nothing configured, triage shares main — and says so, so a client can tell
/// the two reasons for `shared` apart.
#[test]
fn an_omitted_triage_with_no_default_shares_main() {
    let resolved = one_connection().resolve_options(&main_options(), None, None);
    assert_eq!(
        resolved.effective.triage.value,
        Some(EffectiveTriage::Shared)
    );
    assert_eq!(
        resolved.effective.triage.source,
        Some(SelectionSource::SharedMain)
    );
}

/// An explicit `shared` reports the request as its source rather than the
/// fallback — the wire still accepts a pinned share even though the form no
/// longer offers one (there is nothing to pin against).
#[test]
fn an_explicit_shared_triage_reports_the_request() {
    let registry = registry(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let resolved = registry.resolve_options(
        &SessionOptions {
            connection: Some("main".to_string()),
            model: Some("claude-sonnet-4-5".to_string()),
            triage: Some(TriageChoice::Shared),
            ..Default::default()
        },
        None,
        None,
    );
    assert_eq!(
        resolved.effective.triage.value,
        Some(EffectiveTriage::Shared)
    );
    assert_eq!(
        resolved.effective.triage.source,
        Some(SelectionSource::Request)
    );
    assert!(registry.prepare(resolved).unwrap().triage().is_none());
}

/// **Fields are independent.** An unusable spotter must not take the main
/// selection, the triage, the effort, or the temperature down with it — a form
/// showing one error still shows every other resolved value.
#[test]
fn an_unresolvable_role_leaves_every_other_field_resolved() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({
            "connection": "main",
            "model": "claude-sonnet-4-5",
            "spotter_connection": "absent",
            "spotter_model": "whatever",
            "temperature": 42,
            "effort": "high",
        })),
        None,
        None,
    );
    assert_eq!(
        codes(&resolved),
        [("spotter_model", IssueCode::InvalidSelection)]
    );
    assert_eq!(resolved.effective.spotter.value, None);
    assert_eq!(resolved.effective.spotter.source, None);
    assert_eq!(
        resolved
            .effective
            .main
            .value
            .as_ref()
            .map(|s| s.model.as_str()),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(resolved.effective.temperature.value, Some(42));
    assert_eq!(resolved.effective.effort.value.as_deref(), Some("high"));
    assert_eq!(
        resolved.effective.triage.value,
        Some(EffectiveTriage::Shared)
    );
}

/// Past picks are the omitted-branch default: the aggregate answers when the
/// project has no mass of its own, and reports the history branch.
#[test]
fn an_omitted_model_defaults_to_the_history_aggregate() {
    let (registry, store, scope, _dir) = historied(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let now = crate::history::now();
    store.record(&crate::history::Pick {
        at: now,
        cwd: "/code/elsewhere".to_string(),
        config_key: crate::history::BUILTIN_CONFIG_KEY.to_string(),
        connection: "main".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-opus-4-5".to_string(),
    });
    let resolved =
        registry.resolve_options(&SessionOptions::default(), None, Some((&store, &scope)));
    assert_eq!(resolved.issues, vec![]);
    let main = resolved.effective.main.value.expect("a history default");
    assert_eq!(main.model, "claude-opus-4-5");
    assert_eq!(
        resolved.effective.main.source,
        Some(SelectionSource::History)
    );
}

/// Project mass elects the project mode over a heavier aggregate.
#[test]
fn project_mass_elects_the_project_mode_in_resolution() {
    let (registry, store, scope, _dir) = historied(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.other]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let now = crate::history::now();
    for _ in 0..6 {
        store.record(&crate::history::Pick {
            at: now,
            cwd: scope.cwd.to_string_lossy().into_owned(),
            config_key: scope.config_key.clone(),
            connection: "main".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5".to_string(),
        });
    }
    for _ in 0..20 {
        store.record(&crate::history::Pick {
            at: now,
            cwd: "/code/elsewhere".to_string(),
            config_key: crate::history::BUILTIN_CONFIG_KEY.to_string(),
            connection: "other".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-opus-4-5".to_string(),
        });
    }
    let resolved =
        registry.resolve_options(&SessionOptions::default(), None, Some((&store, &scope)));
    let main = resolved.effective.main.value.expect("a project default");
    assert_eq!(main.connection, "main");
    assert_eq!(main.model, "claude-sonnet-4-5");
    assert_eq!(
        resolved.effective.main.source,
        Some(SelectionSource::History)
    );
}

/// A remembered connection that is no longer configured falls through to the
/// refusal silently — no request named it, so there is no invalid-selection
/// issue, just the ordinary omitted-model one.
#[test]
fn a_stale_history_pick_falls_through_to_the_refusal() {
    let (registry, store, scope, _dir) = historied(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let now = crate::history::now();
    store.record(&crate::history::Pick {
        at: now,
        cwd: scope.cwd.to_string_lossy().into_owned(),
        config_key: scope.config_key.clone(),
        connection: "retired".to_string(),
        provider: "anthropic".to_string(),
        model: "old-model".to_string(),
    });
    let resolved =
        registry.resolve_options(&SessionOptions::default(), None, Some((&store, &scope)));
    assert_eq!(codes(&resolved), [("model", IssueCode::Required)]);
    assert_eq!(resolved.effective.main.value, None);
}

/// A host with no past pick is the ordinary first-run state. It leaves an
/// editable response — one issue on `model`, every independent field resolved —
/// rather than an empty one the form cannot open on.
#[test]
fn a_missing_main_default_still_resolves_the_independent_fields() {
    let registry = registry(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let resolved = registry.resolve_options(
        &options(serde_json::json!({"temperature": 42, "effort": "high"})),
        None,
        None,
    );
    assert_eq!(codes(&resolved), [("model", IssueCode::Required)]);
    assert_eq!(resolved.effective.main.value, None);
    assert_eq!(resolved.effective.temperature.value, Some(42));
    assert_eq!(resolved.effective.effort.value.as_deref(), Some("high"));
    // The spotter borrows main's provider, so it has nothing to resolve from —
    // and adds no second issue for the one mistake.
    assert_eq!(resolved.effective.spotter.value, None);
}

/// Two unrelated mistakes are two issues, not the first one found. (200 is a
/// `u8` the resolver rejects; a value past 255 never deserializes at all, which
/// is why `QUERY /sessions` decodes leniently before it resolves.)
#[test]
fn independent_issues_survive_together() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({"connection": "absent", "model": "m", "temperature": 200})),
        None,
        None,
    );
    assert_eq!(
        codes(&resolved),
        [
            ("model", IssueCode::InvalidSelection),
            ("temperature", IssueCode::OutOfRange),
        ]
    );
}

/// **A requested value reports the request.** Reporting `provider_default` for a
/// temperature the caller just sent tells a form its own edit was ignored.
#[test]
fn a_requested_tuning_value_reports_the_request_as_its_source() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({"temperature": 42})),
        None,
        None,
    );
    assert_eq!(resolved.effective.temperature.value, Some(42));
    assert_eq!(
        resolved.effective.temperature.source,
        Some(SelectionSource::Request)
    );
}

/// An omitted tuning field is a known omission — a value the wire will not carry —
/// which is a different answer from a field that failed to resolve.
#[test]
fn an_omitted_tuning_value_is_an_omission_not_a_failure() {
    let resolved = one_connection().resolve_options(&SessionOptions::default(), None, None);
    assert_eq!(resolved.effective.temperature.value, None);
    assert_eq!(
        resolved.effective.temperature.source,
        Some(SelectionSource::ProviderDefault)
    );
    assert_eq!(resolved.effective.effort.value, None);
    assert_eq!(
        resolved.effective.effort.source,
        Some(SelectionSource::ProviderDefault)
    );
}

/// The request beats the project config; the project config beats omission.
#[test]
fn request_effort_beats_project_effort_which_beats_omission() {
    let registry = one_connection();
    let requested = registry.resolve_options(
        &options(serde_json::json!({"effort": "high"})),
        Some("medium"),
        None,
    );
    assert_eq!(requested.effective.effort.value.as_deref(), Some("high"));
    assert_eq!(
        requested.effective.effort.source,
        Some(SelectionSource::Request)
    );
    let inherited = registry.resolve_options(&SessionOptions::default(), Some("medium"), None);
    assert_eq!(inherited.effective.effort.value.as_deref(), Some("medium"));
    assert_eq!(
        inherited.effective.effort.source,
        Some(SelectionSource::ProjectConfig)
    );
}

/// **Naming the connection makes the model a native id.** OpenRouter ids carry a
/// slash of their own, so the slug grammar cannot tell `work/x` the connection
/// from `anthropic/claude` the model — the split spelling has no such ambiguity.
#[test]
fn an_explicit_connection_makes_a_slash_bearing_model_a_native_id() {
    let registry = registry(
        r#"
[connections.main]
provider = "openrouter"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let resolved = registry.resolve_options(
        &options(serde_json::json!({"connection": "main", "model": "anthropic/claude-sonnet-4-5"})),
        None,
        None,
    );
    assert_eq!(resolved.issues, vec![]);
    let main = resolved
        .effective
        .main
        .value
        .expect("an explicit selection");
    assert_eq!(main.connection, "main");
    assert_eq!(main.model, "anthropic/claude-sonnet-4-5");
}

/// The slug spelling still resolves, so one opaque string remains a usable way to
/// name a model — what the CLI flags and the `create` tool pass.
#[test]
fn the_slug_spelling_still_names_a_connection_and_a_model() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({"model": "main/claude-opus-4-5"})),
        None,
        None,
    );
    assert_eq!(resolved.issues, vec![]);
    let main = resolved.effective.main.value.expect("a slug selection");
    assert_eq!(
        (main.connection.as_str(), main.model.as_str()),
        ("main", "claude-opus-4-5")
    );
}

/// A connection the host does not have is disprovable, so it is an issue — not a
/// silent native id on whatever connection the defaults happened to name.
#[test]
fn an_unknown_connection_is_an_issue() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({"connection": "absent", "model": "m"})),
        None,
        None,
    );
    assert_eq!(codes(&resolved), [("model", IssueCode::InvalidSelection)]);
    assert_eq!(resolved.effective.main.value, None);
}

/// A model name the host has no catalog for is *not* disprovable, so it resolves
/// unquestioned: "could not validate" reads the same as "nothing known wrong".
#[test]
fn an_unknown_model_on_a_known_connection_resolves() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({"connection": "main", "model": "not-a-real-model"})),
        None,
        None,
    );
    assert_eq!(resolved.issues, vec![]);
    assert_eq!(
        resolved.effective.main.value.map(|s| s.model),
        Some("not-a-real-model".to_string())
    );
}

/// A form picks a connection before it has a model. There is no provider-default
/// main model to stand in, so the honest answer is that one is still needed.
#[test]
fn a_connection_without_a_model_asks_for_a_model() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({"connection": "main"})),
        None,
        None,
    );
    assert_eq!(codes(&resolved), [("model", IssueCode::Required)]);
    assert_eq!(resolved.effective.main.value, None);
}

/// An invalid explicit override never silently becomes the default: the form must
/// show the edit as rejected, not quietly replace it.
#[test]
fn an_invalid_override_does_not_fall_back_to_the_default() {
    let resolved = one_connection().resolve_options(
        &options(serde_json::json!({"connection": "absent", "model": "m"})),
        None,
        None,
    );
    assert_eq!(resolved.effective.main.value, None);
    assert_eq!(resolved.effective.main.source, None);
}

/// A disabled connection is configured but unusable, and says which of the two it
/// is — the operator turned it off, rather than a credential failing to load.
#[test]
fn a_disabled_connection_is_an_unavailable_one() {
    let registry = registry(
        r#"
[connections.off]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
disabled = true
"#,
    );
    let resolved = registry.resolve_options(
        &options(serde_json::json!({"connection": "off", "model": "m"})),
        None,
        None,
    );
    assert_eq!(
        codes(&resolved),
        [("model", IssueCode::UnavailableConnection)]
    );
}

/// A separate triage reads the text view (docs/triage.md), so a triage model of
/// another wire format than main's is a valid draft, and creation accepts it.
#[test]
fn a_cross_dialect_triage_is_valid() {
    let registry = registry(
        r#"
[connections.messages]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.responses]
provider = "openai"
auth = { kind = "env", key = "OPENAI_KEY" }
"#,
    );
    let resolved = registry.resolve_options(
        &options(serde_json::json!({
            "connection": "messages",
            "model": "claude-sonnet-4-5",
            "triage": {"mode": "separate", "connection": "responses", "model": "gpt-5.6"},
        })),
        None,
        None,
    );
    assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
    assert!(matches!(
        resolved.effective.triage.value,
        Some(EffectiveTriage::Separate { .. })
    ));
}

/// An omitted spotter takes `[defaults].spotter` before the main provider's own
/// cheap-band pick, and names which branch it used.
#[test]
fn an_omitted_spotter_prefers_the_configured_default_to_the_main_provider() {
    let configured = registry(
        r#"
[defaults]
spotter = "spot/claude-haiku-4-5"

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.spot]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    )
    .resolve_options(&main_options(), None, None);
    assert_eq!(
        configured.effective.spotter.source,
        Some(SelectionSource::UserDefault)
    );
    assert_eq!(
        configured.effective.spotter.value.map(|s| s.connection),
        Some("spot".to_string())
    );

    let borrowed = one_connection().resolve_options(&main_options(), None, None);
    assert_eq!(
        borrowed.effective.spotter.source,
        Some(SelectionSource::MainProvider)
    );
    assert_eq!(
        borrowed.effective.spotter.value.map(|s| s.model),
        Some("claude-haiku-4-5".to_string())
    );
}

/// **Preview and creation agree.** The selections creation boots with are the ones
/// preview reported — the whole reason both go through one resolution.
#[test]
fn creation_prepares_exactly_the_selections_preview_reports() {
    let registry = registry(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.other]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let options = options(serde_json::json!({
        "connection": "main",
        "model": "claude-sonnet-4-5",
        "spotter_connection": "other",
        "spotter_model": "claude-haiku-4-5",
        "triage": {"mode": "separate", "connection": "main", "model": "claude-haiku-4-5",
                   "thinking": true},
    }));
    let resolved = registry.resolve_options(&options, None, None);
    let effective = resolved.effective.clone();
    let prepared = registry.prepare(resolved).expect("a clean draft boots");
    assert_eq!(Some(prepared.main().0.clone()), effective.main.value);
    assert_eq!(Some(prepared.spotter().0.clone()), effective.spotter.value);
    let (selection, _, triage_options) = prepared.triage().expect("a separate triage client");
    let EffectiveTriage::Separate {
        selection: previewed,
        thinking,
        ..
    } = effective.triage.value.expect("a previewed triage")
    else {
        panic!("an explicit separate choice previews as separate");
    };
    assert_eq!(*selection, previewed);
    assert!(thinking && triage_options.thinking);
}

/// Creation refuses a draft preview flagged, rather than booting past it.
#[test]
fn creation_refuses_a_draft_with_issues() {
    let registry = one_connection();
    let resolved = registry.resolve_options(
        &options(serde_json::json!({
            "connection": "main",
            "model": "claude-sonnet-4-5",
            "temperature": 200,
        })),
        None,
        None,
    );
    // `SessionInference` has no Debug — it holds credential material — so the
    // refusal is read out of the Err arm rather than unwrapped.
    let Err(error) = registry.prepare(resolved) else {
        panic!("an out-of-range draft unexpectedly prepared");
    };
    assert!(error.to_string().contains("temperature"), "{error}");
}

/// The range lives in one place, so the CLI, `POST /sessions`, and the preview
/// cannot disagree about where the bound is.
#[test]
fn the_temperature_bound_is_the_resolver_s() {
    let registry = one_connection();
    for (temperature, expected) in [(0u8, true), (100, true), (101, false)] {
        let resolved = registry.resolve_options(
            &options(serde_json::json!({
                "connection": "main",
                "model": "claude-sonnet-4-5",
                "temperature": temperature,
            })),
            None,
            None,
        );
        assert_eq!(
            resolved.issues.is_empty(),
            expected,
            "temperature {temperature} should {}be accepted",
            if expected { "" } else { "not " }
        );
    }
}

/// The loader's own words reach the operator at creation and stop at the wire: a
/// credential error may name an environment key or a keychain item, and the
/// preview response is the one surface that is not certainly local.
#[test]
fn a_credential_loader_error_reaches_creation_but_not_the_response() {
    let registry = registry(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ABSENT_KEY" }
"#,
    );
    let resolved = registry.resolve_options(
        &options(serde_json::json!({"connection": "main", "model": "m"})),
        None,
        None,
    );
    let issue = resolved.issues.first().expect("an unavailable connection");
    assert_eq!(issue.code, IssueCode::UnavailableConnection);
    let wire = serde_json::to_string(&resolved).unwrap();
    assert!(!wire.contains("ABSENT_KEY"), "{wire}");
    let Err(error) = registry.prepare(resolved) else {
        panic!("an unavailable connection unexpectedly prepared");
    };
    assert!(error.to_string().contains("ABSENT_KEY"), "{error}");
}

/// A registry with a decision-model connection beside an ordinary one
/// (docs/permissions.md): `typesafe` serves the decisions route only.
fn with_a_decision_connection() -> InferenceRegistry {
    registry(
        r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }

[connections.typesafe]
provider = "typesafe"
auth = { kind = "env", key = "OPENAI_KEY" }
"#,
    )
}

/// **A decision-model provider can back the spotter and nothing else.** It
/// produces no assistant turn, so a role that runs one is refused at resolution
/// — which is the invariant every `inference_dialect()` reader downstream rests
/// on (docs/transport.md).
#[test]
fn a_decision_connection_is_refused_for_the_main_model() {
    let resolved = with_a_decision_connection().resolve_options(
        &options(serde_json::json!({
            "connection": "typesafe",
            "model": "jev-latest",
        })),
        None,
        None,
    );
    assert_eq!(codes(&resolved), [("model", IssueCode::InvalidSelection)]);
    assert_eq!(resolved.effective.main.value, None);
}

/// The same refusal for triage, which sends requests of its own.
#[test]
fn a_decision_connection_is_refused_for_triage() {
    let resolved = with_a_decision_connection().resolve_options(
        &options(serde_json::json!({
            "connection": "main",
            "model": "claude-sonnet-4-5",
            "triage": {"mode": "separate", "connection": "typesafe", "model": "jev-latest"},
        })),
        None,
        None,
    );
    assert_eq!(codes(&resolved), [("triage", IssueCode::InvalidSelection)]);
}

/// The same connection is fine for the spotter, which is the whole point of it.
#[test]
fn a_decision_connection_backs_the_spotter() {
    let resolved = with_a_decision_connection().resolve_options(
        &options(serde_json::json!({
            "connection": "main",
            "model": "claude-sonnet-4-5",
            "spotter_connection": "typesafe",
            "spotter_model": "jev-latest",
        })),
        None,
        None,
    );
    assert_eq!(codes(&resolved), []);
    assert_eq!(
        resolved
            .effective
            .spotter
            .value
            .as_ref()
            .map(|s| s.model.as_str()),
        Some("jev-latest")
    );
}

/// A control client decodes the host's own types rather than a parallel copy of
/// them (docs/control-api.md), so what `QUERY /sessions` writes has to read back
/// as what it was. The bytes here are a live host's answer.
#[test]
fn a_hosts_preview_reads_back_as_what_it_wrote() {
    let wire = r#"{
        "effective": {
            "main": {"value": {"connection": "openrouter", "provider": "openrouter",
                               "model": "z-ai/glm-5.3-flash"}, "source": "user_default"},
            "spotter": {"value": {"connection": "typesafe", "provider": "typesafe",
                                  "model": "jev-latest"}, "source": "user_default"},
            "triage": {"value": {"mode": "shared"}, "source": "shared_main"},
            "effort": {"value": null, "source": "provider_default"},
            "temperature": {"value": null, "source": "provider_default"}
        },
        "issues": []
    }"#;
    let decoded: ResolvedOptions = serde_json::from_str(wire).expect("decode a preview");
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        serde_json::from_str::<serde_json::Value>(wire).unwrap()
    );
    assert_eq!(
        decoded.effective.triage.value,
        Some(EffectiveTriage::Shared)
    );
}

/// `Provider` writes a display name wider than its configuration vocabulary: a
/// custom endpoint spells two config keys and serializes as one word
/// (docs/transport.md). Decoding has to accept what encoding produces, or a
/// client reading `GET /inference` from a host with a custom connection fails on
/// the whole response.
#[test]
fn every_provider_display_name_decodes_back() {
    use crate::log::{Provider, WireDialect};
    for provider in [
        Provider::Anthropic,
        Provider::OpenRouter,
        Provider::OpenAI,
        Provider::OpenAICodex,
        Provider::TypeSafe,
        Provider::Custom(WireDialect::Messages),
        Provider::Custom(WireDialect::Responses),
    ] {
        let encoded = serde_json::to_string(&provider).unwrap();
        assert_eq!(
            serde_json::from_str::<Provider>(&encoded).unwrap(),
            provider,
            "{encoded} did not read back"
        );
    }
}

/// With no spotter named anywhere, a connection that reaches jev supplies it ahead
/// of the main provider's cheap band — first-party typesafe ahead of OpenRouter —
/// while a main connection's own `spotter-model` still outranks both
/// (docs/permissions.md).
#[test]
fn a_reachable_decision_model_is_the_default_spotter() {
    let spotter = |toml: &str| {
        registry(toml)
            .resolve_options(&main_options(), None, None)
            .effective
            .spotter
    };
    let main = r#"
[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#;

    let routed = spotter(&format!(
        r#"{main}
[connections.router]
provider = "openrouter"
auth = {{ kind = "env", key = "ANTHROPIC_KEY" }}
"#
    ));
    assert_eq!(routed.source, Some(SelectionSource::DecisionConnection));
    let routed = routed.value.unwrap();
    assert_eq!(
        (routed.connection.as_str(), routed.model.as_str()),
        ("router", "~typesafe/jev-latest")
    );

    let both = spotter(&format!(
        r#"{main}
[connections.a-router]
provider = "openrouter"
auth = {{ kind = "env", key = "ANTHROPIC_KEY" }}

[connections.ts]
provider = "typesafe"
auth = {{ kind = "env", key = "ANTHROPIC_KEY" }}
"#
    ))
    .value
    .unwrap();
    assert_eq!(
        (both.connection.as_str(), both.model.as_str()),
        ("ts", "jev-latest"),
        "first-party wins even when OpenRouter sorts first"
    );

    let own = spotter(&format!(
        r#"{}
spotter-model = "claude-haiku-4-5"

[connections.ts]
provider = "typesafe"
auth = {{ kind = "env", key = "ANTHROPIC_KEY" }}
"#,
        main.trim_end()
    ));
    assert_eq!(own.source, Some(SelectionSource::MainProvider));
    assert_eq!(own.value.unwrap().model, "claude-haiku-4-5");
}

/// With the host's catalog holding the provider's listing, the borrowed spotter
/// default is the newest `active` model on the `haiku` line; without it, the
/// compiled-in default (docs/model-catalog.md).
#[tokio::test]
async fn the_borrowed_spotter_default_reads_the_catalog() {
    let catalog = crate::catalog::Catalog::new(None);
    let haiku = |id: &str, created: i64| crate::api::ListedModel {
        created: Some(created),
        line: Some("haiku".into()),
        lifecycle: Some("active".into()),
        ..crate::api::ListedModel::new(id)
    };
    catalog
        .refresh_with(
            "main",
            crate::log::Provider::Anthropic,
            crate::catalog::CATALOG_TTL,
            || async {
                Ok(vec![
                    haiku("claude-haiku-4-5", 1),
                    haiku("claude-haiku-5-5", 2),
                ])
            },
        )
        .await;
    let resolved =
        one_connection()
            .with_catalog(catalog)
            .resolve_options(&main_options(), None, None);
    assert_eq!(
        resolved.effective.spotter.value.map(|s| s.model).as_deref(),
        Some("claude-haiku-5-5")
    );
    let unheld = one_connection()
        .with_catalog(crate::catalog::Catalog::new(None))
        .resolve_options(&main_options(), None, None);
    assert_eq!(
        unheld.effective.spotter.value.map(|s| s.model).as_deref(),
        Some("claude-haiku-4-5")
    );
}

/// The spotter verbs resolve the spotter alone: a config with no main default runs
/// them with a named spotter or `[defaults].spotter`, and refuses only when the
/// spotter would have fallen through to main's cheap band.
#[test]
fn the_spotter_role_resolves_without_a_main_model() {
    let (named, _) = one_connection()
        .resolve_spotter_role(Some("main/claude-sonnet-5-5"))
        .unwrap();
    assert_eq!(named.model, "claude-sonnet-5-5");

    let defaulted = registry(
        r#"
[defaults]
spotter = "main/claude-haiku-5-5"

[connections.main]
provider = "anthropic"
auth = { kind = "env", key = "ANTHROPIC_KEY" }
"#,
    );
    let (selection, _) = defaulted.resolve_spotter_role(None).unwrap();
    assert_eq!(selection.model, "claude-haiku-5-5");

    let why = one_connection()
        .resolve_spotter_role(None)
        .unwrap_err()
        .to_string();
    assert!(why.contains("[defaults].spotter"), "{why}");

    // A spotter field's own issue still refuses, named as the spotter's.
    let why = one_connection()
        .resolve_spotter_role(Some("nowhere/m"))
        .unwrap_err()
        .to_string();
    assert!(why.starts_with("spotter: "), "{why}");
}
