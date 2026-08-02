//! The right-docked sidebar (docs/tui.md): glanceable ambient widgets that used to
//! be crammed elsewhere — `PROCESSES` (the bottom strip), `STATS` (the status bar),
//! and `POLICY` (the live perimeter + its pending proposals). It is chrome, not
//! transcript: rendered directly into its `Rect` at absolute coordinates, fed by the
//! `/state` poll. A 1-column `│` divides it from the transcript on its left edge.
//!
//! Sections render top-down until they run out of room; this first cut is a pure
//! readout (focus into the sidebar + proposal commit/cancel are later steps).

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::PolicyEntry;
use super::render_frame::fmt_secs;
use super::state::InteractiveState;

/// Write `text` at absolute `(x, y)`, clipped to `max_w` columns. One cell per char
/// (wide glyphs occupy one cell, as elsewhere in this TUI's chrome).
fn put(buf: &mut Buffer, x: u16, y: u16, max_w: u16, text: &str, style: Style) {
    for (i, ch) in text.chars().take(max_w as usize).enumerate() {
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_char(ch).set_style(style);
        }
    }
}

impl<'a> InteractiveState<'a> {
    pub(super) fn render_sidebar(&self, buf: &mut Buffer, area: Rect) {
        let bg = self.theme.document_bg_style;
        let header = self.theme.breadcrumb_current_style;
        let muted = self.theme.muted_style;
        let text = self.theme.status_style;

        // Clear the panel and paint the left divider column.
        for dy in 0..area.height {
            for dx in 0..area.width {
                if let Some(cell) = buf.cell_mut((area.x + dx, area.y + dy)) {
                    cell.reset();
                    cell.set_style(bg);
                }
            }
            if let Some(cell) = buf.cell_mut((area.x, area.y + dy)) {
                cell.set_char('│').set_style(muted);
            }
        }

        let x0 = area.x + 2; // content start: past the divider + one space
        let max_w = area.width.saturating_sub(3); // trailing pad
        let bottom = area.y + area.height;
        let mut y = area.y;

        // PROCESSES — one running process per two lines (handle/command, then timing).
        if y < bottom {
            put(
                buf,
                x0,
                y,
                max_w,
                &format!("▾ PROCESSES  {}", self.processes.len()),
                header,
            );
            y += 1;
            for p in &self.processes {
                if y >= bottom {
                    break;
                }
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("{}  {}", p.handle, p.command),
                    text,
                );
                y += 1;
                if y >= bottom {
                    break;
                }
                put(
                    buf,
                    x0 + 3,
                    y,
                    max_w.saturating_sub(3),
                    &format!(
                        "{} · idle {}",
                        fmt_secs(p.running_secs),
                        fmt_secs(p.idle_secs)
                    ),
                    muted,
                );
                y += 1;
            }
            y += 1; // section gap
        }

        // STATS — cumulative token + cache telemetry, moved off the status bar.
        if let Some(u) = &self.usage
            && y < bottom
        {
            put(buf, x0, y, max_w, "▾ STATS", header);
            y += 1;
            // Context-window occupancy leads (docs/transport.md): the percentage is
            // the "am I close?" answer; the limit alone renders before the first
            // request (occupancy unknown, not zero).
            if let Some(limit) = u.context_limit.filter(|l| *l > 0)
                && y < bottom
            {
                let line = match u.context_used {
                    Some(used) => format!(
                        "ctx {}% · {}/{}",
                        used * 100 / limit,
                        num(used),
                        num(limit)
                    ),
                    None => format!("ctx {}", num(limit)),
                };
                put(buf, x0 + 1, y, max_w.saturating_sub(1), &line, text);
                y += 1;
            }
            // Session cost (docs/transport.md): billed dollars from the provider's
            // accounting, absent where no per-request surface exists (direct
            // Anthropic). A nonzero unpriced count marks the total an undercount.
            if let Some(usd) = u.cost_usd
                && y < bottom
            {
                let mut line = if usd >= 1.0 {
                    format!("cost ${usd:.2}")
                } else {
                    format!("cost ${usd:.4}")
                };
                if let Some(n) = u.cost_unpriced.filter(|n| *n > 0) {
                    line.push_str(&format!(" · {n} unpriced"));
                }
                put(buf, x0 + 1, y, max_w.saturating_sub(1), &line, text);
                y += 1;
            }
            if y < bottom {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("in {} · out {}", num(u.input_tokens), num(u.output_tokens)),
                    text,
                );
                y += 1;
            }
            // Triage retention (docs/redaction-fork.md): how much of what the gate
            // showed is still on the agent's record. Absent until the first distilled
            // result — zero shown is unknown, not 0%.
            if u.shown_lines > 0 && y < bottom {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!(
                        "kept {}% · {}/{} lines",
                        u.kept_lines * 100 / u.shown_lines,
                        num(u.kept_lines),
                        num(u.shown_lines)
                    ),
                    text,
                );
                y += 1;
            }
            // A failed gate withholds its result's output entirely — a distinct
            // condition from a low ratio, so it is stated rather than folded into one.
            if u.withheld_results > 0 && y < bottom {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("{} withheld", u.withheld_results),
                    muted,
                );
                y += 1;
            }
            if u.cache_read_input_tokens > 0 && y < bottom {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("cache read {}", num(u.cache_read_input_tokens)),
                    muted,
                );
                y += 1;
            }
            if let Some(secs) = u.seconds_since_last_request
                && y < bottom
            {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("idle {}s", secs),
                    muted,
                );
                y += 1;
            }
            y += 1; // section gap
        }

        // POLICY — the effective perimeter + its pending proposals. A proposed grant
        // renders in its committed position with a trailing `⊕` and a muted style
        // (docs/permissions.md); committing it drops the marker in place.
        if let Some(policy) = &self.policy {
            if y < bottom {
                put(buf, x0, y, max_w, "▾ POLICY", header);
                y += 1;
            }
            // The review-level dial (docs/tui.md) — who reviews a
            // gated call. Read-only here; set it with `efference review-level` (the interactive
            // gear selector is deferred to the ergonomics pass).
            if let Some(level) = &policy.review_level
                && y < bottom
            {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("review  {level}"),
                    text,
                );
                y += 1;
            }
            if let Some(scope) = &policy.scope
                && y < bottom
            {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("scope  {scope}"),
                    text,
                );
                y += 1;
            }
            if let Some((_, t)) = &policy.scope_proposal
                && y < bottom
            {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("scope  {t}  ⊕"),
                    muted,
                );
                y += 1;
            }

            let section = |buf: &mut Buffer, y: &mut u16, name: &str, entries: &[PolicyEntry]| {
                if entries.is_empty() || *y >= bottom {
                    return;
                }
                put(buf, x0 + 1, *y, max_w.saturating_sub(1), name, muted);
                *y += 1;
                for e in entries {
                    if *y >= bottom {
                        break;
                    }
                    let (style, suffix) = if e.proposed {
                        (muted, "  ⊕")
                    } else {
                        (text, "")
                    };
                    put(
                        buf,
                        x0 + 3,
                        *y,
                        max_w.saturating_sub(3),
                        &format!("{}{}", e.label, suffix),
                        style,
                    );
                    *y += 1;
                }
            };
            section(buf, &mut y, "commands", &policy.commands);
            section(buf, &mut y, "paths", &policy.paths);
            section(buf, &mut y, "tools", &policy.tools);
        }
    }
}

/// Compact a token count: `12k` / `1m` / `486`.
fn num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}m", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}
