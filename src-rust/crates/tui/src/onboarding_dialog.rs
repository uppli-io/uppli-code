// onboarding_dialog.rs — Interactive first-launch provider setup.
//
// Shown on first launch or via /provider. Walks the user through:
//   1. Provider choice (flèches + Enter)
//   2. API key input (if required)
//   3. Model selection
//   4. Confirmation
// All data comes from the provider registry — zero hardcoded model names.

use cc_api::provider::ModelMetadata;
use cc_api::ProviderPreset;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

use crate::overlays::centered_rect;

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingStep {
    ProviderChoice,
    ApiKey,
    ModelChoice,
    Confirm,
    Done,
}

impl Default for OnboardingStep {
    fn default() -> Self {
        Self::ProviderChoice
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OnboardingDialogState {
    pub visible: bool,
    pub step: OnboardingStep,

    // Provider choice
    pub providers: Vec<ProviderPresetInfo>,
    pub provider_idx: usize,

    // API key
    pub key_input: String,
    pub key_cursor: usize,
    pub key_error: Option<String>,
    pub key_masked: bool,

    // Model choice
    pub models: Vec<ModelInfo>,
    pub model_idx: usize,
    pub fast_model: Option<String>,

    // Result (read after Done)
    pub chosen_provider: Option<String>,
    pub chosen_model: Option<String>,
    pub chosen_fast: Option<String>,
    pub chosen_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderPresetInfo {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub auth_required: bool,
    pub auth_label: String,
    pub auth_env_hint: String,
    pub keychain_key: String,
    pub supports_thinking: bool,
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub supports_thinking: bool,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl OnboardingDialogState {
    pub fn new() -> Self {
        Self {
            visible: false,
            step: OnboardingStep::ProviderChoice,
            providers: Vec::new(),
            provider_idx: 0,
            key_input: String::new(),
            key_cursor: 0,
            key_error: None,
            key_masked: true,
            models: Vec::new(),
            model_idx: 0,
            fast_model: None,
            chosen_provider: None,
            chosen_model: None,
            chosen_fast: None,
            chosen_key: None,
        }
    }

    pub fn show(&mut self) {
        self.visible = true;
        self.step = OnboardingStep::ProviderChoice;
        self.provider_idx = 0;
        self.key_input.clear();
        self.key_cursor = 0;
        self.key_error = None;

        // Load providers from registry
        self.providers = cc_api::provider_registry()
            .iter()
            .map(|p| ProviderPresetInfo {
                name: p.name.to_string(),
                display_name: p.display_name.to_string(),
                description: p.description.to_string(),
                auth_required: p.auth.required,
                auth_label: p.auth.display_label.to_string(),
                auth_env_hint: p.auth.env_vars.first().copied().unwrap_or("").to_string(),
                keychain_key: p.auth.keychain_key.to_string(),
                supports_thinking: p.supports_thinking,
            })
            .collect();
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    pub fn is_done(&self) -> bool {
        self.step == OnboardingStep::Done
    }

    // -- Navigation --

    pub fn select_prev(&mut self) {
        match self.step {
            OnboardingStep::ProviderChoice => {
                if self.provider_idx > 0 {
                    self.provider_idx -= 1;
                } else {
                    self.provider_idx = self.providers.len().saturating_sub(1);
                }
            }
            OnboardingStep::ModelChoice => {
                if self.model_idx > 0 {
                    self.model_idx -= 1;
                } else {
                    self.model_idx = self.models.len().saturating_sub(1);
                }
            }
            _ => {}
        }
    }

    pub fn select_next(&mut self) {
        match self.step {
            OnboardingStep::ProviderChoice => {
                self.provider_idx = (self.provider_idx + 1) % self.providers.len().max(1);
            }
            OnboardingStep::ModelChoice => {
                self.model_idx = (self.model_idx + 1) % self.models.len().max(1);
            }
            _ => {}
        }
    }

    pub fn confirm(&mut self) {
        tracing::info!(step = ?self.step, provider_idx = self.provider_idx, providers_len = self.providers.len(), "onboarding confirm");
        match self.step {
            OnboardingStep::ProviderChoice => {
                if self.providers.is_empty() {
                    tracing::warn!("No providers loaded");
                    return;
                }
                let provider = &self.providers[self.provider_idx];
                self.chosen_provider = Some(provider.name.clone());

                // Load models for this provider
                if let Some(preset) = cc_api::find_preset(&provider.name) {
                    let caps = build_temp_capabilities(preset);
                    self.models = caps
                        .iter()
                        .map(|m| ModelInfo {
                            id: m.id.clone(),
                            display_name: m.display_name.clone(),
                            description: m.description.clone(),
                            supports_thinking: m.supports_thinking,
                        })
                        .collect();
                    self.fast_model = preset.fast_model.map(|s| s.to_string());
                }
                self.model_idx = 0;

                if provider.auth_required {
                    // Check if key already exists in env var or OS keychain
                    let env_key = if !provider.auth_env_hint.is_empty() {
                        std::env::var(&provider.auth_env_hint)
                            .ok()
                            .filter(|k| k.len() > 8)
                    } else {
                        None
                    };
                    let kc_key = cc_core::keychain::get_key(&provider.keychain_key);

                    if env_key.is_some() || kc_key.is_some() {
                        self.chosen_key = env_key.or(kc_key);
                        self.step = OnboardingStep::ModelChoice;
                    } else {
                        self.step = OnboardingStep::ApiKey;
                    }
                } else {
                    self.step = OnboardingStep::ModelChoice;
                }
            }
            OnboardingStep::ApiKey => {
                let key = self.key_input.trim().to_string();
                if key.len() < 8 {
                    self.key_error = Some("Key too short (min 8 chars)".to_string());
                    return;
                }
                self.key_error = None;

                // Store in keychain
                let provider = &self.providers[self.provider_idx];
                if cc_core::keychain::store_key(&provider.keychain_key, &key) {
                    self.key_error = None;
                } else {
                    // Keychain unavailable, keep in memory
                }
                self.chosen_key = Some(key);
                self.step = OnboardingStep::ModelChoice;
            }
            OnboardingStep::ModelChoice => {
                if let Some(m) = self.models.get(self.model_idx) {
                    self.chosen_model = Some(m.id.clone());
                }
                self.chosen_fast = self.fast_model.clone();
                self.step = OnboardingStep::Confirm;
            }
            OnboardingStep::Confirm => {
                // Save to settings
                self.persist_settings();
                self.step = OnboardingStep::Done;
                self.visible = false;
            }
            OnboardingStep::Done => {}
        }
    }

    /// Whether ESC on the first page should quit the app
    /// (true when onboarding is mandatory, i.e. no provider configured).
    pub fn esc_should_quit(&self) -> bool {
        self.step == OnboardingStep::ProviderChoice
    }

    pub fn go_back(&mut self) {
        match self.step {
            OnboardingStep::ProviderChoice => {
                // Can't go back from first page — handled by caller (quit app).
                self.dismiss();
            }
            OnboardingStep::ApiKey => self.step = OnboardingStep::ProviderChoice,
            OnboardingStep::ModelChoice => {
                let provider = &self.providers[self.provider_idx];
                if provider.auth_required && self.chosen_key.is_none() {
                    self.step = OnboardingStep::ApiKey;
                } else {
                    self.step = OnboardingStep::ProviderChoice;
                }
            }
            OnboardingStep::Confirm => self.step = OnboardingStep::ModelChoice,
            OnboardingStep::Done => {}
        }
    }

    // -- API key input --

    pub fn key_insert_char(&mut self, c: char) {
        self.key_input.insert(self.key_cursor, c);
        self.key_cursor += c.len_utf8();
        self.key_error = None;
    }

    pub fn key_backspace(&mut self) {
        if self.key_cursor > 0 {
            let prev = self.key_input[..self.key_cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.key_input.remove(prev);
            self.key_cursor = prev;
        }
    }

    pub fn key_toggle_mask(&mut self) {
        self.key_masked = !self.key_masked;
    }

    // -- Persistence --

    fn persist_settings(&self) {
        if let Ok(mut settings) = cc_core::config::Settings::load_sync() {
            if let Some(ref provider_name) = self.chosen_provider {
                if let Some(preset) = cc_api::find_preset(provider_name) {
                    settings.config.provider = preset.provider_type.clone();
                }
                let ps = settings
                    .config
                    .providers
                    .entry(provider_name.clone())
                    .or_default();
                ps.model = self.chosen_model.clone();
                ps.fast_model = self.chosen_fast.clone();
                let provider = &self.providers[self.provider_idx];
                ps.supports_thinking = Some(provider.supports_thinking);
            }
            settings.has_completed_onboarding = true;
            let _ = settings.save_sync();
        }
    }
}

impl Default for OnboardingDialogState {
    fn default() -> Self {
        Self::new()
    }
}

fn build_temp_capabilities(preset: &ProviderPreset) -> Vec<ModelMetadata> {
    // Return the known_models from the provider preset.
    // We can't construct a full provider without the API key,
    // but the preset's known_models are embedded in the OpenAiProviderConfig presets.
    // For now, build from the preset info.
    let mut models = Vec::new();

    // The preset has default_model and fast_model as &str.
    // We look up the full known_models from the provider factory.
    // Since we can't construct the provider, use the preset's static data.
    models.push(ModelMetadata {
        id: preset.default_model.to_string(),
        display_name: format!("{} (default)", preset.default_model),
        description: if preset.supports_thinking {
            "Reasoning model".to_string()
        } else {
            "Default model".to_string()
        },
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_thinking: preset.supports_thinking,
        pricing: None,
    });

    if let Some(fast) = preset.fast_model {
        models.push(ModelMetadata {
            id: fast.to_string(),
            display_name: format!("{} (fast)", fast),
            description: "Fast model for tool results".to_string(),
            context_window: 128_000,
            max_output_tokens: 16_384,
            supports_thinking: false,
            pricing: None,
        });
    }

    models
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub fn render_onboarding_dialog(frame: &mut Frame, state: &OnboardingDialogState, area: Rect) {
    if !state.visible {
        return;
    }

    let w = 60u16.min(area.width.saturating_sub(4));
    let h = 22u16.min(area.height.saturating_sub(4));
    let dialog = centered_rect(w, h, area);

    frame.render_widget(Clear, dialog);

    match state.step {
        OnboardingStep::ProviderChoice => render_provider_choice(frame, state, dialog),
        OnboardingStep::ApiKey => render_api_key(frame, state, dialog),
        OnboardingStep::ModelChoice => render_model_choice(frame, state, dialog),
        OnboardingStep::Confirm => render_confirm(frame, state, dialog),
        OnboardingStep::Done => {}
    }
}

fn render_provider_choice(frame: &mut Frame, state: &OnboardingDialogState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            " Uppli Code ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )]))
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Choose your LLM provider:",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ];

    for (i, p) in state.providers.iter().enumerate() {
        let marker = if i == state.provider_idx {
            "  \u{25b8} "
        } else {
            "    "
        };
        let style = if i == state.provider_idx {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let desc_style = if i == state.provider_idx {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), style),
            Span::styled(format!("{:<17}", p.display_name), style),
            Span::styled(p.description.clone(), desc_style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  \u{2191}\u{2193} navigate   Enter select   Esc cancel",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )]));

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, frame.buffer_mut());
}

fn render_api_key(frame: &mut Frame, state: &OnboardingDialogState, area: Rect) {
    let provider = &state.providers[state.provider_idx];
    let title = format!(" {} API Key ", provider.auth_label);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]))
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let display_key = if state.key_masked && !state.key_input.is_empty() {
        let len = state.key_input.len();
        if len > 4 {
            format!("{}{}", "*".repeat(len - 4), &state.key_input[len - 4..])
        } else {
            "*".repeat(len)
        }
    } else {
        state.key_input.clone()
    };

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("  Enter your {} API key:", provider.auth_label),
            Style::default().fg(Color::White),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                if display_key.is_empty() {
                    "sk-...".to_string()
                } else {
                    display_key
                },
                Style::default().fg(if state.key_input.is_empty() {
                    Color::DarkGray
                } else {
                    Color::Cyan
                }),
            ),
            Span::styled("\u{2588}", Style::default().fg(Color::White)), // cursor
        ]),
        Line::from(""),
    ];

    if let Some(ref err) = state.key_error {
        lines.push(Line::from(vec![Span::styled(
            format!("  {}", err),
            Style::default().fg(Color::Red),
        )]));
        lines.push(Line::from(""));
    }

    if !provider.auth_env_hint.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  Tip: export {}=sk-...", provider.auth_env_hint),
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![Span::styled(
        "  Enter confirm   Esc back   Tab show/hide key",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )]));

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, frame.buffer_mut());
}

