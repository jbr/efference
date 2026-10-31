//! Keys in the two modes that own the keyboard outright: the theme picker, and the composer
//! (docs/tui.md).

use std::io::Write;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, prelude::Backend};

use super::{InteractiveState, UiMode};
use crate::tui::composer::ComposerAction;
use crate::tui::render_context::RenderContext;

impl<'a> InteractiveState<'a> {
    /// Keys in the theme picker: `↑`/`↓` (and `k`/`j`) step through the themes and apply the one
    /// landed on, `⏎` keeps it.
    pub(super) fn handle_theme_picker_key(&mut self, key: KeyEvent) {
        let UiMode::ThemePicker {
            ref mut selected_index,
            ..
        } = self.ui_mode
        else {
            return;
        };
        let themes = RenderContext::available_themes();
        let theme_count = themes.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if *selected_index > 0 => {
                *selected_index -= 1;
                if let Some(theme_name) = themes.get(*selected_index) {
                    let _ = self.apply_theme(theme_name);
                }
            }
            KeyCode::Down | KeyCode::Char('j') if *selected_index + 1 < theme_count => {
                *selected_index += 1;
                if let Some(theme_name) = themes.get(*selected_index) {
                    let _ = self.apply_theme(theme_name);
                }
            }
            KeyCode::Enter => {
                let theme_name = self
                    .current_theme_name
                    .clone()
                    .unwrap_or_else(|| "default".into());
                self.ui_mode = UiMode::Normal;
                self.ui.debug_message = format!("Theme saved: {theme_name}").into();
            }
            _ => {}
        }
    }

    /// Keys while the composer has focus. A key the composer doesn't want (PgUp/PgDn) still
    /// scrolls the transcript so you can read while typing. Returns whether the operator quit.
    pub(super) fn handle_composing_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<impl Backend + Write>,
    ) -> bool {
        // C-d on an empty composer quits, shell/REPL EOF style (the composer
        // otherwise binds C-d to delete-forward, so the empty-buffer guard is
        // what disambiguates "delete the char ahead" from "nothing left — exit").
        if key.code == KeyCode::Char('d')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.composer.is_empty()
        {
            return true;
        }

        // While a gate is pending, Enter always resolves it — even on an empty
        // buffer (= approve), which the composer would otherwise swallow.
        if self.topics.gate.is_some()
            && key.code == KeyCode::Enter
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            let text = self.composer.text();
            self.submit_composer(text);
            return false;
        }
        // Empty composer, operator's turn: `⏎` hands control back with no input
        // (docs/tui.md) — the zero-token continue, which the composer would
        // otherwise swallow as a no-op. Sent where the `verbs` topic says the
        // route accepts it (an idle loop with a continue in it, a cold park, a
        // wait on the upstream that it retries now); refused at idle, the route's
        // reason goes to the status line, and a stray `⏎` during a generation
        // stays the silent no-op it has always been. The daemon re-checks and
        // declines a race.
        if key.code == KeyCode::Enter
            && !key.modifiers.contains(KeyModifiers::ALT)
            && self.composer.is_empty()
            && !self.read_only()
        {
            // Staged attachments are a turn on their own; with none, an idle loop takes
            // the continue.
            if !self.staged.is_empty() {
                self.submit_composer(String::new());
                return false;
            }
            match self
                .topics
                .verbs
                .as_ref()
                .map(|verbs| verbs.continue_verb.refusal().map(str::to_string))
            {
                Some(None) => {
                    self.continue_trajectory();
                    return false;
                }
                Some(Some(refusal)) if !self.is_loop_active() => {
                    self.ui.debug_message = refusal.into();
                    self.dirty = true;
                    return false;
                }
                // Before the topic lands, an idle loop takes the continue and the
                // daemon answers for itself.
                None if !self.is_loop_active() => {
                    self.continue_trajectory();
                    return false;
                }
                _ => {}
            }
        }
        // `⌫` on an empty composer takes back the most recently staged attachment.
        if key.code == KeyCode::Backspace
            && key.modifiers.is_empty()
            && self.composer.is_empty()
            && let Some(key) = self.staged.last_key()
        {
            self.remove_staged(key);
            return false;
        }
        match self.handle_composer_key(key) {
            ComposerAction::Submit(text) => self.submit_composer(text),
            ComposerAction::Consumed => {}
            // The `↑`-spill at the 0,0 detent lands on the control strip —
            // the middle stop of the transcript → strip → composer axis
            // (docs/tui.md). `↑` again from the strip enters the transcript.
            ComposerAction::SpillUp => self.focus_strip_from_composer(),
            ComposerAction::Ignored => self.handle_scroll_key(key, terminal),
        }
        false
    }
}
