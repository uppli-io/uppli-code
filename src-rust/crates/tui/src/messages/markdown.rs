//! Markdown -> ratatui lines renderer used by transcript message families.

use crate::figures;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

/// Render markdown text to styled ratatui lines.
pub fn render_markdown(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    for raw in text.lines() {
        if raw.trim_start().starts_with("```") {
            if in_code_block {
                lines.push(Line::from(vec![Span::styled(
                    "  └──────────────────────────────────────────────────".to_string(),
                    Style::default().fg(Color::Yellow),
                )]));
                in_code_block = false;
                code_lang.clear();
            } else {
                in_code_block = true;
                code_lang = raw.trim_start().trim_start_matches('`').trim().to_string();
                let lang_label = if code_lang.is_empty() {
                    String::new()
                } else {
                    format!(" {} ", code_lang)
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("  ┌──────────────────────{}", lang_label),
                    Style::default().fg(Color::Yellow),
                )]));
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(Color::Yellow)),
                Span::styled(raw.to_string(), Style::default().fg(Color::White)),
            ]));
            continue;
        }

        if let Some(quoted) = raw.strip_prefix("> ") {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", figures::BLOCKQUOTE_BAR),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(quoted.to_string(), Style::default().fg(Color::DarkGray)),
            ]));
            continue;
        }

        if raw.starts_with("### ") {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", &raw[4..]),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )]));
            continue;
        }
        if raw.starts_with("## ") {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", &raw[3..]),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )]));
            continue;
        }
        if raw.starts_with("# ") {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", &raw[2..]),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED),
            )]));
            continue;
        }

        let padded = format!("  {}", raw);
        let effective_width = width.saturating_sub(4) as usize;
        for wrapped_line in word_wrap(&padded, effective_width) {
            let spans = parse_inline_spans(wrapped_line);
            lines.push(Line::from(spans));
        }
    }

    if in_code_block {
        lines.push(Line::from(vec![Span::styled(
            "  └──────────────────────────────────────────────────".to_string(),
            Style::default().fg(Color::Yellow),
        )]));
    }

    lines
}

fn parse_inline_spans(text: String) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = text.as_str();

    while !remaining.is_empty() {
        let bold_pos = remaining.find("**");
        let code_pos = remaining.find('`');

        match (bold_pos, code_pos) {
            (None, None) => {
                spans.push(Span::raw(remaining.to_string()));
                break;
            }
            (Some(b), Some(c)) if c < b => {
                if c > 0 {
                    spans.push(Span::raw(remaining[..c].to_string()));
                }
                let after_tick = &remaining[c + 1..];
                if let Some(end) = after_tick.find('`') {
                    spans.push(Span::styled(
                        after_tick[..end].to_string(),
                        Style::default().fg(Color::Yellow),
                    ));
                    remaining = &after_tick[end + 1..];
                } else {
                    spans.push(Span::raw(remaining[c..].to_string()));
                    break;
                }
            }
            (Some(b), _) => {
                if b > 0 {
                    spans.push(Span::raw(remaining[..b].to_string()));
                }
                let after_stars = &remaining[b + 2..];
                if let Some(end) = after_stars.find("**") {
                    spans.push(Span::styled(
                        after_stars[..end].to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                    remaining = &after_stars[end + 2..];
                } else {
                    spans.push(Span::raw(remaining[b..].to_string()));
                    break;
                }
            }
            (None, Some(c)) => {
                if c > 0 {
                    spans.push(Span::raw(remaining[..c].to_string()));
                }
                let after_tick = &remaining[c + 1..];
                if let Some(end) = after_tick.find('`') {
                    spans.push(Span::styled(
                        after_tick[..end].to_string(),
                        Style::default().fg(Color::Yellow),
                    ));
                    remaining = &after_tick[end + 1..];
                } else {
                    spans.push(Span::raw(remaining[c..].to_string()));
                    break;
                }
            }
        }
    }

    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 || UnicodeWidthStr::width(text) <= width {
        return vec![text.to_string()];
    }

    let mut result = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0usize;

    for word in text.split_whitespace() {
        let word_w = UnicodeWidthStr::width(word);
        if current_width == 0 {
            current_line.push_str(word);
            current_width = word_w;
        } else if current_width + 1 + word_w <= width {
            current_line.push(' ');
            current_line.push_str(word);
            current_width += 1 + word_w;
        } else {
            result.push(std::mem::take(&mut current_line));
            current_line.push_str(word);
            current_width = word_w;
        }
    }

    if !current_line.is_empty() {
        result.push(current_line);
    }
    if result.is_empty() {
        result.push(text.to_string());
    }
    result
}
