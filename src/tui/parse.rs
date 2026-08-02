//! Decoding `GET /state` into the typed views the sidebar and status bar render.

use super::*;

/// Parse `GET /state`'s `processes` array into the live [`BgProc`] list (docs/supervisor.md).
/// A missing/malformed field is an empty list — the panel just hides.
pub(super) fn parse_processes(body: &str) -> Vec<BgProc> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("processes").cloned())
        .and_then(|p| serde_json::from_value::<Vec<BgProc>>(p).ok())
        .unwrap_or_default()
}

/// Parse `GET /state`'s `pending_approval` object into a [`PendingGate`].
pub(super) fn parse_pending_gate(body: &str) -> Option<PendingGate> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let pending = value.get("pending_approval")?;
    if pending.is_null() {
        return None;
    }
    Some(PendingGate {
        call_id: pending.get("call_id")?.as_str()?.to_string(),
        name: pending.get("name")?.as_str()?.to_string(),
        input: pending
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        reason: pending
            .get("reason")
            .and_then(|r| r.as_str())
            .map(String::from),
    })
}

/// Parse `GET /state`'s `usage` object into a [`UsageStats`].
/// Filesystem perms → a compact tag for the POLICY readout.
pub(super) fn perm_str(read: bool, write: bool) -> &'static str {
    match (read, write) {
        (_, true) => "rw",
        (true, false) => "r",
        (false, false) => "-",
    }
}

