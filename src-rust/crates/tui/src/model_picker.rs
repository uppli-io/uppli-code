//! Model picker overlay (/model command).
//!
//! Fully provider-agnostic: the model list, effort support, fast-mode model,
//! and descriptions are all derived from `ProviderCapabilities::known_models`
//! at runtime.  No hardcoded model IDs, no substring matching.

use cc_api::provider::{ModelMetadata, ProviderCapabilities};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::overlays::centered_rect;

// ---------------------------------------------------------------------------
// Effort level
// ---------------------------------------------------------------------------

/// Mirrors the TS `EffortLevel` enum and `effortLevelToSymbol()` helper.
///
/// Effort controls the extended-thinking `budget_tokens` parameter sent to the
/// API. Only models that support extended thinking honour this; for others it
/// is silently ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum EffortLevel {
    Low,
    #[default]
    Normal,
    High,
    Max,
}

impl EffortLevel {
    /// Unicode quarter-circle symbol used in the TS UI.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Low => "\u{25cb}",    // ○  empty circle
            Self::Normal => "\u{25d0}", // ◐  half
            Self::High => "\u{25d5}",   // ◕  three-quarter
            Self::Max => "\u{25cf}",    // ●  full
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// Returns the budget_tokens value to pass to the API, or `None` for the
    /// default (no extended thinking).
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Low => Some(1_024),
            Self::Normal => None,
            Self::High => Some(16_000),
            Self::Max => Some(32_000),
        }
    }

    /// Cycle to next level; skips `Max` when the selected model does not
    /// support it.
    pub fn next(self, supports_max: bool) -> Self {
        match self {
            Self::Low => Self::Normal,
            Self::Normal => Self::High,
            Self::High => {
                if supports_max {
                    Self::Max
                } else {
                    Self::Low
                }
            }
            Self::Max => Self::Low,
        }
    }

    /// Cycle to previous level.
    pub fn prev(self, supports_max: bool) -> Self {
        match self {
            Self::Low => {
                if supports_max {
                    Self::Max
                } else {
                    Self::High
                }
            }
            Self::Normal => Self::Low,
            Self::High => Self::Normal,
            Self::Max => Self::High,
        }
    }
}

// ---------------------------------------------------------------------------
// Model capability helpers — all provider-agnostic
// ---------------------------------------------------------------------------

// No more hardcoded model ID checks.  Effort support, max-effort, fast-mode
// model, and descriptions are all read from the provider's `ModelMetadata`
// at runtime via the `known_models` field on `ModelPickerState`.

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single model entry shown in the picker.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub description: String,
    /// Whether this is the currently active model.
    pub is_current: bool,
}

/// State for the /model picker overlay.
pub struct ModelPickerState {
    pub visible: bool,
    pub selected_idx: usize,
    pub models: Vec<ModelEntry>,
    /// Live filter typed by the user.
    pub filter: String,
    /// Current effort level for models that support extended thinking.
    pub effort_level: EffortLevel,
    /// Whether fast mode is currently active (locks model to `fast_model_id`).
    pub fast_mode: bool,
    /// `true` once the dynamic model list has been loaded from the API.
    pub models_loaded: bool,
    /// `true` while the background fetch is in flight.
    pub loading_models: bool,

