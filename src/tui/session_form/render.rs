//! The form's appearance, shared by both terminal containers (docs/tui.md).
//!
//! The visual idiom is `efference setup`'s picker (`setup/terminal.rs`): a
//! `?`-marked question, a `❯` cursor colouring the selected row rather than a
//! highlight bar across it, a dim trailing column, and a dim `·`-separated key
//! hint. The two are different rendering stacks — the picker writes crossterm
//! directly, this is ratatui — so what they share is the vocabulary, kept aligned
//! deliberately because `efference tui` shows them back to back.
//!
//! Only the surface differs between this form's own two containers, and it has
//! to: inline over shell scrollback paints nothing, while the embedded modal
//! paints and borders because a transcript is underneath it.
//!
//! Every field renders two things: the value a session would actually get, and
//! where it came from. That pairing is the reason the effective view exists —
//! an inherited default and an explicit override display the same string
//! otherwise, and the two readings of a shared triage (nothing configured,
//! versus an override of a configured default) are indistinguishable without it
//! (docs/control-api.md).
//!
//! A field with an issue is the third case, and it is not "no value": it has
//! neither a value nor a source, and the host's message replaces the source
//! column rather than sitting beside it.

use super::*;
use crate::inference::{EffectiveTriage, ModelSelection, SelectionSource};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

#[derive(Clone, Copy)]
pub(in crate::tui) struct Styles {
    /// The surface behind the form. `Style::default()` leaves the terminal's own
    /// background showing, which is what the inline container wants; the
    /// embedded modal passes an opaque style because a transcript is under it.
    pub text: Style,
    pub title: Style,
    pub hint: Style,
    pub selected: Style,
}

/// One row: what it is, what it resolved to, and the dim trailing column saying
/// where that came from — or, for a flagged field, why it has nothing.
struct Row {
    label: String,
    value: String,
    trailer: String,
    /// A field the host flagged, or a connection it cannot use. Rendered dim,
    /// like the picker's unavailable choice.
    flagged: bool,
}

impl Row {
    fn plain(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            trailer: String::new(),
            flagged: false,
        }
    }
}

/// How each precedence branch reads in the trailing column. Plain descriptions
/// of where the value came from, in the operator's terms rather than the enum's.
fn source_label(source: SelectionSource) -> &'static str {
    match source {
        SelectionSource::Request => "you set",
        SelectionSource::History => "recent pick",
        SelectionSource::UserDefault => "your default",
        SelectionSource::MainProvider => "main's provider",
        SelectionSource::DecisionConnection => "decision model",
        SelectionSource::ProjectConfig => "project config",
        SelectionSource::ProviderDefault => "provider default",
        // Not "shares main": the triage row already renders that as its value,
        // and what this column has to add is *why* — nothing named a separate
        // client, as against an explicit choice to share, which reads "you set".
        SelectionSource::SharedMain => "nothing configured",
    }
}

impl Form {
    /// How tall the form can ever get: the most rows any page it can reach would
    /// draw, plus the question, the status line, and the key hint.
    ///
    /// The tallest page rather than the current one because an inline viewport's
    /// height is fixed at creation — ratatui's `resize` reads its argument as the
    /// *terminal* size for `Viewport::Inline`, not the viewport's — so a
    /// container that sized itself to the form page would clip the connection
    /// list. Bounded by what this host actually offers, so it is far tighter than
    /// a constant picked for the worst imaginable case.
    pub(in crate::tui) fn viewport_height(&self) -> u16 {
        let connections = self.connections().len().max(1);
        let rows = [
            FORM_ROWS,
            // A model menu lists what the provider offers; taller ones scroll.
            MENU_ROWS,
            // Triage offers two leading choices; the others one.
            connections + 2,
            EFFORTS.len(),
        ]
        .into_iter()
        .max()
        .unwrap_or(FORM_ROWS);
        u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(3)
    }

