//! The same rectangular form in both terminal containers (docs/tui.md).

use super::*;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{List, ListItem, ListState, Paragraph, StatefulWidget, Widget},
};

#[derive(Clone, Copy)]
pub(in crate::tui) struct Styles {
    pub text: Style,
    pub title: Style,
    pub hint: Style,
    pub selected: Style,
}

impl Form {
    pub(in crate::tui) fn render(&self, buf: &mut Buffer, area: Rect, styles: Styles) {
        buf.set_style(area, styles.text);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        let (title, subtitle, choices, selected) = self.content();
        Paragraph::new(title)
            .style(styles.title.add_modifier(Modifier::BOLD))
            .render(rows[0], buf);
        Paragraph::new(subtitle)
            .style(styles.hint)
            .render(rows[1], buf);
        let mut state = ListState::default().with_selected(Some(selected));
        let items: Vec<_> = choices
            .into_iter()
            .map(|s| ListItem::new(Line::from(s)))
            .collect();
        StatefulWidget::render(
            List::new(items)
                .highlight_symbol("▸ ")
                .highlight_style(styles.selected),
            rows[2],
            buf,
            &mut state,
        );
        Paragraph::new(
            self.note
                .as_deref()
                .unwrap_or("Prototype · sample data only · no session will be created"),
        )
        .style(styles.hint)
        .render(rows[3], buf);
        let hint = match self.page {
            Page::Form => "↑↓ move  Enter select  r reset  Esc cancel",
            Page::Models { .. } => "Type to filter  ↑↓ move  Enter select  Esc back",
            Page::Text { .. } | Page::Temperature { .. } => "Type a value  Enter accept  Esc back",
            _ => "↑↓ move  Enter select  Esc back",
        };
        Paragraph::new(hint).style(styles.hint).render(rows[4], buf);
    }

    fn content(&self) -> (String, String, Vec<String>, usize) {
        match &self.page {
            Page::Form => {
                let model = |role: Role, changed| {
                    format!(
                        "{:<16} {}  [{}]",
                        role.label(),
                        self.draft.selection(role),
                        if changed { "override" } else { "default" }
                    )
                };
                (
                    "New session".into(),
                    "Sample project · ~/code/example".into(),
                    vec![
                        "Start session".into(),
                        model(Role::Main, self.draft.main.is_some()),
                        format!(
                            "  Effort         {}",
                            self.draft
                                .effort
                                .as_deref()
                                .unwrap_or("high  [project default]")
                        ),
                        format!(
                            "  Temperature    {}",
                            self.draft
                                .temperature
                                .map(|t| format!("{t}  [override]"))
                                .unwrap_or("provider default".into())
                        ),
                        model(Role::Spotter, self.draft.spotter.is_some()),
                        model(Role::Triage, self.draft.triage.is_some()),
                    ],
                    self.selected,
                )
            }
            Page::Connections { role, selected } => {
                let mut choices = vec![if *role == Role::Triage {
                    "Use default — share main client".into()
                } else {
                    "Use default".into()
                }];
                choices.extend(
                    CONNECTIONS
                        .iter()
                        .map(|(name, provider, _)| format!("{name}  · {provider}")),
                );
                (
                    format!("{} › connection", role.label()),
                    "Choose a configured connection (sample accounts)".into(),
                    choices,
                    *selected,
                )
            }
            Page::Models {
                role,
                connection,
                query,
                selected,
            } => {
                let mut choices: Vec<String> = matching_models(*connection, query)
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                choices.push("Enter a model ID…".into());
                (
                    format!("{} › {} › model", role.label(), CONNECTIONS[*connection].0),
                    format!("Filter: {query}▏"),
                    choices,
                    *selected,
                )
            }
            Page::Text {
                role,
                connection,
                value,
            } => (
                format!(
                    "{} › {} › model ID",
                    role.label(),
                    CONNECTIONS[*connection].0
                ),
                "Provider-native ID · validated only when real creation is wired".into(),
                vec![format!("{value}▏")],
                0,
            ),
            Page::Effort { selected } => (
                "Main model › effort".into(),
                "Sample choices, not provider capability validation".into(),
                EFFORTS.iter().map(|s| s.to_string()).collect(),
                *selected,
            ),
            Page::Temperature { value } => (
                "Main model › temperature".into(),
                "0–100 · empty uses provider default".into(),
                vec![format!("{value}▏")],
                0,
            ),
        }
    }
}