    // ── Provider metadata (provider-agnostic) ─────────────────
    /// The model ID that fast-mode locks to (from `ProviderCapabilities::fast_model`).
    /// `None` means the provider has no fast-mode concept.
    pub fast_model_id: Option<String>,
    /// Per-model metadata from the provider — used to derive effort support,
    /// descriptions, and max-effort eligibility without hardcoded model IDs.
    known_metadata: Vec<ModelMetadata>,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl ModelPickerState {
    /// Create a new picker with no models (call `init_from_capabilities` to populate).
    ///
    /// This is used at App construction time before a provider is available.
    /// The picker will show an empty list until `init_from_capabilities()` or
    /// `set_models()` is called.
    pub fn new() -> Self {
        Self {
            visible: false,
            selected_idx: 0,
            models: Vec::new(),
            filter: String::new(),
            effort_level: EffortLevel::Normal,
            fast_mode: false,
            models_loaded: false,
            loading_models: false,
            fast_model_id: None,
            known_metadata: Vec::new(),
        }
    }

    /// Initialize the picker from a provider's self-description.
    ///
    /// Populates the model list, fast-mode model, and per-model metadata
    /// so that effort support and descriptions are fully provider-agnostic.
    pub fn init_from_capabilities(&mut self, caps: &ProviderCapabilities) {
        self.fast_model_id = caps.fast_model.clone();
        self.known_metadata = caps.known_models.clone();
        self.models = Self::models_from_metadata(&caps.known_models);
        // Mark as loaded so the picker doesn't show "loading…" on first open.
        if !self.models.is_empty() {
            self.models_loaded = true;
        }
    }

    /// Open the overlay.
    ///
    /// `current_model` is highlighted as active; `current_effort` and
    /// `fast_mode` are carried over from app state so the user sees the live
    /// values.
    pub fn open(&mut self, current_model: &str) {
        self.open_with_state(current_model, EffortLevel::Normal, false);
    }

    /// Open the overlay with full state context.
    pub fn open_with_state(&mut self, current_model: &str, effort: EffortLevel, fast_mode: bool) {
        for m in &mut self.models {
            m.is_current = m.id == current_model;
        }
        self.selected_idx = self.models.iter().position(|m| m.is_current).unwrap_or(0);
        self.filter.clear();
        self.effort_level = effort;
        self.fast_mode = fast_mode;
        self.visible = true;
    }

    /// Close the overlay without selecting.
    pub fn close(&mut self) {
        self.visible = false;
        self.filter.clear();
    }

    /// Move selection up one row (wraps to last if at top).
    pub fn select_prev(&mut self) {
        let count = self.filtered_models().len();
        if count == 0 {
            return;
        }
        if self.selected_idx == 0 {
            self.selected_idx = count - 1;
        } else {
            self.selected_idx -= 1;
        }
    }

    /// Move selection down one row (wraps to first if at bottom).
    pub fn select_next(&mut self) {
        let count = self.filtered_models().len();
        if count == 0 {
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % count;
    }

    /// Cycle effort level forward (→ key).
    pub fn effort_next(&mut self) {
        let filtered = self.filtered_models();
        let id = filtered
            .get(self.selected_idx)
            .map(|m| m.id.as_str())
            .unwrap_or("");
        // All thinking-capable models support max effort in the provider-agnostic model.
        let supports_max = self.model_supports_thinking(id);
        self.effort_level = self.effort_level.next(supports_max);
    }

    /// Cycle effort level backward (← key).
    pub fn effort_prev(&mut self) {
        let filtered = self.filtered_models();
        let id = filtered
            .get(self.selected_idx)
            .map(|m| m.id.as_str())
            .unwrap_or("");
        let supports_max = self.model_supports_thinking(id);
        self.effort_level = self.effort_level.prev(supports_max);
    }

    /// Returns the effective effort for the currently highlighted model:
    /// `None` if the model does not support extended thinking.
    pub fn effective_effort(&self) -> Option<EffortLevel> {
        let filtered = self.filtered_models();
        let id = filtered
            .get(self.selected_idx)
            .map(|m| m.id.as_str())
            .unwrap_or("");
        if self.model_supports_thinking(id) {
            Some(self.effort_level)
        } else {
            None
        }
    }

    /// Confirm the current selection.
    ///
    /// Returns `(model_id, effort)` where `effort` is `None` for models that
    /// do not support extended thinking.  Closes the picker.
    ///
    /// Persists the selected model (and effort level, if applicable) to
    /// `~/.uppli/settings.json` under the keys `"model"` and `"effort"`.
    pub fn confirm(&mut self) -> Option<(String, Option<EffortLevel>)> {
        let filtered = self.filtered_models();
        let entry = filtered.get(self.selected_idx)?;
        let id = entry.id.clone();
        let effort = if self.model_supports_thinking(&id) {
            Some(self.effort_level)
        } else {
            None
        };
        // If user chose a model other than the fast-mode model while fast mode is
        // active, the caller should turn off fast mode (mirrors TS behaviour).
        self.close();

        // Persist selection to ~/.uppli/settings.json (best-effort).
        // Write to a temp file first, then atomically rename to avoid
        // corruption if another process reads mid-write.
        let settings_path = cc_core::config::Settings::global_settings_path();
        let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
        let mut json: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = json.as_object_mut() {
            obj.insert("model".to_string(), serde_json::Value::String(id.clone()));
            if let Some(e) = effort {
                obj.insert(
                    "effort".to_string(),
                    serde_json::Value::String(e.label().to_string()),
                );
            } else {
                obj.remove("effort");
            }
        }
        if let Ok(serialized) = serde_json::to_string_pretty(&json) {
            if let Some(parent) = settings_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Atomic write: temp file → rename.
            let tmp_path = settings_path.with_extension("json.tmp");
            if std::fs::write(&tmp_path, &serialized).is_ok() {
                let _ = std::fs::rename(&tmp_path, &settings_path);
            }
        }

        Some((id, effort))
    }

    /// Append a character to the filter string and reset the selection.
    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.selected_idx = 0;
    }

    /// Remove the last character from the filter string.
    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.selected_idx = 0;
    }

