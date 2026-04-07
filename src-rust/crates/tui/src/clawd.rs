//! Clawd robot mascot rendering for ratatui.
//!
//! The Clawd character is a 3-row Unicode block-art robot matching the
//! TypeScript source.  Call `clawd_lines()` to get 4 `Line` values (3 body
//! rows + 1 blank spacing row) ready for embedding in a Paragraph.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The pose / expression of the Clawd mascot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClawdPose {
    Default,
    ArmsUp,
    LookLeft,
    LookRight,
}

/// Body-part style: bold pink foreground (#e91e63).
fn body_style() -> Style {
    Style::default()
        .fg(Color::Rgb(233, 30, 99))
        .add_modifier(Modifier::BOLD)
}

/// Eye-row style: pink text on black background.
fn eye_bg_style() -> Style {
    Style::default()
        .fg(Color::Rgb(233, 30, 99))
        .bg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Returns 4 Lines representing the Clawd mascot:
///   [0] — head row (optional arms + eyes)
///   [1] — body row (arms + torso)
///   [2] — feet row
///   [3] — blank spacing line
pub fn clawd_lines(pose: &ClawdPose) -> [Line<'static>; 4] {
    // Each pose defines:
    //   r1L, r1E (eye section), r1R — head row segments
    //   r2L, r2R                   — body row arm segments
    // The body centre (5 full-block chars) and the feet row are always the same.

    let (r1l, r1e, r1r, r2l, r2r) = match pose {
        ClawdPose::Default => (
            " ▐",    // head row left
            "▛███▜", // head / eye section
            "▌",     // head row right
            "▝▜",    // body row left arm
            "▛▘",    // body row right arm
        ),
        ClawdPose::ArmsUp => ("▗▟", "▛███▜", "▙▖", " ▜", "▛ "),
        ClawdPose::LookLeft => (" ▐", "▟███▟", "▌", "▝▜", "▛▘"),
        ClawdPose::LookRight => (" ▐", "▙███▙", "▌", "▝▜", "▛▘"),
    };

    // Row 1: head
    let row1 = Line::from(vec![
        Span::styled(r1l.to_string(), body_style()),
        Span::styled(r1e.to_string(), eye_bg_style()),
        Span::styled(r1r.to_string(), body_style()),
    ]);

    // Row 2: body — arms + 5 full-block chars as torso
    let row2 = Line::from(vec![
        Span::styled(r2l.to_string(), body_style()),
        Span::styled("█████".to_string(), body_style()),
        Span::styled(r2r.to_string(), body_style()),
    ]);

    // Row 3: feet (fixed)
    let row3 = Line::from(vec![Span::styled("  ▘▘ ▝▝  ".to_string(), body_style())]);

    // Row 4: blank spacing
    let row4 = Line::from("");

    [row1, row2, row3, row4]
}
