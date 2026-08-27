use ratatui::style::{Color, Modifier, Style};

use crate::tui::document::SpanStyle;

use super::state::InteractiveState;

/// How far a code color is pulled toward the dimmed register, 0.0 (untouched) to
/// 1.0 (fully dim). Thinking must not read as the assistant's answer, but the
/// visible reason to render it as markdown at all is that its code blocks become
/// legible — so highlighted code keeps its hue separation and gives up
/// brightness, rather than collapsing to one flat muted color like the prose
/// around it (docs/tui.md).
const CODE_DIM_BLEND: f32 = 0.55;

/// Whether a style carries syntax information worth keeping distinguishable in
/// the dimmed register. These are the categories a highlighter assigns; the
/// prose and structural styles around them dim the whole way.
fn is_code_style(style: SpanStyle) -> bool {
    matches!(
        style,
        SpanStyle::Keyword
            | SpanStyle::TypeName
            | SpanStyle::FunctionName
            | SpanStyle::FieldName
            | SpanStyle::Lifetime
            | SpanStyle::Generic
            | SpanStyle::Operator
            | SpanStyle::InlineCode
            | SpanStyle::InlineRustCode
    )
}

/// Linear blend of two channels, `t` of the way from `from` to `to`.
fn blend(from: u8, to: u8, t: f32) -> u8 {
    (from as f32 + (to as f32 - from as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

impl<'a> InteractiveState<'a> {
    /// Convert SpanStyle to ratatui Style
    pub(super) fn style(&self, span_style: SpanStyle) -> Style {
        let style = self.undimmed_style(span_style);
        if self.dim_register {
            self.dim(style, span_style)
        } else {
            style
        }
    }

    fn undimmed_style(&self, span_style: SpanStyle) -> Style {
        match span_style {
            SpanStyle::Plain => {
                let fg = self.render_context.color_scheme().default_foreground();
                Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b))
            }
            SpanStyle::Punctuation => Style::default(),
            SpanStyle::Strong => Style::default().add_modifier(Modifier::BOLD),
            SpanStyle::Emphasis => Style::default().add_modifier(Modifier::ITALIC),
            SpanStyle::Strikethrough => Style::default().add_modifier(Modifier::CROSSED_OUT),
            SpanStyle::InlineCode | SpanStyle::InlineRustCode => {
                let color = self.render_context.color_scheme().color_for(span_style);
                Style::default().fg(Color::Rgb(color.r, color.g, color.b))
            }
            _ => {
                let color = self.render_context.color_scheme().color_for(span_style);
                Style::default().fg(Color::Rgb(color.r, color.g, color.b))
            }
        }
    }

    /// Move one resolved style into the dimmed register. Prose lands flat on the
    /// register's own color; code is blended toward it by [`CODE_DIM_BLEND`].
    ///
    /// The modifier-only styles (`Strong`, `Emphasis`, `Strikethrough`) resolve
    /// to no foreground at all, so dimming them means *giving* them one — left
    /// alone they would render at full-strength default and the emphasis inside
    /// a thinking block would be brighter than the sentence holding it.
    fn dim(&self, style: Style, span_style: SpanStyle) -> Style {
        let scheme = self.render_context.color_scheme();
        let dim = scheme.color_for(SpanStyle::Comment);
        let target = if is_code_style(span_style) {
            match style.fg {
                Some(Color::Rgb(r, g, b)) => Color::Rgb(
                    blend(r, dim.r, CODE_DIM_BLEND),
                    blend(g, dim.g, CODE_DIM_BLEND),
                    blend(b, dim.b, CODE_DIM_BLEND),
                ),
                _ => Color::Rgb(dim.r, dim.g, dim.b),
            }
        } else {
            Color::Rgb(dim.r, dim.g, dim.b)
        };
        style.fg(target)
    }
}

#[cfg(test)]
mod test;
