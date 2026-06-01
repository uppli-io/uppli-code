//! Analytics and telemetry (OpenTelemetry-compatible counters)
//!
//! PR P (2026-05-29): USD/cost tracking removed. Provider pricing was the
//! perpetual maintenance burden (silently underrated 4× under DeepSeek's
//! promo). The provider dashboard is the source of truth for actual spend.
//! Tokens (objective, provider-reported) are tracked instead.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Session-level metrics counters (mirrors TypeScript bootstrap state).
///
/// All counters use `AtomicU64` so they can be shared across threads without
/// a mutex.
#[derive(Debug, Default)]
pub struct SessionMetrics {
    pub total_input_tokens: AtomicU64,
    pub total_output_tokens: AtomicU64,
    pub total_api_duration_ms: AtomicU64,
    pub total_tool_duration_ms: AtomicU64,
    pub total_lines_added: AtomicU64,
    pub total_lines_removed: AtomicU64,
    pub session_count: AtomicU64,
    pub commit_count: AtomicU64,
    pub pr_count: AtomicU64,
    pub tool_use_count: AtomicU64,
}

impl SessionMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn add_tokens(&self, input: u32, output: u32) {
        self.total_input_tokens
            .fetch_add(input as u64, Ordering::Relaxed);
        self.total_output_tokens
            .fetch_add(output as u64, Ordering::Relaxed);
    }

    pub fn add_api_duration(&self, ms: u64) {
        self.total_api_duration_ms.fetch_add(ms, Ordering::Relaxed);
    }

    pub fn add_tool_duration(&self, ms: u64) {
        self.total_tool_duration_ms.fetch_add(ms, Ordering::Relaxed);
    }

    pub fn add_lines(&self, added: i64, removed: i64) {
        if added > 0 {
            self.total_lines_added
                .fetch_add(added as u64, Ordering::Relaxed);
        }
        if removed > 0 {
            self.total_lines_removed
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
    }

    pub fn increment_commits(&self) {
        self.commit_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_prs(&self) {
        self.pr_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_tool_use(&self) {
        self.tool_use_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn summary(&self) -> MetricsSummary {
        MetricsSummary {
            input_tokens: self.total_input_tokens.load(Ordering::Relaxed),
            output_tokens: self.total_output_tokens.load(Ordering::Relaxed),
            api_duration_ms: self.total_api_duration_ms.load(Ordering::Relaxed),
            tool_duration_ms: self.total_tool_duration_ms.load(Ordering::Relaxed),
            lines_added: self.total_lines_added.load(Ordering::Relaxed),
            lines_removed: self.total_lines_removed.load(Ordering::Relaxed),
            commits: self.commit_count.load(Ordering::Relaxed),
            prs: self.pr_count.load(Ordering::Relaxed),
            tool_uses: self.tool_use_count.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time snapshot of session metrics.
#[derive(Debug, Clone)]
pub struct MetricsSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub api_duration_ms: u64,
    pub tool_duration_ms: u64,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub commits: u64,
    pub prs: u64,
    pub tool_uses: u64,
}

impl MetricsSummary {
    /// Format total token count with K/M suffix.
    pub fn format_tokens(&self) -> String {
        let total = self.input_tokens + self.output_tokens;
        if total >= 1_000_000 {
            format!("{:.1}M tok", total as f64 / 1_000_000.0)
        } else if total >= 1_000 {
            format!("{:.1}K tok", total as f64 / 1_000.0)
        } else {
            format!("{} tok", total)
        }
    }
}

/// Event types for first-party analytics (privacy-respecting — no PII).
#[derive(Debug, Clone)]
pub enum AnalyticsEvent {
    SessionStarted {
        model: String,
        is_interactive: bool,
    },
    SessionEnded {
        turn_count: u32,
        total_tokens: u64,
        duration_ms: u64,
        had_errors: bool,
    },
    ToolUsed {
        tool_name: String,
        success: bool,
        duration_ms: u64,
    },
    CommandExecuted {
        command: String,
        success: bool,
    },
    CompactionTriggered {
        tokens_before: u32,
        tokens_after: u32,
    },
}

/// Analytics sink — currently logs via `tracing`; can be extended to push
/// events to a first-party endpoint.
pub struct Analytics {
    enabled: bool,
    session_id: String,
}

impl Analytics {
    pub fn new(session_id: String, enabled: bool) -> Self {
        Self {
            enabled,
            session_id,
        }
    }

    pub fn track(&self, event: AnalyticsEvent) {
        if !self.enabled {
            return;
        }
        tracing::debug!(
            session_id = %self.session_id,
            event = ?event,
            "analytics event"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_metrics_initial_zero() {
        let m = SessionMetrics::new();
        assert_eq!(m.total_input_tokens.load(Ordering::Relaxed), 0);
        assert_eq!(m.total_output_tokens.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_add_tokens() {
        let m = SessionMetrics::new();
        m.add_tokens(1000, 500);
        assert_eq!(m.total_input_tokens.load(Ordering::Relaxed), 1000);
        assert_eq!(m.total_output_tokens.load(Ordering::Relaxed), 500);
    }

    #[test]
    fn test_add_tokens_accumulates() {
        let m = SessionMetrics::new();
        m.add_tokens(1000, 500);
        m.add_tokens(200, 100);
        assert_eq!(m.total_input_tokens.load(Ordering::Relaxed), 1200);
        assert_eq!(m.total_output_tokens.load(Ordering::Relaxed), 600);
    }

    #[test]
    fn test_add_lines_positive() {
        let m = SessionMetrics::new();
        m.add_lines(10, 5);
        assert_eq!(m.total_lines_added.load(Ordering::Relaxed), 10);
        assert_eq!(m.total_lines_removed.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_add_lines_negative_ignored() {
        let m = SessionMetrics::new();
        m.add_lines(-3, -7);
        assert_eq!(m.total_lines_added.load(Ordering::Relaxed), 0);
        assert_eq!(m.total_lines_removed.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_increment_commits_and_prs() {
        let m = SessionMetrics::new();
        m.increment_commits();
        m.increment_commits();
        m.increment_prs();
        assert_eq!(m.commit_count.load(Ordering::Relaxed), 2);
        assert_eq!(m.pr_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_increment_tool_use() {
        let m = SessionMetrics::new();
        for _ in 0..5 {
            m.increment_tool_use();
        }
        assert_eq!(m.tool_use_count.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn test_summary_snapshot() {
        let m = SessionMetrics::new();
        m.add_tokens(100, 50);
        m.add_api_duration(300);
        m.add_tool_duration(150);
        m.add_lines(8, 3);
        m.increment_commits();
        m.increment_prs();
        m.increment_tool_use();

        let s = m.summary();
        assert_eq!(s.input_tokens, 100);
        assert_eq!(s.output_tokens, 50);
        assert_eq!(s.api_duration_ms, 300);
        assert_eq!(s.tool_duration_ms, 150);
        assert_eq!(s.lines_added, 8);
        assert_eq!(s.lines_removed, 3);
        assert_eq!(s.commits, 1);
        assert_eq!(s.prs, 1);
        assert_eq!(s.tool_uses, 1);
    }

    #[test]
    fn test_format_tokens_exact() {
        let s = MetricsSummary {
            input_tokens: 500,
            output_tokens: 300,
            api_duration_ms: 0,
            tool_duration_ms: 0,
            lines_added: 0,
            lines_removed: 0,
            commits: 0,
            prs: 0,
            tool_uses: 0,
        };
        assert_eq!(s.format_tokens(), "800 tok");
    }

    #[test]
    fn test_format_tokens_kilo() {
        let s = MetricsSummary {
            input_tokens: 5_000,
            output_tokens: 3_000,
            api_duration_ms: 0,
            tool_duration_ms: 0,
            lines_added: 0,
            lines_removed: 0,
            commits: 0,
            prs: 0,
            tool_uses: 0,
        };
        assert!(s.format_tokens().ends_with("K tok"));
    }

    #[test]
    fn test_format_tokens_mega() {
        let s = MetricsSummary {
            input_tokens: 1_500_000,
            output_tokens: 500_000,
            api_duration_ms: 0,
            tool_duration_ms: 0,
            lines_added: 0,
            lines_removed: 0,
            commits: 0,
            prs: 0,
            tool_uses: 0,
        };
        assert!(s.format_tokens().ends_with("M tok"));
    }

    #[test]
    fn test_analytics_track_disabled_no_panic() {
        let a = Analytics::new("test-session".to_string(), false);
        // Should not panic even though disabled
        a.track(AnalyticsEvent::SessionStarted {
            model: "claude-opus-4-6".to_string(),
            is_interactive: true,
        });
    }

    #[test]
    fn test_analytics_track_enabled_no_panic() {
        let a = Analytics::new("test-session-2".to_string(), true);
        a.track(AnalyticsEvent::ToolUsed {
            tool_name: "Bash".to_string(),
            success: true,
            duration_ms: 42,
        });
    }
}