    /// Return models that match the current filter (case-insensitive).
    pub fn filtered_models(&self) -> Vec<&ModelEntry> {
        if self.filter.is_empty() {
            return self.models.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.models
            .iter()
            .filter(|m| {
                m.id.to_lowercase().contains(needle.as_str())
                    || m.display_name.to_lowercase().contains(needle.as_str())
                    || m.description.to_lowercase().contains(needle.as_str())
            })
            .collect()
    }

    /// Replace the model list with dynamically loaded entries.
    ///
    /// Called by the app event loop when the background fetch completes.
    /// Resets `loading_models` and sets `models_loaded`.
    pub fn set_models(&mut self, entries: Vec<ModelEntry>) {
        self.models = entries;
        self.loading_models = false;
        self.models_loaded = true;
        // Keep selected_idx in bounds.
        let count = self.filtered_models().len();
        if count > 0 && self.selected_idx >= count {
            self.selected_idx = count - 1;
        }
    }

    /// Fetch the list of available models from the provider API and convert
    /// them to `ModelEntry` values.
    ///
    /// On success, models are sorted newest-first (by `created_at` descending).
    /// On any error, returns the provider's `known_models` as a fallback so the
    /// picker is never left empty.
    pub async fn fetch_models(client: &dyn cc_api::LlmProvider) -> Vec<ModelEntry> {
        let caps = client.capabilities();
        let available = client.list_models().await;
        if available.is_empty() {
            return Self::models_from_metadata(&caps.known_models);
        }

        // Build a lookup map from known_models for enriching API results with
        // display names and descriptions from the provider preset.
        let meta_map: std::collections::HashMap<&str, &ModelMetadata> = caps
            .known_models
            .iter()
            .map(|m| (m.id.as_str(), m))
            .collect();

        let mut entries: Vec<(i64, ModelEntry)> = available
            .into_iter()
            .map(|m| {
                let meta = meta_map.get(m.id.as_str());
                let display = meta
                    .map(|md| md.display_name.clone())
                    .or_else(|| m.display_name.clone())
                    .unwrap_or_else(|| m.id.clone());
                let description = meta
                    .map(|md| md.description.clone())
                    .unwrap_or_else(|| format!("{} model", caps.display_name));
                let ts = m.created_at.unwrap_or(0);
                (
                    ts,
                    ModelEntry {
                        id: m.id,
                        display_name: display,
                        description,
                        is_current: false,
                    },
                )
            })
            .collect();

        // Sort newest-first.
        entries.sort_by_key(|e| std::cmp::Reverse(e.0));
        entries.into_iter().map(|(_, e)| e).collect()
    }

    // ── Private helpers ─────────────────────────────────────────

    /// Convert provider `ModelMetadata` entries to picker `ModelEntry` values.
    fn models_from_metadata(metadata: &[ModelMetadata]) -> Vec<ModelEntry> {
        metadata
            .iter()
            .map(|m| ModelEntry {
                id: m.id.clone(),
                display_name: m.display_name.clone(),
                description: m.description.clone(),
                is_current: false,
            })
            .collect()
    }

    /// Check whether a model ID supports thinking/reasoning via stored metadata.
    fn model_supports_thinking(&self, id: &str) -> bool {
        self.known_metadata
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.supports_thinking)
            .unwrap_or(false)
    }
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the model picker overlay directly into `buf`.
///
/// Draws a centred modal (≈70 wide × ≈22 tall) with:
/// - Fast-mode notice when fast mode is active
/// - A filter line when the user is typing
/// - A scrollable list of models with effort indicator for supporting models
/// - Selection highlight on the focused row
/// - Bottom hint bar with ←/→ keys for effort adjustment
pub fn render_model_picker(state: &ModelPickerState, area: Rect, buf: &mut Buffer) {
    if !state.visible {
        return;
    }

    const MODAL_W: u16 = 70;
    const MODAL_H: u16 = 22;

    let dialog_area = centered_rect(
        MODAL_W.min(area.width.saturating_sub(2)),
        MODAL_H.min(area.height.saturating_sub(2)),
        area,
    );

    // --- Clear background -------------------------------------------------
    for y in dialog_area.y..dialog_area.y + dialog_area.height {
        for x in dialog_area.x..dialog_area.x + dialog_area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }

    // --- Build line list --------------------------------------------------
    let mut lines: Vec<Line> = Vec::new();

    // Fast-mode notice
    if state.fast_mode {
        let fast_label = state.fast_model_id.as_deref().unwrap_or("fast model");
        lines.push(Line::from(vec![
            Span::styled("  \u{26a1} ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!(
                    "Fast mode is ON ({} only). Switching turns it off.",
                    fast_label
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
        lines.push(Line::from(""));
    }

    // Loading-models notice
    if state.loading_models {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                "\u{29d7} Loading models\u{2026}",
                Style::default()
                    .fg(Color::Rgb(100, 180, 255))
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
        lines.push(Line::from(""));
    }

    // Optional filter line
    if !state.filter.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  Filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                state.filter.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));
    }

    let filtered = state.filtered_models();

    if filtered.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No models match filter",
            Style::default().fg(Color::DarkGray),
        )]));
    } else {
        let inner_w = dialog_area.width.saturating_sub(2) as usize;

        for (i, model) in filtered.iter().enumerate() {
            let is_selected = i == state.selected_idx;
            let supports_effort = state.model_supports_thinking(&model.id);

            // Bullet: filled circle for the currently active model.
            let bullet = if model.is_current {
                "\u{25cf}"
            } else {
                "\u{25cb}"
            };
            let bullet_style = if model.is_current {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let row_style = if is_selected {
                Style::default()
                    .bg(Color::Rgb(40, 60, 80))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let name_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::Rgb(40, 60, 80))
            } else {
                Style::default().fg(Color::White)
            };
            let desc_style = if is_selected {
                Style::default()
                    .fg(Color::Rgb(180, 200, 220))
                    .bg(Color::Rgb(40, 60, 80))
            } else {
                Style::default().fg(Color::DarkGray)
            };

            // Effort indicator for supported models (shown right of name).
            let effort_span: Option<Span<'static>> = if supports_effort && is_selected {
                let sym = state.effort_level.symbol();
                let lbl = state.effort_level.label();
                Some(Span::styled(
                    format!("  {} {}", sym, lbl),
                    Style::default()
                        .fg(Color::Rgb(100, 200, 120))
                        .bg(Color::Rgb(40, 60, 80)),
                ))
            } else if supports_effort {
                // Subtle indicator when not selected
                Some(Span::styled(
                    format!("  {}", state.effort_level.symbol()),
                    Style::default().fg(Color::Rgb(60, 100, 60)),
                ))
            } else {
                None
            };

            // Description budget accounts for effort span width.
            let effort_w = effort_span.as_ref().map(|s| s.content.width()).unwrap_or(0);
            let name_w = model.display_name.width();
            let desc_budget = inner_w.saturating_sub(4 + name_w + effort_w + 2);
            let desc: String = if model.description.width() > desc_budget && desc_budget > 3 {
                let mut s = model.description.clone();
                while s.width() > desc_budget.saturating_sub(1) {
                    s.pop();
                }
                format!("{s}\u{2026}")
            } else {
                model.description.clone()
            };

            let mut spans = vec![
                Span::styled("  ", row_style),
                Span::styled(bullet, bullet_style.patch(row_style)),
                Span::styled(" ", row_style),
                Span::styled(model.display_name.clone(), name_style),
            ];
            if let Some(es) = effort_span {
                spans.push(es);
            }
            spans.push(Span::styled("  ", row_style));
            spans.push(Span::styled(desc, desc_style));

            lines.push(Line::from(spans));
        }
    }

    // Spacer + hint
    lines.push(Line::from(""));
    let hint_line = Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            "Enter",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("=select  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "\u{2190}\u{2192}",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("=effort  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "Esc",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("=cancel  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Type to filter", Style::default().fg(Color::DarkGray)),
    ]);
    lines.push(hint_line);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Model Picker ")
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Cyan));

    let para = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });

    use ratatui::widgets::Widget;
    para.render(dialog_area, buf);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cc_api::provider::{ApiFormat, AuthConfig, ModelMetadata, ProviderCapabilities};

    /// Build a test ProviderCapabilities with three models:
    /// - "test-reasoning" (supports thinking)
    /// - "test-chat" (no thinking — the fast model)
    /// - "test-mini" (no thinking)
    fn test_capabilities() -> ProviderCapabilities {
        ProviderCapabilities {
            name: "test".to_string(),
            display_name: "Test Provider".to_string(),
            attribution: "powered by Test".to_string(),
            default_model: "test-reasoning".to_string(),
            fast_model: Some("test-chat".to_string()),
            known_models: vec![
                ModelMetadata {
                    id: "test-reasoning".to_string(),
                    display_name: "Test Reasoning".to_string(),
                    description: "Deep reasoning model".to_string(),
                    context_window: 128_000,
                    max_output_tokens: 64_000,
                    supports_thinking: true,
                    pricing: None,
                },
                ModelMetadata {
                    id: "test-chat".to_string(),
                    display_name: "Test Chat".to_string(),
                    description: "Fast chat model".to_string(),
                    context_window: 128_000,
                    max_output_tokens: 8_192,
                    supports_thinking: false,
                    pricing: None,
                },
                ModelMetadata {
                    id: "test-mini".to_string(),
                    display_name: "Test Mini".to_string(),
                    description: "Lightweight model".to_string(),
                    context_window: 32_000,
                    max_output_tokens: 4_096,
                    supports_thinking: false,
                    pricing: None,
                },
            ],
            default_max_tokens: 64_000,
            default_thinking_budget: Some(32_000),
            api_format: ApiFormat::OpenAI,
            default_api_base: "http://localhost:8080".to_string(),
            auth: AuthConfig::default(),
        }
    }

    fn make_picker() -> ModelPickerState {
        let mut p = ModelPickerState::new();
        p.init_from_capabilities(&test_capabilities());
        p
    }

    fn make_picker_with_current(current: &str) -> ModelPickerState {
        let mut p = make_picker();
        p.open(current);
        p
    }

    // 1. init_from_capabilities populates the model list.
    #[test]
    fn init_from_capabilities_populates_models() {
        let p = make_picker();
        assert_eq!(p.models.len(), 3);
        let ids: Vec<&str> = p.models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"test-reasoning"));
        assert!(ids.contains(&"test-chat"));
        assert!(ids.contains(&"test-mini"));
    }

    // 2. open() marks exactly one model as current.
    #[test]
    fn open_marks_current_model() {
        let p = make_picker_with_current("test-chat");
        let current_count = p.models.iter().filter(|m| m.is_current).count();
        assert_eq!(current_count, 1);
        assert!(
            p.models
                .iter()
                .find(|m| m.id == "test-chat")
                .unwrap()
                .is_current
        );
    }

    // 3. open() with an unknown model ID marks none as current and sets idx=0.
    #[test]
    fn open_unknown_model_selects_first() {
        let p = make_picker_with_current("unknown-model");
        assert_eq!(p.selected_idx, 0);
        assert!(p.models.iter().all(|m| !m.is_current));
    }

    // 4. select_next() wraps around to 0 after the last entry.
    #[test]
    fn select_next_wraps() {
        let mut p = make_picker_with_current("test-reasoning");
        let total = p.filtered_models().len();
        p.selected_idx = total - 1;
        p.select_next();
        assert_eq!(p.selected_idx, 0);
    }

    // 5. select_prev() wraps around to last after idx 0.
    #[test]
    fn select_prev_wraps() {
        let mut p = make_picker_with_current("test-reasoning");
        p.selected_idx = 0;
        p.select_prev();
        let total = p.filtered_models().len();
        assert_eq!(p.selected_idx, total - 1);
    }

    // 6. filter reduces visible entries.
    #[test]
    fn filter_reduces_results() {
        let mut p = make_picker_with_current("test-reasoning");
        for c in "chat".chars() {
            p.push_filter_char(c);
        }
        let all = p.models.len();
        let filtered = p.filtered_models();
        assert!(
            filtered.len() < all,
            "filter should reduce the result count"
        );
        assert!(!filtered.is_empty(), "at least one chat model must match");
        for m in &filtered {
            let haystack = format!("{} {} {}", m.id, m.display_name, m.description).to_lowercase();
            assert!(
                haystack.contains("chat"),
                "model '{}' does not match filter",
                m.id
            );
        }
    }

    // 7. pop_filter_char removes last char.
    #[test]
    fn pop_filter_char_removes_last() {
        let mut p = make_picker_with_current("test-reasoning");
        p.push_filter_char('h');
        p.push_filter_char('a');
        p.push_filter_char('i');
        assert_eq!(p.filter, "hai");
        p.pop_filter_char();
        assert_eq!(p.filter, "ha");
    }

    // 8. confirm() returns selected model ID and closes the picker.
    #[test]
    fn confirm_returns_id_and_closes() {
        let mut p = make_picker_with_current("test-reasoning");
        p.selected_idx = 0;
        let first_id = p.filtered_models()[0].id.clone();
        let result = p.confirm();
        assert_eq!(result.map(|(id, _)| id), Some(first_id));
        assert!(!p.visible, "picker should be closed after confirm");
    }

    // 9. confirm() on empty filter list returns None.
    #[test]
    fn confirm_empty_filter_returns_none() {
        let mut p = make_picker_with_current("test-reasoning");
        p.filter = "zzznomatch999".to_string();
        p.selected_idx = 0;
        let result = p.confirm();
        assert!(result.is_none());
    }

    // 10. close() clears filter and hides overlay.
    #[test]
    fn close_clears_state() {
        let mut p = make_picker_with_current("test-reasoning");
        p.push_filter_char('x');
        p.close();
        assert!(!p.visible);
        assert!(p.filter.is_empty());
    }

    // 11. effort cycling works for thinking-capable models.
    #[test]
    fn effort_cycles_correctly() {
        let mut p = make_picker_with_current("test-reasoning");
        // test-reasoning supports thinking → effort cycles including max.
        assert_eq!(p.effort_level, EffortLevel::Normal);
        p.effort_next();
        assert_eq!(p.effort_level, EffortLevel::High);
        p.effort_next();
        assert_eq!(p.effort_level, EffortLevel::Max);
        p.effort_next();
        assert_eq!(p.effort_level, EffortLevel::Low);
    }

    // 12. thinking support is derived from provider metadata, not hardcoded IDs.
    #[test]
    fn thinking_support_from_metadata() {
        let p = make_picker();
        assert!(p.model_supports_thinking("test-reasoning"));
        assert!(!p.model_supports_thinking("test-chat"));
        assert!(!p.model_supports_thinking("test-mini"));
        // Unknown model → false
        assert!(!p.model_supports_thinking("nonexistent"));
    }

    // 13. Non-thinking models return None effort from confirm.
    #[test]
    fn non_thinking_model_has_no_effort() {
        let mut p = make_picker_with_current("test-chat");
        p.selected_idx = p.models.iter().position(|m| m.id == "test-chat").unwrap();
        let effort = p.confirm();
        assert!(effort.is_some_and(|(_, e)| e.is_none()));
    }

    // 14. fast_model_id is set from capabilities.
    #[test]
    fn fast_model_from_capabilities() {
        let p = make_picker();
        assert_eq!(p.fast_model_id.as_deref(), Some("test-chat"));
    }

    // 15. render_model_picker does not panic for a default-area call.
    #[test]
    fn render_does_not_panic() {
        let mut p = make_picker();
        p.open("test-reasoning");
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        render_model_picker(&p, area, &mut buf);
    }

    // 16. render does nothing when not visible.
    #[test]
    fn render_noop_when_hidden() {
        let p = ModelPickerState::new();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render_model_picker(&p, area, &mut buf);
        for cell in buf.content() {
            assert_eq!(
                cell.symbol(),
                " ",
                "buffer should be empty when picker is hidden"
            );
        }
    }

    // 17. models_from_metadata preserves display_name and description.
    #[test]
    fn models_from_metadata_preserves_fields() {
        let caps = test_capabilities();
        let entries = ModelPickerState::models_from_metadata(&caps.known_models);
        let first = &entries[0];
        assert_eq!(first.id, "test-reasoning");
        assert_eq!(first.display_name, "Test Reasoning");
        assert_eq!(first.description, "Deep reasoning model");
        assert!(!first.is_current);
    }

    // 18. Empty picker (no init_from_capabilities) still works.
    #[test]
    fn empty_picker_is_safe() {
        let mut p = ModelPickerState::new();
        assert!(p.models.is_empty());
        p.open("anything");
        assert_eq!(p.selected_idx, 0);
        assert!(p.confirm().is_none());
    }
}
