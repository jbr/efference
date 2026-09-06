//! The right-docked sidebar (docs/tui.md): `SESSION`, `PROCESSES`, `STATS`, and
//! `POLICY`. It is chrome, not transcript: rendered directly into its `Rect` at
//! absolute coordinates, fed by the `state` topics (docs/control-api.md). A
//! 1-column `│` divides it from the transcript on its left edge.
//!
//! Sections render top-down until they run out of room. Row and verb rects are
//! recorded per frame for click dispatch (`sidebar_hits` / `sidebar_verb_hits`); the
//! verbs themselves dispatch by identity through `tui/sidebar.rs`.

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::SidebarRow;
use super::render_frame::fmt_secs;
use super::state::InteractiveState;
use super::write_text::fold_control;

/// Write `text` at absolute `(x, y)`, clipped to `max_w` columns. One cell per char
/// (wide glyphs occupy one cell, as elsewhere in this TUI's chrome). Content text
/// lands here too — a process's command line is whatever the model wrote, newlines
/// included — so control characters are folded, not written.
fn put(buf: &mut Buffer, x: u16, y: u16, max_w: u16, text: &str, style: Style) {
    for (i, ch) in text.chars().take(max_w as usize).enumerate() {
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_char(fold_control(ch)).set_style(style);
        }
    }
}