    pub(in crate::tui) fn render(&self, buf: &mut Buffer, area: Rect, styles: Styles) {
        buf.set_style(area, styles.text);
        let (title, rows, selected) = self.content();
        // Packed to the top, with the slack below the hint rather than above it.
        // The viewport is sized for the tallest page this form can reach, so a
        // shorter page has rows to spare; putting them between the fields and the
        // hint would leave the hint floating away from what it describes.
        let layout = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(u16::try_from(rows.len()).unwrap_or(u16::MAX)),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
        Paragraph::new(Line::from(vec![
            Span::styled("? ", styles.selected.add_modifier(Modifier::BOLD)),
            Span::styled(title, styles.title.add_modifier(Modifier::BOLD)),
        ]))
        .render(layout[0], buf);

        // Both columns are as wide as their widest entry, like the picker's, so
        // the trailing column lines up without a hard-coded width that a longer
        // label would silently overrun.
        let width = |pick: fn(&Row) -> &String| {
            rows.iter()
                .map(|row| pick(row).chars().count())
                .max()
                .unwrap_or(0)
        };
        let label_width = width(|row| &row.label);
        let value_width = width(|row| &row.value);
        // A menu can list more rows than the viewport (fixed at creation) holds,
        // so the window follows the selection rather than clipping the tail.
        let capacity = usize::from(layout[1].height).max(1);
        let first = (selected + 1).saturating_sub(capacity);
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(first)
            .take(capacity)
            .map(|(index, row)| {
                let body = match row.value.is_empty() && row.trailer.is_empty() {
                    true => row.label.clone(),
                    false => format!("{:label_width$}  {:value_width$}", row.label, row.value),
                };
                let body_style = match (row.flagged, index == selected) {
                    (true, _) => styles.hint,
                    (false, true) => styles.selected,
                    (false, false) => styles.text,
                };
                let mut spans = vec![
                    Span::styled(if index == selected { "❯ " } else { "  " }, styles.selected),
                    Span::styled(body, body_style),
                ];
                if !row.trailer.is_empty() {
                    spans.push(Span::styled(format!("  {}", row.trailer), styles.hint));
                }
                Line::from(spans)
            })
            .collect();
        Paragraph::new(lines).render(layout[1], buf);

        // Partway down a scrolled menu, the position outranks the preview
        // chatter; a note or the loading line still says something more urgent.
        let scrolled = rows.len() > capacity && self.note.is_none();
        let status = match scrolled && !matches!(self.page, Page::Models { loading: true, .. }) {
            true => format!("{}/{}", selected + 1, rows.len()),
            false => self.status(),
        };
        Paragraph::new(format!("  {status}"))
            .style(styles.hint)
            .render(layout[2], buf);
        Paragraph::new(format!("  {}", self.keys()))
            .style(styles.hint)
            .render(layout[3], buf);
    }

    /// The line under the rows: what the form last had to say, else what it is
    /// waiting for. A preview in flight and no host at all are different states,
    /// and an all-`—` column means the second one when nothing was ever sent.
    fn status(&self) -> String {
        if matches!(self.page, Page::Models { loading: true, .. }) && self.note.is_none() {
            return "Loading...".into();
        }
        match (&self.note, self.pending.is_some(), &self.resolved) {
            (Some(note), _, _) => note.clone(),
            (None, true, _) => "resolving this draft with the host…".into(),
            (None, false, None) => "no host is resolving this draft".into(),
            (None, false, Some(_)) => String::new(),
        }
    }