/// Decode `GET /state`'s `role` (the effective policy) + `proposals` into a [`PolicyView`]
/// for the sidebar (docs/permissions.md). Proposals are merged into their section and
/// sorted alongside the committed grants, flagged `proposed`, so a commit drops the flag
/// in place without reflow. `ConfigChange` is externally tagged
/// (`{"Command":{"command":…}}`, `{"Path":{…}}`, `{"Tool":{"name":…}}`, `{"Scope":{"text":…}}`).
pub(super) fn parse_policy(body: &str) -> Option<PolicyView> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let role = v.get("role")?;

    let str_entries = |key: &str| -> Vec<PolicyEntry> {
        role.get(key)
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| PolicyEntry {
                        label: s.to_string(),
                        proposed: false,
                        seq: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut commands = str_entries("commands");
    let mut tools = str_entries("tools");
    let mut paths: Vec<PolicyEntry> = role
        .get("paths")
        .and_then(|p| p.as_object())
        .map(|m| {
            m.iter()
                .map(|(path, perms)| {
                    let r = perms.get("read").and_then(|b| b.as_bool()).unwrap_or(false);
                    let w = perms
                        .get("write")
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false);
                    PolicyEntry {
                        label: format!("{path}  {}", perm_str(r, w)),
                        proposed: false,
                        seq: None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let mut scope_proposal = None;
    if let Some(arr) = v.get("proposals").and_then(|p| p.as_array()) {
        for prop in arr {
            let (Some(seq), Some(change)) =
                (prop.get("seq").and_then(|s| s.as_u64()), prop.get("change"))
            else {
                continue;
            };
            if let Some(c) = change
                .get("Command")
                .and_then(|c| c.get("command"))
                .and_then(|s| s.as_str())
            {
                commands.push(PolicyEntry {
                    label: c.to_string(),
                    proposed: true,
                    seq: Some(seq),
                });
            } else if let Some(p) = change.get("Path") {
                let path = p.get("path").and_then(|s| s.as_str()).unwrap_or("");
                let r = p.get("read").and_then(|b| b.as_bool()).unwrap_or(false);
                let w = p.get("write").and_then(|b| b.as_bool()).unwrap_or(false);
                paths.push(PolicyEntry {
                    label: format!("{path}  {}", perm_str(r, w)),
                    proposed: true,
                    seq: Some(seq),
                });
            } else if let Some(n) = change
                .get("Tool")
                .and_then(|t| t.get("name"))
                .and_then(|s| s.as_str())
            {
                tools.push(PolicyEntry {
                    label: n.to_string(),
                    proposed: true,
                    seq: Some(seq),
                });
            } else if let Some(t) = change
                .get("Scope")
                .and_then(|s| s.get("text"))
                .and_then(|s| s.as_str())
            {
                scope_proposal = Some((seq, t.to_string()));
            }
        }
    }

    // Sort each section so a proposed entry sits in its committed position (no reflow on commit).
    commands.sort_by(|a, b| a.label.cmp(&b.label));
    paths.sort_by(|a, b| a.label.cmp(&b.label));
    tools.sort_by(|a, b| a.label.cmp(&b.label));

    Some(PolicyView {
        scope: role.get("scope").and_then(|s| s.as_str()).map(String::from),
        scope_proposal,
        commands,
        paths,
        tools,
        // `review_level` sits at the top level of `/state`, not under `role`.
        review_level: v
            .get("review_level")
            .and_then(|l| l.as_str())
            .map(String::from),
    })
}

/// Parse `GET /state`'s `running` bool (docs/tui.md) — is the loop executing a turn.
pub(super) fn parse_running(body: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get("running")?.as_bool()
}

/// Parse `GET /state`'s `pace` object into a [`PaceView`] (docs/tui.md).
pub(super) fn parse_pace(body: &str) -> Option<PaceView> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let pace = value.get("pace")?;
    Some(PaceView {
        speed_ms: pace.get("speed_ms")?.as_u64()?,
        held: pace.get("held")?.as_bool()?,
    })
}

/// Parse the optional last non-success upstream response from `GET /state`.
pub(super) fn parse_upstream_failure(body: &str) -> Option<UpstreamFailureView> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let failure = value.get("upstream_failure")?;
    if failure.is_null() {
        return None;
    }
    Some(UpstreamFailureView {
        status: u16::try_from(failure.get("status")?.as_u64()?).ok()?,
        count: u32::try_from(failure.get("count")?.as_u64()?).ok()?,
    })
}

pub(super) fn parse_usage(body: &str) -> Option<UsageStats> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let usage = value.get("usage")?;
    if usage.is_null() {
        return None;
    }
    let cumulative = usage.get("cumulative")?;
    // Context-window occupancy (docs/transport.md): `context` is null when the
    // daemon fetched no capabilities; `used` is null before the first request.
    let context = usage.get("context").filter(|c| !c.is_null());
    // Session cost: null when the provider has no per-request accounting surface.
    let cost = usage.get("cost").filter(|c| !c.is_null());
    // Triage retention (docs/redaction-fork.md). Absent on a daemon predating the
    // field; a missing counter reads as 0, which the sidebar renders as absent.
    let retention = |field: &str| {
        usage
            .get("retention")
            .and_then(|r| r.get(field))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };
    Some(UsageStats {
        input_tokens: cumulative.get("input_tokens")?.as_u64()?,
        output_tokens: cumulative.get("output_tokens")?.as_u64()?,
        cache_read_input_tokens: cumulative.get("cache_read_input_tokens")?.as_u64()?,
        cache_creation_input_tokens: cumulative.get("cache_creation_input_tokens")?.as_u64()?,
        seconds_since_last_request: usage
            .get("seconds_since_last_request")
            .and_then(|v| v.as_u64()),
        context_limit: context
            .and_then(|c| c.get("limit"))
            .and_then(|v| v.as_u64()),
        context_used: context.and_then(|c| c.get("used")).and_then(|v| v.as_u64()),
        cost_usd: cost.and_then(|c| c.get("usd")).and_then(|v| v.as_f64()),
        cost_unpriced: cost.and_then(|c| c.get("unpriced")).and_then(|v| v.as_u64()),
        kept_lines: retention("kept_lines"),
        shown_lines: retention("shown_lines"),
        withheld_results: retention("withheld"),
    })
}