fn render_model_choice(frame: &mut Frame, state: &OnboardingDialogState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            " Choose Model ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )]))
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Select your main model:",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ];

    for (i, m) in state.models.iter().enumerate() {
        let marker = if i == state.model_idx {
            "  \u{25b8} "
        } else {
            "    "
        };
        let style = if i == state.model_idx {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), style),
            Span::styled(m.display_name.clone(), style),
            Span::styled(format!("  {}", m.description), Style::default().fg(Color::DarkGray)),
        ]));
    }

    if let Some(ref fast) = state.fast_model {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  Fast model: ", Style::default().fg(Color::DarkGray)),
            Span::styled(fast.clone(), Style::default().fg(Color::Yellow)),
            Span::styled(" (for tool results)", Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  \u{2191}\u{2193} navigate   Enter select   Esc back",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )]));

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, frame.buffer_mut());
}

fn render_confirm(frame: &mut Frame, state: &OnboardingDialogState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            " Ready ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )]))
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let provider_name = state
        .providers
        .get(state.provider_idx)
        .map(|p| p.display_name.as_str())
        .unwrap_or("?");

    let model = state.chosen_model.as_deref().unwrap_or("?");
    let fast = state
        .chosen_fast
        .as_deref()
        .unwrap_or("none");

    let key_display = if state.chosen_key.is_some() {
        "\u{2713} keychain"
    } else {
        "not needed"
    };

    let lines: Vec<Line<'static>> = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Provider:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                provider_name.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Model:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(model.to_string(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Fast:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(fast.to_string(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Key:       ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                key_display.to_string(),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(""),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Enter to start coding",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Esc to go back",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )]),
    ];

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, frame.buffer_mut());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn onboarding_defaults_hidden() {
        let state = OnboardingDialogState::new();
        assert!(!state.visible);
    }

    #[test]
    fn onboarding_show_loads_providers() {
        let mut state = OnboardingDialogState::new();
        state.show();
        assert!(state.visible);
        assert!(!state.providers.is_empty());
        assert_eq!(state.step, OnboardingStep::ProviderChoice);
    }

    #[test]
    fn onboarding_navigate_providers() {
        let mut state = OnboardingDialogState::new();
        state.show();
        assert_eq!(state.provider_idx, 0);
        state.select_next();
        assert_eq!(state.provider_idx, 1);
        state.select_prev();
        assert_eq!(state.provider_idx, 0);
    }

    #[test]
    fn onboarding_renders_without_panic() {
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        let mut state = OnboardingDialogState::new();
        state.show();
        terminal
            .draw(|frame| {
                render_onboarding_dialog(frame, &state, frame.area());
            })
            .unwrap();
    }

    #[test]
    fn onboarding_api_key_input() {
        let mut state = OnboardingDialogState::new();
        state.show();
        state.step = OnboardingStep::ApiKey;
        state.key_insert_char('a');
        state.key_insert_char('b');
        assert_eq!(state.key_input, "ab");
        state.key_backspace();
        assert_eq!(state.key_input, "a");
    }
}
