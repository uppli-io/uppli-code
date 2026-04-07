// notifications.rs — Notification / banner system for the TUI.

use std::collections::VecDeque;
use std::time::Instant;

/// Severity / visual style of a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationKind {
    Info,
    Warning,
    Error,
    Success,
}

/// A single notification entry.
#[derive(Debug, Clone)]
pub struct Notification {
    /// Unique identifier (used for dismissal).
    pub id: String,
    pub kind: NotificationKind,
    pub message: String,
    /// When `Some`, the notification auto-expires at this instant.
    pub expires_at: Option<Instant>,
    /// Whether the user can manually dismiss this notification.
    pub dismissible: bool,
}

/// A FIFO queue of active notifications.
#[derive(Debug, Default)]
pub struct NotificationQueue {
    pub notifications: VecDeque<Notification>,
    next_id: u64,
}

impl NotificationQueue {
    pub fn new() -> Self {
        Self {
            notifications: VecDeque::new(),
            next_id: 0,
        }
    }

    /// Push a new notification.
    ///
    /// * `duration_secs` — `None` for persistent, `Some(n)` for auto-expire after *n* seconds.
    pub fn push(&mut self, kind: NotificationKind, msg: String, duration_secs: Option<u64>) {
        let expires_at =
            duration_secs.map(|secs| Instant::now() + std::time::Duration::from_secs(secs));
        let id = format!("notif-{}", self.next_id);
        self.next_id += 1;
        self.notifications.push_back(Notification {
            id,
            kind,
            message: msg,
            expires_at,
            dismissible: true,
        });
    }

    /// Dismiss the notification with the given `id`.
    pub fn dismiss(&mut self, id: &str) {
        self.notifications.retain(|n| n.id != id);
    }

    /// Remove all expired notifications.  Call this once per render frame.
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.notifications
            .retain(|n| n.expires_at.is_none_or(|exp| exp > now));
    }

    /// Return the topmost (oldest) active notification, if any.
    pub fn current(&self) -> Option<&Notification> {
        self.notifications.front()
    }

    /// Dismiss the currently visible (topmost) notification.
    pub fn dismiss_current(&mut self) {
        if let Some(n) = self.notifications.front().cloned() {
            if n.dismissible {
                self.notifications.pop_front();
            }
        }
    }

    /// Return `true` if there are no active notifications.
    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

impl NotificationKind {
    pub fn color(&self) -> Color {
        match self {
            NotificationKind::Info => Color::Cyan,
            NotificationKind::Warning => Color::Yellow,
            NotificationKind::Error => Color::Red,
            NotificationKind::Success => Color::Green,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            NotificationKind::Info => "ℹ",
            NotificationKind::Warning => "⚠",
            NotificationKind::Error => "✗",
            NotificationKind::Success => "✓",
        }
    }
}

/// Render the topmost notification as a floating banner at the top of `area`.
pub fn render_notification_banner(frame: &mut Frame, queue: &NotificationQueue, area: Rect) {
    let notif = match queue.current() {
        Some(n) => n,
        None => return,
    };

    // One-line banner across the top of the provided area, inset slightly
    let banner_width = area.width.saturating_sub(4);
    let banner_area = Rect {
        x: area.x + 2,
        y: area.y,
        width: banner_width,
        height: 3,
    };

    // Only draw if there's room
    if area.height < 4 || banner_width < 10 {
        return;
    }

    let color = notif.kind.color();
    let icon = notif.kind.icon();

    let dismiss_hint = if notif.dismissible {
        " [Esc to dismiss]"
    } else {
        ""
    };
    let msg_text = format!(" {} {} {}", icon, notif.message, dismiss_hint);
    // Truncate if needed
    let max_inner = banner_width.saturating_sub(2) as usize;
    let msg_display = if msg_text.len() > max_inner {
        format!("{}…", &msg_text[..max_inner.saturating_sub(1)])
    } else {
        msg_text
    };

    let lines = vec![Line::from(vec![Span::styled(
        msg_display,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )])];

    frame.render_widget(Clear, banner_area);
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color)),
    );
    frame.render_widget(para, banner_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_current() {
        let mut q = NotificationQueue::new();
        assert!(q.current().is_none());
        q.push(NotificationKind::Info, "hello".to_string(), None);
        assert_eq!(q.current().unwrap().message, "hello");
    }

    #[test]
    fn dismiss_by_id() {
        let mut q = NotificationQueue::new();
        q.push(NotificationKind::Warning, "warn".to_string(), None);
        let id = q.current().unwrap().id.clone();
        q.dismiss(&id);
        assert!(q.is_empty());
    }

    #[test]
    fn tick_removes_expired() {
        let mut q = NotificationQueue::new();
        // Push a notification that expired in the past
        q.notifications.push_back(super::Notification {
            id: "x".to_string(),
            kind: NotificationKind::Info,
            message: "gone".to_string(),
            expires_at: Some(Instant::now() - std::time::Duration::from_secs(1)),
            dismissible: true,
        });
        assert!(!q.is_empty());
        q.tick();
        assert!(q.is_empty());
    }

    #[test]
    fn persistent_notification_survives_tick() {
        let mut q = NotificationQueue::new();
        q.push(NotificationKind::Success, "persistent".to_string(), None);
        q.tick();
        assert!(!q.is_empty());
    }
}