    fn keys(&self) -> &'static str {
        match self.page {
            Page::Form => "↑↓ move · enter picks · r resets · esc cancels",
            Page::Text { .. } | Page::Temperature { .. } => {
                "type a value · enter accepts · esc backs out"
            }
            _ => "↑↓ move · enter picks · esc backs out",
        }
    }

    /// The host's message for a request field, when it named one. A field with
    /// an issue has no source to report, so this is what replaces it.
    fn issue(&self, fields: &[&str]) -> Option<&str> {
        self.resolved.as_ref()?.issues.iter().find_map(|issue| {
            fields
                .contains(&issue.field.as_ref())
                .then_some(issue.message.as_str())
        })
    }

    /// One form row. `fields` are the request fields whose issues attach to this
    /// control — a role has two, since naming a connection and naming a model
    /// fail separately (docs/control-api.md).
    fn row(
        &self,
        label: &str,
        fields: &[&str],
        value: Option<String>,
        source: Option<&str>,
    ) -> Row {
        match self.issue(fields) {
            Some(message) => Row {
                label: label.to_string(),
                value: "—".into(),
                trailer: message.to_string(),
                flagged: true,
            },
            None => Row {
                label: label.to_string(),
                value: value.unwrap_or_else(|| "—".into()),
                trailer: source.unwrap_or_default().to_string(),
                flagged: false,
            },
        }
    }

    /// Read one effective field out of the host's last answer, as the value's
    /// rendering plus its source label. `None` for the whole pair while no
    /// preview has landed yet.
    fn effective<T>(
        &self,
        pick: impl Fn(&crate::inference::EffectiveOptions) -> &crate::inference::Effective<T>,
        show: impl Fn(&T) -> String,
    ) -> (Option<String>, Option<&'static str>) {
        let Some(resolved) = &self.resolved else {
            return (None, None);
        };
        let effective = pick(&resolved.effective);
        (
            effective.value.as_ref().map(show),
            effective.source.map(source_label),
        )
    }

    fn content(&self) -> (String, Vec<Row>, usize) {
        match &self.page {
            Page::Form => self.form_page(),
            Page::Connections { role, selected } => {
                // Triage has no configured default: its inherit row shares the
                // main client, which is what the session gets.
                let mut rows = match role {
                    Role::Triage => vec![Row::plain("share the main client")],
                    _ => vec![Row::plain("use the default")],
                };
                rows.extend(self.picker_recents(*role).iter().map(|pick| Row {
                    label: format!("{}/{}", pick.connection, pick.model),
                    value: String::new(),
                    trailer: "recent".into(),
                    flagged: false,
                }));
                if self.connections().is_empty() {
                    rows.push(Row::plain("(no connections read from this host)"));
                } else {
                    rows.extend(self.connections().iter().map(|row| Row {
                        label: row.name.clone(),
                        value: row.provider.to_string(),
                        trailer: match row.available {
                            true => String::new(),
                            false => "unavailable".into(),
                        },
                        flagged: !row.available,
                    }));
                }
                (format!("{} — connection", role.label()), rows, *selected)
            }
            Page::Models {
                role,
                connection,
                models,
                selected,
                ..
            } => {
                let mut rows: Vec<Row> = models
                    .iter()
                    .map(|suggestion| Row::plain(&suggestion.model))
                    .collect();
                rows.push(Row::plain("Type a model id"));
                (
                    format!(
                        "{} on {} — models",
                        role.label(),
                        self.connection_name(*connection)
                    ),
                    rows,
                    *selected,
                )
            }
            Page::Text {
                role,
                connection,
                value,
            } => (
                format!(
                    "{} on {} — provider-native model ID",
                    role.label(),
                    self.connection_name(*connection)
                ),
                vec![Row::plain(value.with_cursor())],
                0,
            ),
            Page::Effort { selected } => (
                "main model — effort".into(),
                EFFORTS.iter().map(|s| Row::plain(*s)).collect(),
                *selected,
            ),
            Page::Temperature { value } => (
                format!(
                    "main model — temperature, {}–{} or empty for the provider's",
                    crate::inference::TEMPERATURE.start(),
                    crate::inference::TEMPERATURE.end()
                ),
                vec![Row::plain(value.with_cursor())],
                0,
            ),
        }
    }

    fn connection_name(&self, connection: usize) -> String {
        self.connections()
            .get(connection)
            .map(|row| row.name.clone())
            .unwrap_or_default()
    }

    fn form_page(&self) -> (String, Vec<Row>, usize) {
        let selection = |s: &ModelSelection| format!("{}/{}", s.connection, s.model);
        let (main, main_source) = self.effective(|e| &e.main, selection);
        let (spotter, spotter_source) = self.effective(|e| &e.spotter, selection);
        let (triage, triage_source) = self.effective(
            |e| &e.triage,
            |t| match t {
                EffectiveTriage::Shared => "the main client".to_string(),
                EffectiveTriage::Separate { selection: s, .. } => {
                    format!("{}/{}", s.connection, s.model)
                }
            },
        );
        let (effort, effort_source) = self.effective(|e| &e.effort, Clone::clone);
        let (temperature, temperature_source) = self.effective(|e| &e.temperature, u8::to_string);
        (
            format!("new session in {}", tilde(&self.context.cwd)),
            vec![
                Row::plain(match self.creating {
                    true => "creating…",
                    false => "start session",
                }),
                self.row(
                    Role::Main.label(),
                    &["model", "connection"],
                    main,
                    main_source,
                ),
                self.row("effort", &["effort"], effort, effort_source),
                self.row(
                    "temperature",
                    &["temperature"],
                    temperature,
                    temperature_source,
                ),
                self.row(
                    Role::Spotter.label(),
                    &["spotter_model", "spotter_connection"],
                    spotter,
                    spotter_source,
                ),
                self.row(Role::Triage.label(), &["triage"], triage, triage_source),
            ],
            self.selected,
        )
    }
}

/// `~`-shorten a path under the home directory, as the picker's location line
/// does: the working root is the longest string in the question and the home
/// prefix carries no information.
fn tilde(path: &std::path::Path) -> String {
    let rendered = path.display().to_string();
    let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
        return rendered;
    };
    match rendered.strip_prefix(&std::path::Path::new(&home).display().to_string()) {
        Some(rest) => format!("~{rest}"),
        None => rendered,
    }
}