impl<'a> InteractiveState<'a> {
    pub(super) fn render_sidebar(&mut self, buf: &mut Buffer, area: Rect) {
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

        // Hits are rebuilt with the frame, like the strip's and block menu's.
        self.sidebar_hits.clear();
        self.sidebar_verb_hits.clear();

        let x0 = area.x + 2; // content start: past the divider + one space
        let max_w = area.width.saturating_sub(3); // trailing pad
        let bottom = area.y + area.height;
        let mut y = area.y;

        // Owned snapshots for the render walk: the verb menus record hits through
        // `&mut self`, which cannot run while the view structs are borrowed.
        let processes = self.processes.clone();
        let policy = self.policy.clone();

        // SESSION — which model is answering and where it is working, from the
        // `session_identity` topic (docs/control-api.md). It leads the column: it is
        // the only section that says what this session *is*, and nothing in the
        // transcript names the model. Absent until the connect snapshot lands.
        if let Some(identity) = self.session_identity.clone()
            && y < bottom
        {
            put(buf, x0, y, max_w, "▾ SESSION", header);
            y += 1;
            if y < bottom {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &identity.model,
                    text,
                );
                y += 1;
            }
            if y < bottom {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &identity.cwd,
                    muted,
                );
                y += 1;
            }
            y += 1; // section gap
        }

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
            for p in &processes {
                if y + 1 >= bottom {
                    break;
                }
                let row = SidebarRow::Process(p.id);
                let focused = self.sidebar_focus == Some(row);
                if focused {
                    self.paint_focus_bar(buf, area.x, y, 2);
                }
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("{}  {}", p.handle(), p.command),
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
                put(buf, x0 + 3, y + 1, max_w.saturating_sub(3), &timing, muted);
                if focused {
                    // The verb menu trails the timing on the row's second line, so
                    // focusing never reflows the column (the block menu's rule).
                    let tx = x0 + 3 + timing.chars().count() as u16 + 2;
                    self.render_row_verbs(buf, tx, y + 1, area.x + area.width, row);
                }
                y += 2;
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
                    Some(used) => {
                        format!("ctx {}% · {}/{}", used * 100 / limit, num(used), num(limit))
                    }
                    None => format!("ctx {}", num(limit)),
                };
                put(buf, x0 + 1, y, max_w.saturating_sub(1), &line, text);
                y += 1;
            }
            // The cold-cache warning, beside the context meter
            // (docs/log-and-projection.md): consecutive main-loop requests that read
            // nothing from the cache and had no legitimate reason to. The byte pins
            // prove the harness emits what it emitted yesterday; only this proves the
            // provider still matches it, and a moved prefix is otherwise silent —
            // every turn simply costs full price. Absent below the threshold, and it
            // clears the moment a warm read arrives (the daemon resets the count).
            if u.cold_streak >= crate::daemon::COLD_STREAK_WARN && y < bottom {
                put(
                    buf,
                    x0 + 1,
                    y,
                    max_w.saturating_sub(1),
                    &format!("⚠ cache cold ×{}", u.cold_streak),
                    self.theme
                        .status_style
                        .add_modifier(ratatui::style::Modifier::BOLD),
                );
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
        if let Some(policy) = &policy {
            if y < bottom {
                put(buf, x0, y, max_w, "▾ POLICY", header);
                y += 1;
            }
            // The review-level dial (docs/tui.md) — who reviews a
            // gated call. Read-only here; set it with `efference control review-level` (the interactive
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
            if let Some((seq, t)) = &policy.scope_proposal
                && y < bottom
            {
                self.render_policy_line(
                    buf,
                    area,
                    y,
                    &format!("scope  {t}  ⊕"),
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
                put(buf, x0 + 1, y, max_w.saturating_sub(1), name, muted);
                y += 1;
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
                            put(
                                buf,
                                x0 + 3,
                                y,
                                max_w.saturating_sub(3),
                                &format!("{}{}", e.label, suffix),
                                style,
                            );
                        }
                    }
                    y += 1;
                }
            }
        }
    }

    /// One actionable POLICY line: the label, plus — when it is the focused row —
    /// the `▌` bar and the verb menu trailing it, truncated from the label side so
    /// the verbs (what `⏎` fires) always render whole. Records the row hit rect.
    /// Content geometry (`x0`, `max_w`) derives from the panel rect, as everywhere
    /// else in this module.
    fn render_policy_line(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        label: &str,
        row: SidebarRow,
        style: Style,
    ) {
        let x0 = area.x + 2;
        let max_w = area.width.saturating_sub(3);
        let focused = self.sidebar_focus == Some(row);
        // Actionable rows are clickable focused or not — the click is how a row
        // *becomes* focused.
        self.sidebar_hits
            .push((Rect::new(x0 + 3, y, max_w - 3, 1), row));
        if !focused {
            put(buf, x0 + 3, y, max_w.saturating_sub(3), label, style);
            return;
        }
        self.paint_focus_bar(buf, area.x, y, 1);
        // The menu's width claims the line first; the label gets what remains.
        let menu_w: u16 = self
            .sidebar_verbs(row)
            .iter()
            .map(|v| 3 + v.label().chars().count() as u16 + 1)
            .sum();
        let label_room = max_w.saturating_sub(3 + menu_w + 1);
        put(buf, x0 + 3, y, label_room, label, style);
        let tx = x0 + 3 + label_room + 1;
        self.render_row_verbs(buf, tx, y, area.x + area.width, row);
    }

    /// The focused row's left-edge bar: a `▌` per line in the pad column between
    /// the divider and the content — the transcript unit marker, one column over.
    fn paint_focus_bar(&self, buf: &mut Buffer, x: u16, y: u16, height: u16) {
        for dy in 0..height {
            if let Some(cell) = buf.cell_mut((x + 1, y + dy)) {
                cell.set_char('▌')
                    .set_style(self.theme.sidebar_focus_bar_style);
            }
        }
    }

    /// A focused row's verb menu: each verb as `▸ label` with a reserved marker
    /// cell (the block menu's grammar), the selected one in the strip's focused
    /// style. Records each verb's rect for click dispatch; verbs cut from the
    /// right at the panel edge are simply not offered — the `←`/`→` selection
    /// clamps to what is on screen.
    fn render_row_verbs(
        &mut self,
        buf: &mut Buffer,
        mut x: u16,
        y: u16,
        max_x: u16,
        row: SidebarRow,
    ) {
        let selected = self.sidebar_verb_focus;
        for (i, verb) in self.sidebar_verbs(row).into_iter().enumerate() {
            let text = format!(
                "{} {}",
                if i == selected { "▸" } else { "  " },
                verb.label()
            );
            let w = text.chars().count() as u16;
            if x + w > max_x {
                break;
            }
            let style = if i == selected {
                self.theme.strip_item_focused_style
            } else {
                self.theme.strip_item_style
            };
            put(buf, x, y, w, &text, style);
            self.sidebar_verb_hits.push((Rect::new(x, y, w, 1), row, i));
            x += w + 1;
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
