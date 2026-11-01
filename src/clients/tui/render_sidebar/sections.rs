//! The drawer's four sections (docs/tui.md): Session, Processes, Usage, and Policy. Each
//! draws from `y` down to the panel's bottom and returns the row the next one starts at.

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::{LABEL_W, Panel, num, put};
use crate::clients::tui::SidebarRow;
use crate::clients::tui::policy_view::PolicyView;
use crate::clients::tui::process_panel::BgProc;
use crate::clients::tui::render_frame::fmt_secs;
use crate::clients::tui::state::InteractiveState;
use crate::config::user_config::ModelSelection;
use crate::inference::TriageIdentity;
use crate::protocol::state::{KeepaliveSkip, KeepaliveSkipReason};

/// Lines the session title may wrap to before it is cut.
const TITLE_LINES: usize = 2;
/// Lines the scope sentence may wrap to before it is cut.
const SCOPE_LINES: usize = 3;

/// Greedy word wrap to `width` columns, at most `max_lines` lines; a word longer
/// than the width is cut at it. The last line keeps whatever did not fit, and
/// [`put`] clips it at the column edge.
fn wrap_words(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let fits = line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width;
        if !fits && lines.len() + 1 < max_lines {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// A role's selection as the drawer names it: `<connection>/<native model>`.
fn selection_label(selection: &ModelSelection) -> String {
    format!("{}/{}", selection.connection, selection.model)
}

/// The triage role's label: `shared` when it rides the main client.
fn triage_label(triage: &TriageIdentity) -> String {
    if triage.shared {
        "shared".to_string()
    } else {
        selection_label(&triage.selection)
    }
}

/// The drawer's wording for `usage.keepalive_skipped`: what the loop is doing in place of
/// pinging, with the figure that decided it.
fn keepalive_note(skipped: &KeepaliveSkip) -> String {
    match &skipped.why {
        KeepaliveSkipReason::Rejected { .. } => "keepalive off: ping rejected".to_string(),
        KeepaliveSkipReason::Exhausted { .. } => "keepalive paused: usage limit".to_string(),
        KeepaliveSkipReason::Discard {
            output_tokens,
            completion_usd,
            ..
        } => {
            let cost = completion_usd.map_or_default(|usd| format!(" (${usd:.4})"));
            format!("keepalive paused: {} out{cost}", num(*output_tokens))
        }
    }
}

impl<'a> InteractiveState<'a> {
    /// Session — what this session is: its title, then which model answers and where it
    /// works, from the `session_identity` topic (docs/control-api.md). Nothing in the
    /// transcript names the model. Absent until the connect snapshot lands.
    pub(super) fn render_session_section(
        &self,
        buf: &mut Buffer,
        panel: &Panel,
        mut y: u16,
    ) -> u16 {
        let Panel {
            x0,
            max_w,
            bottom,
            text,
            muted,
            header,
            ..
        } = *panel;
        if let Some(identity) = self.topics.session_identity.clone()
            && y < bottom
        {
            put(buf, x0, y, max_w, "Session", header);
            y += 1;
            // The title leads (docs/session-title.md): what the session is for.
            if let Some(title) = &identity.title {
                for line in wrap_words(title, max_w as usize, TITLE_LINES) {
                    if y < bottom {
                        put(buf, x0, y, max_w, &line, text);
                        y += 1;
                    }
                }
            }
            // Hosted: the main role is named by connection; the spotter and
            // triage rows follow, since which account answers a permission check
            // or a triage is as invisible in the transcript as the model.
            let model = identity
                .inference
                .as_ref()
                .map_or(identity.model.clone(), |roles| selection_label(&roles.main));
            let mut rows = vec![("model", model, text)];
            if let Some(roles) = &identity.inference {
                rows.push(("spotter", selection_label(&roles.spotter), text));
                rows.push(("triage", triage_label(&roles.triage), text));
            }
            // Shortened against `$HOME` here, where the process environment is read; the
            // topic carries the path as the host knows it.
            rows.push((
                "directory",
                crate::platform::home::tilde(std::path::Path::new(&identity.cwd)),
                text,
            ));
            rows.push(("session", identity.id.clone(), muted));
            for (label, value, style) in rows {
                if y < bottom {
                    panel.kv(buf, y, label, &value, style);
                    y += 1;
                }
            }
            y += 1; // section gap
        }
        y
    }

    /// Processes — one running process per two lines (handle/command, then timing).
    /// `processes` is the owned snapshot the render walk takes, since the verb menus
    /// record hits through `&mut self`.
    pub(super) fn render_processes_section(
        &mut self,
        buf: &mut Buffer,
        panel: &Panel,
        mut y: u16,
        processes: &[BgProc],
    ) -> u16 {
        let Panel {
            area,
            x0,
            max_w,
            bottom,
            text,
            muted,
            header,
            ..
        } = *panel;
        if y < bottom {
            put(
                buf,
                x0,
                y,
                max_w,
                &format!("Processes  {}", self.process_panel.len()),
                header,
            );
            y += 1;
            for p in processes {
                if y + 1 >= bottom {
                    break;
                }
                let row = SidebarRow::Process(p.process.id);
                let focused = self.sidebar_focus == Some(row);
                if focused {
                    self.paint_focus_bar(buf, area.x, y, 2);
                }
                put(
                    buf,
                    x0,
                    y,
                    max_w,
                    &format!("{}  {}", p.handle(), p.process.command),
                    text,
                );
                // Every actionable row is clickable, focused or not — the click is
                // how a row *becomes* focused.
                self.sidebar_hits.push((Rect::new(x0, y, max_w, 2), row));
                let timing = format!(
                    "{} · idle {}",
                    fmt_secs(p.running_secs),
                    fmt_secs(p.idle_secs)
                );
                put(buf, x0 + 2, y + 1, max_w.saturating_sub(2), &timing, muted);
                if focused {
                    // The verb menu trails the timing on the row's second line, so
                    // focusing never reflows the column (the block menu's rule).
                    let tx = x0 + 2 + timing.chars().count() as u16 + 2;
                    self.render_row_verbs(buf, tx, y + 1, area.x + area.width, row);
                }
                y += 2;
            }
            y += 1; // section gap
        }
        y
    }

    /// Usage — the context window, cost, and token and cache telemetry.
    pub(super) fn render_usage_section(&self, buf: &mut Buffer, panel: &Panel, mut y: u16) -> u16 {
        let Panel {
            x0,
            max_w,
            bottom,
            text,
            muted,
            header,
            ..
        } = *panel;
        if let Some(u) = self.topics.usage.clone()
            && y < bottom
        {
            put(buf, x0, y, max_w, "Usage", header);
            y += 1;
            let mut rows: Vec<(&str, String, Style)> = Vec::new();
            // Context-window occupancy leads (docs/transport.md): the percentage is
            // the "am I close?" answer; the limit alone renders before the first
            // request (occupancy unknown, not zero).
            let context = u.context.unwrap_or_default();
            if let Some(limit) = context.limit.filter(|l| *l > 0) {
                let value = match context.used {
                    Some(used) => {
                        format!("{}% · {}/{}", used * 100 / limit, num(used), num(limit))
                    }
                    None => num(limit),
                };
                rows.push(("context", value, text));
            }
            // The cold-cache warning, beside the context meter
            // (docs/log-and-projection.md): consecutive main-loop requests that read
            // nothing from the cache and had no legitimate reason to. The byte pins
            // prove the harness emits what it emitted yesterday; only this proves the
            // provider still matches it, and a moved prefix is otherwise silent —
            // every turn simply costs full price. Absent below the threshold, and it
            // clears the moment a warm read arrives (the daemon resets the count).
            if u.cold_streak >= crate::protocol::state::COLD_STREAK_WARN {
                rows.push(("", format!("⚠ cache misses ×{}", u.cold_streak), header));
            }
            // The keepalive's stance (docs/transport.md): stated only while it is not
            // pinging on schedule, so a session going cold unrefreshed is not a
            // surprise.
            if let Some(skipped) = &u.keepalive_skipped {
                rows.push(("", format!("⚠ {}", keepalive_note(skipped)), header));
            }
            // Session cost (docs/transport.md): billed dollars from the provider's
            // accounting, absent where no per-request surface exists (direct
            // Anthropic). A nonzero unpriced count marks the total an undercount.
            if let Some(cost) = &u.cost {
                let usd = cost.usd;
                let mut value = if usd >= 1.0 {
                    format!("${usd:.2}")
                } else {
                    format!("${usd:.4}")
                };
                if cost.unpriced > 0 {
                    value.push_str(&format!(" · {} unpriced", cost.unpriced));
                }
                rows.push(("cost", value, text));
            }
            rows.push((
                "tokens",
                format!(
                    "in {} · out {}",
                    num(u.cumulative.input_tokens),
                    num(u.cumulative.output_tokens)
                ),
                text,
            ));
            // Triage retention (docs/triage.md): how much of what the triage
            // showed is still on the agent's record. Absent until the first triaged
            // result — zero shown is unknown, not 0%.
            if let Some(percent) =
                (u.retention.kept_lines * 100).checked_div(u.retention.shown_lines)
            {
                rows.push((
                    "kept",
                    format!(
                        "{percent}% · {}/{} lines",
                        num(u.retention.kept_lines),
                        num(u.retention.shown_lines)
                    ),
                    text,
                ));
            }
            // A result withheld on a failed triage carries no output at all — a distinct
            // condition from a low ratio, so it is stated rather than folded into one.
            if u.retention.withheld > 0 {
                rows.push(("withheld", u.retention.withheld.to_string(), muted));
            }
            if u.cumulative.cache_read_input_tokens > 0 {
                rows.push((
                    "cache read",
                    num(u.cumulative.cache_read_input_tokens),
                    muted,
                ));
            }
            if let Some(secs) = self.since_last_request {
                rows.push(("idle", format!("{secs}s"), muted));
            }
            for (label, value, style) in rows {
                if y < bottom {
                    panel.kv(buf, y, label, &value, style);
                    y += 1;
                }
            }
            y += 1; // section gap
        }
        y
    }

    /// Policy — the effective perimeter + its pending proposals. A proposed grant
    /// renders in its committed position with a trailing `⊕` and a muted style
    /// (docs/permissions.md); committing it drops the marker in place. `policy` is the
    /// owned snapshot the render walk takes, for the reason `processes` is.
    pub(super) fn render_policy_section(
        &mut self,
        buf: &mut Buffer,
        panel: &Panel,
        mut y: u16,
        policy: Option<&PolicyView>,
    ) {
        let Panel {
            area,
            x0,
            max_w,
            vx,
            vw,
            bottom,
            text,
            muted,
            header,
        } = *panel;
        if let Some(policy) = policy {
            if y < bottom {
                put(buf, x0, y, max_w, "Policy", header);
                y += 1;
            }
            // The review-level dial (docs/tui.md) — who reviews a gated call.
            // Read-only here; set it with `efference control review-level`.
            if let Some(level) = policy.review_level
                && y < bottom
            {
                let value = match policy.resolved_review_level {
                    // The dial and the position in force disagree: say both, dial
                    // first, so what was set and who answers are both readable
                    // (docs/permissions.md).
                    Some(resolved) => format!("{} → {}", level.as_str(), resolved.as_str()),
                    None => level.as_str().to_string(),
                };
                panel.kv(buf, y, "review", &value, text);
                y += 1;
            }
            // The scope is a sentence, so it wraps in the value column, up to a few
            // lines, rather than truncating at the first.
            if let Some(scope) = &policy.scope {
                for (i, line) in wrap_words(scope, vw as usize, SCOPE_LINES)
                    .iter()
                    .enumerate()
                {
                    if y >= bottom {
                        break;
                    }
                    panel.kv(buf, y, if i == 0 { "scope" } else { "" }, line, text);
                    y += 1;
                }
            }
            if let Some((seq, t)) = &policy.scope_proposal
                && y < bottom
            {
                self.render_policy_line(
                    buf,
                    area,
                    y,
                    &format!("{t}  ⊕"),
                    SidebarRow::Proposal(*seq),
                    muted,
                );
                y += 1;
            }

            for (name, entries) in [
                ("commands", &policy.commands),
                ("paths", &policy.paths),
                ("tools", &policy.tools),
            ] {
                if entries.is_empty() || y >= bottom {
                    continue;
                }
                // The label shares its first entry's row, as in the rows above.
                put(buf, x0, y, LABEL_W, name, muted);
                for e in entries {
                    if y >= bottom {
                        break;
                    }
                    let (style, suffix) = if e.proposed {
                        (muted, "  ⊕")
                    } else {
                        (text, "")
                    };
                    let row_id = e.seq.map(SidebarRow::Proposal);
                    match row_id.filter(|_| e.proposed) {
                        Some(id) => {
                            self.render_policy_line(
                                buf,
                                area,
                                y,
                                &format!("{}{}", e.label, suffix),
                                id,
                                style,
                            );
                        }
                        None => {
                            put(buf, vx, y, vw, &format!("{}{}", e.label, suffix), style);
                        }
                    }
                    y += 1;
                }
            }
        }
    }
}
