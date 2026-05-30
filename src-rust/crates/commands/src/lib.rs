// cc-commands: Slash command system for the Uppli Code Rust port.
//
// This crate implements the /command framework that allows users to type
// commands like /help, /compact, /clear, /model, /config, /cost, etc.
// Each command is a struct implementing the `SlashCommand` trait.

use async_trait::async_trait;
use cc_core::config::{Config, Settings, Theme};
use cc_core::cost::CostTracker;
use cc_core::types::Message;
use std::collections::BTreeMap;
#[allow(unused_imports)]
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Core trait
// ---------------------------------------------------------------------------

/// Context available to every slash command.
pub struct CommandContext {
    pub config: Config,
    pub cost_tracker: Arc<CostTracker>,
    pub messages: Vec<Message>,
    pub working_dir: std::path::PathBuf,
    pub session_id: String,
    pub session_title: Option<String>,
    /// Remote session URL set when a bridge connection is active.
    pub remote_session_url: Option<String>,
    // Note: config already contains hooks, mcp_servers, etc.
    /// Live MCP manager — present when servers are connected.
    pub mcp_manager: Option<Arc<cc_mcp::McpManager>>,
}

/// Result of running a slash command.
#[derive(Debug)]
pub enum CommandResult {
    /// Display a message to the user (does NOT go to the model).
    Message(String),
    /// Inject a message into the conversation as though the user typed it.
    UserMessage(String),
    /// Modify the configuration.
    ConfigChange(Config),
    /// Modify the configuration and show a specific status message.
    ConfigChangeMessage(Config, String),
    /// Clear the conversation.
    ClearConversation,
    /// Replace the conversation with a specific message list (used by /rewind).
    SetMessages(Vec<Message>),
    /// Load a previously saved session into the live REPL.
    ResumeSession(cc_core::history::ConversationSession),
    /// Update the current session title.
    RenameSession(String),
    /// Trigger the OAuth login flow (handled by the REPL in main.rs).
    /// The bool indicates whether to use Claude.ai auth (true) or Console auth (false).
    StartOAuthFlow(bool),
    /// Exit the REPL.
    Exit,
    /// No visible output.
    Silent,
    /// An error.
    Error(String),
    /// Open the rewind/message-selector overlay in the TUI.
    /// The TUI will call SetMessages when the user confirms.
    OpenRewindOverlay,
}

/// Every slash command implements this trait.
#[async_trait]
pub trait SlashCommand: Send + Sync {
    /// The primary name (without the leading `/`).
    fn name(&self) -> &str;
    /// Alias names (e.g. `["h"]` for `/help`).
    fn aliases(&self) -> Vec<&str> {
        vec![]
    }
    /// One-line description for /help.
    fn description(&self) -> &str;
    /// Detailed help text (shown by `/help <command>`).
    fn help(&self) -> &str {
        self.description()
    }
    /// Whether this command is visible in /help output.
    fn hidden(&self) -> bool {
        false
    }
    /// Execute the command with the given arguments string.
    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult;
}

// ---------------------------------------------------------------------------
// Built-in commands
// ---------------------------------------------------------------------------

pub struct HelpCommand;
pub struct ClearCommand;
pub struct CompactCommand;
pub struct CostCommand;
pub struct ExitCommand;
pub struct ModelCommand;
pub struct ConfigCommand;
pub struct ColorCommand;
pub struct VersionCommand;
pub struct ResumeCommand;
pub struct StatusCommand;
pub struct DiffCommand;
pub struct MemoryCommand;
pub struct BugCommand;
pub struct UsageCommand;
pub struct DoctorCommand;
pub struct LoginCommand;
pub struct LogoutCommand;
pub struct InitCommand;
pub struct ReviewCommand;
pub struct HooksCommand;
pub struct McpCommand;
pub struct PermissionsCommand;
pub struct PlanCommand;
pub struct TasksCommand;
pub struct SessionCommand;
pub struct ThinkingCommand;
// New commands
pub struct ExportCommand;
pub struct SkillsCommand;
pub struct RewindCommand;
pub struct StatsCommand;
pub struct FilesCommand;
pub struct RenameCommand;
pub struct EffortCommand;
pub struct SummaryCommand;
pub struct CommitCommand;
pub struct PluginCommand;
pub struct ReloadPluginsCommand;
pub struct ThemeCommand;
pub struct OutputStyleCommand;
pub struct KeybindingsCommand;
pub struct PrivacySettingsCommand;
// Batch-1 new commands
pub struct RemoteControlCommand;
pub struct RemoteEnvCommand;
pub struct ContextCommand;
pub struct CopyCommand;
pub struct ChromeCommand;
pub struct VimCommand;
pub struct VoiceCommand;
pub struct UpgradeCommand;
pub struct ReleaseNotesCommand;
pub struct RateLimitOptionsCommand;
pub struct StatuslineCommand;
pub struct SecurityReviewCommand;
pub struct TerminalSetupCommand;
pub struct ExtraUsageCommand;
pub struct FastCommand;
pub struct ThinkBackCommand;
pub struct ThinkBackPlayCommand;
pub struct FeedbackCommand;
pub struct ColorSetCommand;
// New commands: share, teleport, btw, ctx-viz, sandbox-toggle
pub struct ShareCommand;
pub struct TeleportCommand;
pub struct BtwCommand;
pub struct CtxVizCommand;
pub struct SandboxToggleCommand;
pub struct HeapdumpCommand;
pub struct InsightsCommand;
pub struct UltrareviewCommand;
pub struct AdvisorCommand;
pub struct InstallSlackAppCommand;
pub struct NamedCommandAdapter {
    pub slash_name: &'static str,
    pub target_name: &'static str,
    pub slash_aliases: &'static [&'static str],
    pub slash_description: &'static str,
    pub slash_help: &'static str,
}

#[derive(serde::Serialize)]
struct KeybindingTemplateFile {
    #[serde(rename = "$schema")]
    schema: &'static str,
    #[serde(rename = "$docs")]
    docs: &'static str,
    bindings: Vec<KeybindingTemplateBlock>,
}

#[derive(serde::Serialize)]
struct KeybindingTemplateBlock {
    context: String,
    bindings: BTreeMap<String, Option<String>>,
}

fn save_settings_mutation<F>(mutate: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut Settings),
{
    let mut settings = Settings::load_sync()?;
    mutate(&mut settings);
    settings.save_sync()
}

fn open_with_system(target: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let ps_cmd = format!("Start-Process '{}'", target.replace('\'', "''"));
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(())
    }
}

fn format_keystroke(keystroke: &cc_core::keybindings::ParsedKeystroke) -> String {
    let mut parts = Vec::new();
    if keystroke.ctrl {
        parts.push("ctrl".to_string());
    }
    if keystroke.alt {
        parts.push("alt".to_string());
    }
    if keystroke.shift {
        parts.push("shift".to_string());
    }
    if keystroke.meta {
        parts.push("meta".to_string());
    }
    parts.push(match keystroke.key.as_str() {
        "space" => "space".to_string(),
        other => other.to_string(),
    });
    parts.join("+")
}

fn format_chord(chord: &[cc_core::keybindings::ParsedKeystroke]) -> String {
    chord
        .iter()
        .map(format_keystroke)
        .collect::<Vec<_>>()
        .join(" ")
}

fn generate_keybindings_template() -> anyhow::Result<String> {
    let mut grouped: BTreeMap<String, BTreeMap<String, Option<String>>> = BTreeMap::new();
    for binding in cc_core::keybindings::default_bindings() {
        let chord = format_chord(&binding.chord);
        if cc_core::keybindings::NON_REBINDABLE.contains(&chord.as_str()) {
            continue;
        }
        grouped
            .entry(format!("{:?}", binding.context))
            .or_default()
            .insert(chord, binding.action.clone());
    }

    let template = KeybindingTemplateFile {
        schema: "https://docs.uppli.dev/schemas/keybindings.json",
        docs: "https://docs.uppli.dev/keybindings",
        bindings: grouped
            .into_iter()
            .map(|(context, bindings)| KeybindingTemplateBlock { context, bindings })
            .collect(),
    };

    Ok(format!("{}\n", serde_json::to_string_pretty(&template)?))
}

fn parse_theme(name: &str) -> Option<Theme> {
    match name.trim().to_lowercase().as_str() {
        "default" | "system" => Some(Theme::Default),
        "dark" => Some(Theme::Dark),
        "light" => Some(Theme::Light),
        custom if !custom.is_empty() => Some(Theme::Custom(custom.to_string())),
        _ => None,
    }
}

fn current_output_style_name(config: &Config) -> &str {
    config.output_style.as_deref().unwrap_or("default")
}

fn available_output_style_names() -> Vec<String> {
    cc_core::output_styles::all_styles(&Settings::config_dir())
        .into_iter()
        .map(|style| style.name)
        .collect()
}

fn split_command_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escape = false;

    for ch in args.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' => escape = true,
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            ch if ch.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        out.push(current);
    }

    out
}

fn execute_named_command_from_slash(
    target_name: &str,
    args: &str,
    ctx: &CommandContext,
) -> CommandResult {
    let Some(cmd) = named_commands::find_named_command(target_name) else {
        return CommandResult::Error(format!(
            "Named command '{}' is not available in this build.",
            target_name
        ));
    };

    let parsed_args = split_command_args(args);
    let parsed_refs = parsed_args.iter().map(String::as_str).collect::<Vec<_>>();
    cmd.execute_named(&parsed_refs, ctx)
}

// ---- /help ---------------------------------------------------------------

/// Category labels for help grouping.
fn command_category(name: &str) -> &'static str {
    match name {
        "clear" | "compact" | "rewind" | "summary" | "export" | "rename" | "branch" => {
            "Conversation"
        }
        "model" | "config" | "theme" | "color" | "vim" | "fast" | "effort" | "voice"
        | "statusline" | "output-style" | "keybindings" | "privacy-settings"
        | "rate-limit-options" | "sandbox-toggle" => "Settings",
        "cost" | "stats" | "usage" | "extra-usage" | "context" | "ctx-viz" => "Usage & Cost",
        "status" | "doctor" | "terminal-setup" | "version" | "upgrade" | "release-notes" => {
            "System"
        }
        "login" | "logout" | "permissions" => "Auth & Permissions",
        "memory" | "files" | "diff" | "init" | "commit" | "review" | "security-review" => "Project",
        "mcp" | "hooks" | "ide" | "chrome" => "Integrations",
        "session" | "resume" | "remote-control" | "remote-env" | "share" | "teleport" => {
            "Sessions & Remote"
        }
        "help" | "exit" | "feedback" | "bug" => "General",
        "think-back" | "thinkback-play" | "thinking" | "plan" | "tasks" => "AI & Thinking",
        "copy" | "skills" | "agents" | "plugin" | "reload-plugins" | "stickers" | "passes"
        | "desktop" | "mobile" | "btw" => "Tools & Extras",
        _ => "Other",
    }
}

#[async_trait]
impl SlashCommand for HelpCommand {
    fn name(&self) -> &str {
        "help"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["h", "?"]
    }
    fn description(&self) -> &str {
        "Show available commands and usage information"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        if !args.is_empty() {
            // Show help for a specific command
            if let Some(cmd) = find_command(args) {
                let aliases = cmd.aliases();
                let alias_line = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\nAliases: {}",
                        aliases
                            .iter()
                            .map(|a| format!("/{}", a))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                return CommandResult::Message(format!(
                    "/{name}{aliases}\n{desc}\n\n{help}",
                    name = cmd.name(),
                    aliases = alias_line,
                    desc = cmd.description(),
                    help = cmd.help(),
                ));
            }
            return CommandResult::Error(format!("Unknown command: /{}", args));
        }

        // Grouped output
        let commands = all_commands();
        let visible: Vec<_> = commands.iter().filter(|c| !c.hidden()).collect();

        // Collect categories in stable order
        let category_order = [
            "Conversation",
            "Settings",
            "Usage & Cost",
            "System",
            "Auth & Permissions",
            "Project",
            "Integrations",
            "Sessions & Remote",
            "AI & Thinking",
            "Tools & Extras",
            "General",
            "Other",
        ];

        let mut by_cat: std::collections::HashMap<&str, Vec<String>> =
            std::collections::HashMap::new();

        for cmd in &visible {
            let cat = command_category(cmd.name());
            let aliases = cmd.aliases();
            let alias_str = if aliases.is_empty() {
                String::new()
            } else {
                format!(
                    " ({})",
                    aliases
                        .iter()
                        .map(|a| format!("/{}", a))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            by_cat.entry(cat).or_default().push(format!(
                "  /{:<20} {}",
                format!("{}{}", cmd.name(), alias_str),
                cmd.description()
            ));
        }

        let mut output = String::from("Uppli Code — Slash Commands\n");
        output.push_str("════════════════════════════\n");

        for cat in &category_order {
            if let Some(entries) = by_cat.get(cat) {
                output.push_str(&format!("\n{}\n", cat));
                for entry in entries {
                    output.push_str(&format!("{}\n", entry));
                }
            }
        }

        output.push_str("\nType /help <command> for detailed help on a specific command.");
        CommandResult::Message(output)
    }
}

// ---- /clear --------------------------------------------------------------

#[async_trait]
impl SlashCommand for ClearCommand {
    fn name(&self) -> &str {
        "clear"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["c", "reset", "new"]
    }
    fn description(&self) -> &str {
        "Clear the conversation history"
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        CommandResult::ClearConversation
    }
}

// ---- /compact ------------------------------------------------------------

#[async_trait]
impl SlashCommand for CompactCommand {
    fn name(&self) -> &str {
        "compact"
    }
    fn description(&self) -> &str {
        "Compact the conversation to reduce token usage"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let msg_count = ctx.messages.len();
        let instruction = if args.is_empty() {
            "Provide a detailed summary of our conversation so far, preserving all \
             key technical details, decisions made, file paths mentioned, and current \
             task status."
                .to_string()
        } else {
            args.to_string()
        };

        CommandResult::UserMessage(format!(
            "[Compact requested ({} messages). Instruction: {}]",
            msg_count, instruction
        ))
    }
}

// ---- /cost ---------------------------------------------------------------

#[async_trait]
impl SlashCommand for CostCommand {
    fn name(&self) -> &str {
        "cost"
    }
    fn description(&self) -> &str {
        "Show token usage and estimated cost for this session"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let tracker = &ctx.cost_tracker;
        let cost = tracker.total_cost_usd();
        let cost_line = if cost > 0.0 {
            format!("\n  Estimated cost: ${:.4} (best-effort)", cost)
        } else {
            String::new()
        };
        CommandResult::Message(format!(
            "Session token usage:\n  Input:          {}\n  Output:         {}\n  \
             Cache creation: {}\n  Cache read:     {}\n  Total:          {}{}",
            tracker.input_tokens(),
            tracker.output_tokens(),
            tracker.cache_creation_tokens(),
            tracker.cache_read_tokens(),
            tracker.total_tokens(),
            cost_line,
        ))
    }
}

// ---- /exit ---------------------------------------------------------------

#[async_trait]
impl SlashCommand for ExitCommand {
    fn name(&self) -> &str {
        "exit"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["quit", "q"]
    }
    fn description(&self) -> &str {
        "Exit Uppli Code"
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        CommandResult::Exit
    }
}

// ---- /model --------------------------------------------------------------

#[async_trait]
impl SlashCommand for ModelCommand {
    fn name(&self) -> &str {
        "model"
    }
    fn description(&self) -> &str {
        "Show or change the current model"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        if args.is_empty() {
            CommandResult::Message(format!("Current model: {}", ctx.config.effective_model()))
        } else {
            let mut new_config = ctx.config.clone();
            new_config.model = Some(args.trim().to_string());
            CommandResult::ConfigChange(new_config)
        }
    }
}

// ---- /config -------------------------------------------------------------

#[async_trait]
impl SlashCommand for ConfigCommand {
    fn name(&self) -> &str {
        "config"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["settings"]
    }
    fn description(&self) -> &str {
        "Show or modify configuration settings"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let args = args.trim();
        if args.is_empty() || matches!(args, "show" | "get") {
            let json = serde_json::to_string_pretty(&ctx.config).unwrap_or_default();
            return CommandResult::Message(format!(
                "Current configuration:\n{}\n\nUsage:\n  /config\n  /config set theme <default|dark|light>\n  /config set output-style <default|concise|explanatory|learning|formal|casual>\n  /config set model <model>\n  /config set permission-mode <default|accept-edits|bypass-permissions|plan>\n  /config unset <model|output-style>",
                json
            ));
        }

        if let Some(key) = args.strip_prefix("get ").map(str::trim) {
            return match key {
                "theme" => CommandResult::Message(format!("theme = {:?}", ctx.config.theme)),
                "output-style" | "output_style" => CommandResult::Message(format!(
                    "output-style = {}",
                    current_output_style_name(&ctx.config)
                )),
                "model" => {
                    CommandResult::Message(format!("model = {}", ctx.config.effective_model()))
                }
                "permission-mode" | "permission_mode" => CommandResult::Message(format!(
                    "permission-mode = {:?}",
                    ctx.config.permission_mode
                )),
                other => CommandResult::Error(format!("Unknown config key '{}'", other)),
            };
        }

        if let Some(key) = args.strip_prefix("unset ").map(str::trim) {
            return match key {
                "model" => {
                    let mut new_config = ctx.config.clone();
                    new_config.model = None;
                    if let Err(err) =
                        save_settings_mutation(|settings| settings.config.model = None)
                    {
                        return CommandResult::Error(format!(
                            "Failed to save configuration: {}",
                            err
                        ));
                    }
                    CommandResult::ConfigChangeMessage(
                        new_config,
                        "Model reset to the default for new sessions.".to_string(),
                    )
                }
                "output-style" | "output_style" => {
                    let mut new_config = ctx.config.clone();
                    new_config.output_style = None;
                    if let Err(err) =
                        save_settings_mutation(|settings| settings.config.output_style = None)
                    {
                        return CommandResult::Error(format!(
                            "Failed to save configuration: {}",
                            err
                        ));
                    }
                    CommandResult::ConfigChangeMessage(
                        new_config,
                        "Output style reset to default.".to_string(),
                    )
                }
                other => CommandResult::Error(format!("Unknown config key '{}'", other)),
            };
        }

        let mut parts = args.splitn(3, ' ');
        let command = parts.next().unwrap_or_default();
        let key = parts.next().unwrap_or_default().trim();
        let value = parts.next().unwrap_or_default().trim();
        if command != "set" || key.is_empty() || value.is_empty() {
            return CommandResult::Error("Usage: /config set <key> <value>".to_string());
        }

        match key {
            "theme" => {
                let Some(theme) = parse_theme(value) else {
                    return CommandResult::Error(
                        "Theme must be one of: default, dark, light".to_string(),
                    );
                };
                let mut new_config = ctx.config.clone();
                new_config.theme = theme.clone();
                if let Err(err) =
                    save_settings_mutation(|settings| settings.config.theme = theme.clone())
                {
                    return CommandResult::Error(format!("Failed to save configuration: {}", err));
                }
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!("Theme set to {}.", value.trim().to_lowercase()),
                )
            }
            "output-style" | "output_style" => {
                let normalized = value.trim().to_lowercase();
                let valid = available_output_style_names();
                if !valid.iter().any(|name| name == &normalized) {
                    return CommandResult::Error(format!(
                        "Unsupported output style '{}'. Use one of: {}",
                        value,
                        valid.join(", ")
                    ));
                }

                let mut new_config = ctx.config.clone();
                new_config.output_style = (normalized != "default").then(|| normalized.clone());
                if let Err(err) = save_settings_mutation(|settings| {
                    settings.config.output_style =
                        (normalized != "default").then(|| normalized.clone());
                }) {
                    return CommandResult::Error(format!("Failed to save configuration: {}", err));
                }
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!(
                        "Output style set to {}. Changes take effect on the next request.",
                        normalized
                    ),
                )
            }
            "model" => {
                let mut new_config = ctx.config.clone();
                new_config.model = Some(value.to_string());
                if let Err(err) = save_settings_mutation(|settings| {
                    settings.config.model = Some(value.to_string());
                }) {
                    return CommandResult::Error(format!("Failed to save configuration: {}", err));
                }
                CommandResult::ConfigChangeMessage(new_config, format!("Model set to {}.", value))
            }
            "permission-mode" | "permission_mode" => {
                let mode = match value.trim().to_lowercase().as_str() {
                    "default" => cc_core::config::PermissionMode::Default,
                    "accept-edits" | "accept_edits" => {
                        cc_core::config::PermissionMode::AcceptEdits
                    }
                    "bypass-permissions" | "bypass_permissions" => {
                        cc_core::config::PermissionMode::BypassPermissions
                    }
                    "plan" => cc_core::config::PermissionMode::Plan,
                    _ => {
                        return CommandResult::Error(
                            "Permission mode must be one of: default, accept-edits, bypass-permissions, plan"
                                .to_string(),
                        )
                    }
                };

                let mut new_config = ctx.config.clone();
                new_config.permission_mode = mode.clone();
                if let Err(err) = save_settings_mutation(|settings| {
                    settings.config.permission_mode = mode.clone();
                }) {
                    return CommandResult::Error(format!("Failed to save configuration: {}", err));
                }
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!("Permission mode set to {}.", value.trim().to_lowercase()),
                )
            }
            other => CommandResult::Error(format!("Unknown config key '{}'", other)),
        }
    }
}

// ---- /color --------------------------------------------------------------

#[async_trait]
impl SlashCommand for ColorCommand {
    fn name(&self) -> &str {
        "color"
    }
    fn description(&self) -> &str {
        "Set or show the prompt bar color for this session"
    }
    fn help(&self) -> &str {
        "Usage: /color [<name|#RRGGBB|default>]\n\n\
         Sets the accent color for the prompt bar in this session.\n\
         Named colors: red, green, blue, yellow, cyan, magenta, white, orange, purple\n\
         Hex codes:    #RGB or #RRGGBB\n\
         Reset:        /color default\n\n\
         The color is persisted to ~/.uppli/ui-settings.json and\n\
         applied on the next REPL startup."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let color = args.trim();
        if color.is_empty() {
            let current = load_ui_settings();
            return CommandResult::Message(format!(
                "Current prompt color: {}\n\
                 Use /color <name|#RRGGBB|default> to change it.\n\n\
                 Named colors: red, green, blue, yellow, cyan, magenta, white, orange, purple",
                current.prompt_color.as_deref().unwrap_or("default"),
            ));
        }

        let normalized = if color == "default" {
            None
        } else {
            let known_colors = [
                "red", "green", "blue", "yellow", "cyan", "magenta", "white", "orange", "purple",
                "pink", "gray", "grey",
            ];
            let is_hex = color.starts_with('#')
                && (color.len() == 4 || color.len() == 7)
                && color[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !is_hex && !known_colors.contains(&color.to_lowercase().as_str()) {
                return CommandResult::Error(format!(
                    "Unknown color '{}'. Use a color name (red, green, …) or a hex code (#RGB or #RRGGBB).",
                    color
                ));
            }
            Some(color.to_string())
        };

        match mutate_ui_settings(|s| s.prompt_color = normalized.clone()) {
            Ok(_) => CommandResult::Message(format!(
                "Prompt color set to {}.\n\
                 Restart the REPL for the change to take effect.",
                normalized.as_deref().unwrap_or("default")
            )),
            Err(e) => CommandResult::Error(format!("Failed to save color: {}", e)),
        }
    }
}

// ---- /theme --------------------------------------------------------------

#[async_trait]
impl SlashCommand for ThemeCommand {
    fn name(&self) -> &str {
        "theme"
    }
    fn description(&self) -> &str {
        "Show or change the current theme"
    }
    fn help(&self) -> &str {
        "Usage: /theme [default|dark|light]\n\
         Without arguments, shows the active theme. With an argument, updates the theme for this and future sessions."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let args = args.trim();
        if args.is_empty() {
            return CommandResult::Message(format!(
                "Current theme: {:?}\nUse /theme <default|dark|light> to change it.",
                ctx.config.theme
            ));
        }

        let Some(theme) = parse_theme(args) else {
            return CommandResult::Error("Theme must be one of: default, dark, light".to_string());
        };

        let mut new_config = ctx.config.clone();
        new_config.theme = theme.clone();
        if let Err(err) = save_settings_mutation(|settings| settings.config.theme = theme.clone()) {
            return CommandResult::Error(format!("Failed to save theme: {}", err));
        }

        CommandResult::ConfigChangeMessage(
            new_config,
            format!("Theme set to {}.", args.to_lowercase()),
        )
    }
}

// ---- /output-style -------------------------------------------------------

#[async_trait]
impl SlashCommand for OutputStyleCommand {
    fn name(&self) -> &str {
        "output-style"
    }
    fn description(&self) -> &str {
        "Show or switch the current output style"
    }
    fn help(&self) -> &str {
        "Usage: /output-style [style-name]\n\n\
         With no argument: list available styles and show the current one.\n\
         With a style name: switch to that style (persisted to settings).\n\n\
         Built-in styles: default, verbose, concise\n\
         Plugin-defined styles are listed automatically.\n\n\
         Changes take effect on the next request."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let arg = args.trim();
        let valid_styles = available_output_style_names();
        let current = current_output_style_name(&ctx.config);

        if arg.is_empty() {
            // List available styles
            let mut lines = format!("Current output style: {}\n\nAvailable styles:\n", current);
            for style in &valid_styles {
                let marker = if style == current { " *" } else { "" };
                lines.push_str(&format!("  {}{}\n", style, marker));
            }
            lines.push_str("\nUse /output-style <name> to switch.");
            return CommandResult::Message(lines);
        }

        let normalized = arg.to_lowercase();
        if !valid_styles.iter().any(|name| name == &normalized) {
            return CommandResult::Error(format!(
                "Unknown output style '{}'. Available styles: {}",
                arg,
                valid_styles.join(", ")
            ));
        }

        let mut new_config = ctx.config.clone();
        new_config.output_style = (normalized != "default").then(|| normalized.clone());
        if let Err(err) = save_settings_mutation(|settings| {
            settings.config.output_style = (normalized != "default").then(|| normalized.clone());
        }) {
            return CommandResult::Error(format!("Failed to save configuration: {}", err));
        }

        CommandResult::ConfigChangeMessage(
            new_config,
            format!(
                "Output style set to '{}'. Changes take effect on the next request.",
                normalized
            ),
        )
    }
}

// ---- /keybindings --------------------------------------------------------

#[async_trait]
impl SlashCommand for KeybindingsCommand {
    fn name(&self) -> &str {
        "keybindings"
    }
    fn description(&self) -> &str {
        "Create or open ~/.uppli/keybindings.json"
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let config_dir = Settings::config_dir();
        let path = config_dir.join("keybindings.json");
        let existed = path.exists();

        if !existed {
            if let Err(err) = std::fs::create_dir_all(&config_dir) {
                return CommandResult::Error(format!(
                    "Failed to create {}: {}",
                    config_dir.display(),
                    err
                ));
            }

            let template = match generate_keybindings_template() {
                Ok(template) => template,
                Err(err) => {
                    return CommandResult::Error(format!(
                        "Failed to generate keybindings template: {}",
                        err
                    ))
                }
            };

            if let Err(err) = std::fs::write(&path, template) {
                return CommandResult::Error(format!(
                    "Failed to write {}: {}",
                    path.display(),
                    err
                ));
            }
        }

        match open_with_system(&path.display().to_string()) {
            Ok(_) => CommandResult::Message(if existed {
                format!("Opened {} in your editor.", path.display())
            } else {
                format!(
                    "Created {} with a template and opened it in your editor.",
                    path.display()
                )
            }),
            Err(err) => CommandResult::Message(if existed {
                format!(
                    "Opened {}. Could not launch an editor automatically: {}",
                    path.display(),
                    err
                )
            } else {
                format!(
                    "Created {} with a template. Could not launch an editor automatically: {}",
                    path.display(),
                    err
                )
            }),
        }
    }
}

// ---- /privacy-settings ---------------------------------------------------

#[async_trait]
impl SlashCommand for PrivacySettingsCommand {
    fn name(&self) -> &str {
        "privacy-settings"
    }
    fn description(&self) -> &str {
        "Open Claude privacy settings"
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let url = "https://uppli.dev/settings/data-privacy-controls";
        let fallback = format!("Review and manage your privacy settings at {}", url);
        match open_with_system(url) {
            Ok(_) => CommandResult::Message(format!("Opened privacy settings: {}", url)),
            Err(_) => CommandResult::Message(fallback),
        }
    }
}

// ---- /version ------------------------------------------------------------

#[async_trait]
impl SlashCommand for VersionCommand {
    fn name(&self) -> &str {
        "version"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["v"]
    }
    fn description(&self) -> &str {
        "Show version information"
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        CommandResult::Message(format!(
            "Uppli Code (Rust) v{}",
            cc_core::constants::APP_VERSION
        ))
    }
}

// ---- /resume -------------------------------------------------------------

#[async_trait]
impl SlashCommand for ResumeCommand {
    fn name(&self) -> &str {
        "resume"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["r", "continue"]
    }
    fn description(&self) -> &str {
        "Resume a previous conversation"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        if args.is_empty() {
            let sessions = cc_core::history::list_sessions().await;
            if sessions.is_empty() {
                return CommandResult::Message("No previous sessions found.".to_string());
            }
            let mut output = String::from("Recent sessions:\n\n");
            for (i, session) in sessions.iter().take(10).enumerate() {
                let title = session.title.as_deref().unwrap_or("(untitled)");
                let id_short = &session.id[..session.id.len().min(8)];
                output.push_str(&format!(
                    "  {}. {} - {} ({} messages)\n",
                    i + 1,
                    id_short,
                    title,
                    session.messages.len()
                ));
            }
            output.push_str("\nUse /resume <id> to resume a session.");
            CommandResult::Message(output)
        } else {
            match cc_core::history::load_session(args.trim()).await {
                Ok(session) => CommandResult::ResumeSession(session),
                Err(e) => {
                    CommandResult::Error(format!("Failed to load session {}: {}", args.trim(), e))
                }
            }
        }
    }
}

// ---- /status -------------------------------------------------------------

#[async_trait]
impl SlashCommand for StatusCommand {
    fn name(&self) -> &str {
        "status"
    }
    fn description(&self) -> &str {
        "Show comprehensive system and session status"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        // Auth status
        let auth_status = match cc_core::oauth::OAuthTokens::load().await {
            Some(tokens) => {
                let sub = tokens.subscription_type.as_deref().unwrap_or("oauth");
                format!("Authenticated ({})", sub)
            }
            None => {
                if ctx.config.resolve_api_key().is_some() {
                    "Authenticated (API key)".to_string()
                } else {
                    "Not authenticated".to_string()
                }
            }
        };

        // MCP status
        let mcp_count = ctx.config.mcp_servers.len();
        let mcp_status = if mcp_count == 0 {
            "none configured".to_string()
        } else {
            format!("{} server(s) configured", mcp_count)
        };

        // Hook status
        let hook_count: usize = ctx.config.hooks.values().map(|v| v.len()).sum();

        // UI settings
        let ui = load_ui_settings();
        let editor_mode = ui.editor_mode.as_deref().unwrap_or("normal");
        let fast_mode = ui.fast_mode.unwrap_or(false);

        // Git status
        let git_branch = tokio::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&ctx.working_dir)
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "n/a".to_string());

        CommandResult::Message(format!(
            "Uppli Code Status\n\
             ══════════════════\n\
             Auth:           {auth_status}\n\
             Model:          {model}\n\
             Permission mode: {perm:?}\n\
             Fast mode:      {fast}\n\
             Editor mode:    {editor}\n\n\
             Session\n\
             ───────\n\
             Session ID:     {sid}\n\
             Title:          {title}\n\
             Messages:       {msgs}\n\
             Working dir:    {wd}\n\
             Git branch:     {branch}\n\n\
             Integrations\n\
             ────────────\n\
             MCP servers:    {mcp}\n\
             Hooks:          {hooks} configured\n\n\
             Usage\n\
             ─────\n\
             {summary}",
            auth_status = auth_status,
            model = ctx.config.effective_model(),
            perm = ctx.config.permission_mode,
            fast = if fast_mode { "on" } else { "off" },
            editor = editor_mode,
            sid = &ctx.session_id[..ctx.session_id.len().min(12)],
            title = ctx.session_title.as_deref().unwrap_or("(untitled)"),
            msgs = ctx.messages.len(),
            wd = ctx.working_dir.display(),
            branch = git_branch,
            mcp = mcp_status,
            hooks = hook_count,
            summary = ctx.cost_tracker.summary(),
        ))
    }
}

// ---- /diff ---------------------------------------------------------------

#[async_trait]
impl SlashCommand for DiffCommand {
    fn name(&self) -> &str {
        "diff"
    }
    fn description(&self) -> &str {
        "Show git diff of changes in the working directory"
    }
    fn help(&self) -> &str {
        "Usage: /diff [--stat|--staged|<ref>]\n\n\
         Shows git diff output for the current working directory.\n\n\
         Options:\n\
           /diff           — diff of all unstaged changes (git diff)\n\
           /diff --stat    — summary of changed files\n\
           /diff --staged  — diff of staged changes (git diff --cached)\n\
           /diff <ref>     — diff against a branch, tag, or commit"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let args = args.trim();

        let git_args: Vec<&str> = if args == "--stat" {
            vec!["diff", "--stat"]
        } else if args == "--staged" || args == "--cached" {
            vec!["diff", "--cached"]
        } else if args.is_empty() {
            vec!["diff"]
        } else {
            // Treat as a ref
            vec!["diff", args]
        };

        let output = tokio::process::Command::new("git")
            .args(&git_args)
            .current_dir(&ctx.working_dir)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() || out.status.code() == Some(1) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.trim().is_empty() {
                    CommandResult::Message(
                        "No changes found. Working tree is clean (or not a git repository)."
                            .to_string(),
                    )
                } else {
                    // Truncate very long diffs
                    let text = stdout.as_ref();
                    let display = if text.len() > 8000 {
                        format!(
                            "{}\n… (truncated — {} total bytes; use `git diff` for full output)",
                            &text[..8000],
                            text.len()
                        )
                    } else {
                        text.to_string()
                    };
                    CommandResult::Message(format!("Changes:\n{}", display))
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                CommandResult::Error(format!(
                    "git diff failed (exit {}): {}",
                    out.status.code().unwrap_or(-1),
                    stderr.trim()
                ))
            }
            Err(e) => CommandResult::Error(format!("Failed to run git diff: {}", e)),
        }
    }
}

// ---- /memory -------------------------------------------------------------

#[async_trait]
impl SlashCommand for MemoryCommand {
    fn name(&self) -> &str {
        "memory"
    }
    fn description(&self) -> &str {
        "View UPPLI.md memory files (project and global)"
    }
    fn help(&self) -> &str {
        "Usage: /memory [edit]\n\n\
         Shows the content of UPPLI.md files that provide project context to Claude.\n\
         Claude reads these files automatically at session start.\n\n\
         Locations checked (in priority order):\n\
         1. <project>/.uppli/UPPLI.md\n\
         2. <project>/UPPLI.md\n\
         3. ~/.uppli/UPPLI.md  (global memory)\n\n\
         Use /memory edit to open the project UPPLI.md in your editor.\n\
         Use /init to create a new UPPLI.md from a template."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let project_claude_dir = ctx.working_dir.join(".uppli").join("UPPLI.md");
        let project_root = ctx.working_dir.join("UPPLI.md");
        let global_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".uppli")
            .join("UPPLI.md");

        let locations = [
            ("project (.uppli/UPPLI.md)", project_claude_dir.clone()),
            ("project (UPPLI.md)", project_root.clone()),
            ("global (~/.uppli/UPPLI.md)", global_path.clone()),
        ];

        let edit_mode = args.trim() == "edit";

        if edit_mode {
            // Open best available UPPLI.md
            let target = if project_root.exists() {
                project_root.clone()
            } else if project_claude_dir.exists() {
                project_claude_dir.clone()
            } else {
                project_root.clone() // will be created by editor
            };
            return match open_with_system(&target.display().to_string()) {
                Ok(_) => {
                    CommandResult::Message(format!("Opening {} in your editor.", target.display()))
                }
                Err(e) => CommandResult::Message(format!(
                    "Could not launch editor: {}. Edit {} manually.",
                    e,
                    target.display()
                )),
            };
        }

        let mut output = String::from("UPPLI.md Memory Files\n══════════════════════\n");
        let mut found_any = false;

        for (label, path) in &locations {
            if path.exists() {
                found_any = true;
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => {
                        let lines: usize = content.lines().count();
                        let chars = content.len();
                        output.push_str(&format!(
                            "\n[{label}]\nPath: {path}\nSize: {lines} lines, {chars} chars\n\
                             ─────────────────────────────────\n\
                             {content}\n",
                            label = label,
                            path = path.display(),
                            lines = lines,
                            chars = chars,
                            content = if content.len() > 2000 {
                                format!(
                                    "{}…\n(truncated — file is {} chars)",
                                    &content[..2000],
                                    chars
                                )
                            } else {
                                content.clone()
                            }
                        ));
                    }
                    Err(e) => output.push_str(&format!(
                        "\n[{label}] — Error reading {}: {}\n",
                        path.display(),
                        e,
                        label = label
                    )),
                }
            }
        }

        if !found_any {
            output.push_str(
                "\nNo UPPLI.md files found.\n\
                 Use /init to create one in the current project.",
            );
        } else {
            output.push_str("\nUse /memory edit to open the project UPPLI.md.");
        }

        CommandResult::Message(output)
    }
}

// ---- /bug ----------------------------------------------------------------

#[async_trait]
impl SlashCommand for BugCommand {
    fn name(&self) -> &str {
        "feedback"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["bug"]
    }
    fn description(&self) -> &str {
        "Submit feedback about Uppli Code"
    }
    fn help(&self) -> &str {
        "Usage: /feedback [report]"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let report = args.trim();
        if report.is_empty() {
            CommandResult::Message(
                "To submit feedback or report a bug, visit: https://github.com/anthropics/claude-code/issues"
                    .to_string(),
            )
        } else {
            CommandResult::Message(format!(
                "To submit feedback or report a bug, visit: https://github.com/anthropics/claude-code/issues\nSuggested report summary: {}",
                report
            ))
        }
    }
}

// ---- /usage --------------------------------------------------------------

#[async_trait]
impl SlashCommand for UsageCommand {
    fn name(&self) -> &str {
        "usage"
    }
    fn description(&self) -> &str {
        "Show API usage, quotas, and rate limit status"
    }
    fn help(&self) -> &str {
        "Usage: /usage\n\n\
         Shows current session API usage and account quota information.\n\
         For detailed per-call breakdown, use /extra-usage.\n\
         For cost details, use /cost."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let input = ctx.cost_tracker.input_tokens();
        let output = ctx.cost_tracker.output_tokens();
        let cache_creation = ctx.cost_tracker.cache_creation_tokens();
        let cache_read = ctx.cost_tracker.cache_read_tokens();
        let total = ctx.cost_tracker.total_tokens();
        let cost = ctx.cost_tracker.total_cost_usd();

        // Try to get account tier from OAuth tokens
        let account_info = match cc_core::oauth::OAuthTokens::load().await {
            Some(tokens) => {
                let sub = tokens.subscription_type.as_deref().unwrap_or("unknown");
                format!("Plan: {}", sub)
            }
            None => {
                if ctx.config.resolve_api_key().is_some() {
                    "Plan: API key (Console billing)".to_string()
                } else {
                    "Plan: not authenticated — run /login".to_string()
                }
            }
        };

        let cost_line = if cost > 0.0 {
            format!("Estimated cost: ${cost:.4} (best-effort)\n\n", cost = cost)
        } else {
            String::new()
        };

        CommandResult::Message(format!(
            "API Usage — Current Session\n\
             ────────────────────────────\n\
             {account_info}\n\
             Model:          {model}\n\n\
             Tokens used this session:\n\
               Input:        {input:>10}\n\
               Output:       {output:>10}\n\
               Cache write:  {cache_creation:>10}\n\
               Cache read:   {cache_read:>10}\n\
               Total:        {total:>10}\n\n\
             {cost_line}For authoritative billing, see your provider dashboard.\n\
             Use /extra-usage for per-call breakdown.\n\
             Use /rate-limit-options to see your plan limits.",
            account_info = account_info,
            model = ctx.config.effective_model(),
            input = input,
            output = output,
            cache_creation = cache_creation,
            cache_read = cache_read,
            total = total,
            cost_line = cost_line,
        ))
    }
}

// ---- /plugin -------------------------------------------------------------

#[async_trait]
impl SlashCommand for PluginCommand {
    fn name(&self) -> &str {
        "plugin"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["plugins"]
    }
    fn description(&self) -> &str {
        "Manage plugins"
    }
    fn help(&self) -> &str {
        "Usage: /plugin [list|info <name>|enable <name>|disable <name>|install <path>|reload]\n\
         Manage Uppli Code plugins.\n\n\
         Subcommands:\n\
           /plugin              — list all installed plugins\n\
           /plugin list         — list all installed plugins\n\
           /plugin info <name>  — show detailed info about a plugin\n\
           /plugin enable <name>   — enable a plugin (persisted to settings)\n\
           /plugin disable <name>  — disable a plugin (persisted to settings)\n\
           /plugin install <path>  — install a plugin from a local directory\n\
           /plugin reload       — reload plugins from disk"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let project_dir = ctx.working_dir.clone();

        // Helper: prefer the already-loaded global registry, falling back to a
        // fresh disk scan so the command still works without the global being set.
        async fn get_registry(project_dir: &std::path::Path) -> cc_plugins::PluginRegistry {
            if let Some(global) = cc_plugins::global_plugin_registry() {
                let mut reg = cc_plugins::PluginRegistry::new();
                for p in global.all() {
                    reg.insert(p.clone());
                }
                reg
            } else {
                cc_plugins::load_plugins(project_dir, &[]).await
            }
        }

        let parsed = cc_plugins::parse_plugin_args(args);
        match parsed {
            cc_plugins::PluginSubCommand::List => {
                let registry = get_registry(&project_dir).await;
                CommandResult::Message(cc_plugins::format_plugin_list(&registry))
            }
            cc_plugins::PluginSubCommand::Enable(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin enable <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            cc_plugins::PluginSubCommand::Enable(name) => {
                let registry = get_registry(&project_dir).await;
                if registry.get(&name).is_none() {
                    return CommandResult::Error(format!(
                        "Plugin '{}' not found. Use `/plugin list` to see installed plugins.",
                        name
                    ));
                }
                let mut settings = cc_core::config::Settings::load_sync().unwrap_or_default();
                settings.enabled_plugins.insert(name.clone());
                settings.disabled_plugins.remove(&name);
                let _ = settings.save_sync();
                CommandResult::Message(format!(
                    "Plugin '{}' enabled. Run `/plugin reload` to apply changes in this session.",
                    name
                ))
            }
            cc_plugins::PluginSubCommand::Disable(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin disable <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            cc_plugins::PluginSubCommand::Disable(name) => {
                let registry = get_registry(&project_dir).await;
                if registry.get(&name).is_none() {
                    return CommandResult::Error(format!(
                        "Plugin '{}' not found. Use `/plugin list` to see installed plugins.",
                        name
                    ));
                }
                let mut settings = cc_core::config::Settings::load_sync().unwrap_or_default();
                settings.disabled_plugins.insert(name.clone());
                settings.enabled_plugins.remove(&name);
                let _ = settings.save_sync();
                CommandResult::Message(format!(
                    "Plugin '{}' disabled. Run `/plugin reload` to apply changes in this session.",
                    name
                ))
            }
            cc_plugins::PluginSubCommand::Info(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin info <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            cc_plugins::PluginSubCommand::Info(name) => {
                let registry = get_registry(&project_dir).await;
                CommandResult::Message(cc_plugins::format_plugin_info(&registry, &name))
            }
            cc_plugins::PluginSubCommand::Install(ref path) if path.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin install <path>\nProvide the path to a local plugin directory."
                        .to_string(),
                )
            }
            cc_plugins::PluginSubCommand::Install(path) => {
                let result = cc_plugins::install_plugin_from_path(std::path::Path::new(&path));
                match result {
                    Ok(name) => CommandResult::Message(format!(
                        "Plugin '{}' installed successfully. Run `/plugin reload` to activate it.",
                        name
                    )),
                    Err(e) => CommandResult::Error(format!("Install failed: {}", e)),
                }
            }
            cc_plugins::PluginSubCommand::Reload => {
                let old_registry = get_registry(&project_dir).await;
                let (new_registry, diff) =
                    cc_plugins::reload_plugins(&old_registry, &project_dir, &[]).await;
                CommandResult::Message(cc_plugins::format_reload_summary(&new_registry, &diff))
            }
            cc_plugins::PluginSubCommand::Help => CommandResult::Message(
                "Plugin commands:\n\
                     /plugin              — list all installed plugins\n\
                     /plugin list         — list all installed plugins\n\
                     /plugin info <name>  — show plugin details\n\
                     /plugin enable <name>   — enable a plugin\n\
                     /plugin disable <name>  — disable a plugin\n\
                     /plugin install <path>  — install plugin from local path\n\
                     /plugin reload       — reload plugins from disk"
                    .to_string(),
            ),
        }
    }
}

// ---- /reload-plugins -----------------------------------------------------

#[async_trait]
impl SlashCommand for ReloadPluginsCommand {
    fn name(&self) -> &str {
        "reload-plugins"
    }
    fn description(&self) -> &str {
        "Reload all plugins without restarting"
    }
    fn help(&self) -> &str {
        "Usage: /reload-plugins\n\
         Reloads all plugins and shows what changed."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let project_dir = ctx.working_dir.clone();

        let old_registry = cc_plugins::load_plugins(&project_dir, &[]).await;
        let (new_registry, diff) =
            cc_plugins::reload_plugins(&old_registry, &project_dir, &[]).await;

        CommandResult::Message(cc_plugins::format_reload_summary(&new_registry, &diff))
    }
}

// ---- Plugin slash command adapter ----------------------------------------

/// Wraps a plugin-defined `PluginCommandDef` so it can be executed like a
/// built-in slash command.  The adapter is created on-the-fly inside
/// `execute_command` when no built-in matches the input.
pub struct PluginSlashCommandAdapter {
    pub def: cc_plugins::PluginCommandDef,
}

#[async_trait]
impl SlashCommand for PluginSlashCommandAdapter {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        match &self.def.run_action {
            cc_plugins::CommandRunAction::StaticResponse(msg) => {
                CommandResult::Message(msg.clone())
            }
            cc_plugins::CommandRunAction::MarkdownPrompt {
                file_path,
                plugin_root: _,
            } => {
                // Read the markdown file and inject it into the conversation
                match std::fs::read_to_string(file_path) {
                    Ok(content) => {
                        let full_prompt = if args.is_empty() {
                            content
                        } else {
                            format!("{}\n\nArguments: {}", content, args)
                        };
                        CommandResult::UserMessage(full_prompt)
                    }
                    Err(e) => CommandResult::Error(format!(
                        "Could not read plugin command file '{}': {}",
                        file_path, e
                    )),
                }
            }
            cc_plugins::CommandRunAction::ShellCommand {
                command,
                plugin_root,
            } => {
                let full_cmd = if args.is_empty() {
                    command.clone()
                } else {
                    format!("{} {}", command, args)
                };
                let cmd_result =
                    std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
                        .args(if cfg!(windows) {
                            vec!["/C", &full_cmd]
                        } else {
                            vec!["-c", &full_cmd]
                        })
                        .env("CLAUDE_PLUGIN_ROOT", plugin_root)
                        .output();
                match cmd_result {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        if out.status.success() {
                            CommandResult::Message(stdout.to_string())
                        } else {
                            CommandResult::Error(format!("Command failed:\n{}", stderr))
                        }
                    }
                    Err(e) => CommandResult::Error(format!("Failed to run command: {}", e)),
                }
            }
        }
    }
}

// ---- /doctor -------------------------------------------------------------

#[async_trait]
impl SlashCommand for DoctorCommand {
    fn name(&self) -> &str {
        "doctor"
    }
    fn description(&self) -> &str {
        "Check system health and diagnose issues"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let mut lines: Vec<String> = Vec::new();

        // ── Header ─────────────────────────────────────────────────────────
        lines.push(format!(
            "Uppli Code v{}  |  {}",
            cc_core::constants::APP_VERSION,
            std::env::consts::OS,
        ));
        lines.push(String::new());

        // ── API / Auth ──────────────────────────────────────────────────────
        lines.push("Authentication".to_string());
        if ctx.config.resolve_api_key().is_some() {
            lines.push("  ✓ API key configured".to_string());
        } else {
            // Check for OAuth token as fallback
            let oauth_path = cc_core::config::Settings::config_dir().join("credentials.json");
            if oauth_path.exists() {
                lines.push("  ✓ OAuth credentials found".to_string());
            } else {
                lines
                    .push("  ✗ No API key found — set ANTHROPIC_API_KEY or run /login".to_string());
            }
        }
        // Show which model is active
        lines.push(format!(
            "  • Active model: {}",
            ctx.config.effective_model()
        ));
        lines.push(String::new());

        // ── Git ─────────────────────────────────────────────────────────────
        lines.push("Tools".to_string());
        let git_out = tokio::process::Command::new("git")
            .arg("--version")
            .output()
            .await;
        match git_out {
            Ok(o) if o.status.success() => {
                let ver = String::from_utf8_lossy(&o.stdout).trim().to_string();
                lines.push(format!("  ✓ {ver}"));
            }
            _ => lines.push("  ✗ git not found — many features require git".to_string()),
        }

        // Ripgrep
        let rg_out = tokio::process::Command::new("rg")
            .arg("--version")
            .output()
            .await;
        match rg_out {
            Ok(o) if o.status.success() => {
                let first = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                lines.push(format!("  ✓ ripgrep: {first}"));
            }
            _ => lines.push(
                "  ⚠ ripgrep (rg) not found — Grep tool will fall back to built-in".to_string(),
            ),
        }
        lines.push(String::new());

        // ── Config directory ────────────────────────────────────────────────
        lines.push("Configuration".to_string());
        let config_dir = cc_core::config::Settings::config_dir();
        if config_dir.exists() {
            lines.push(format!("  ✓ Config dir: {}", config_dir.display()));
        } else {
            lines.push(format!("  ✗ Config dir missing: {}", config_dir.display()));
        }

        // Settings validation
        let settings_path = config_dir.join("settings.json");
        if settings_path.exists() {
            match std::fs::read_to_string(&settings_path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            {
                Some(_) => lines.push("  ✓ settings.json valid JSON".to_string()),
                None => {
                    lines.push("  ✗ settings.json is invalid — run /config to repair".to_string())
                }
            }
        } else {
            lines.push("  • settings.json not found (defaults will be used)".to_string());
        }

        // UPPLI.md
        let claude_md = std::env::current_dir().ok().map(|d| d.join("UPPLI.md"));
        if claude_md.as_deref().map(|p| p.exists()).unwrap_or(false) {
            lines.push("  ✓ UPPLI.md present in working directory".to_string());
        } else {
            lines
                .push("  • No UPPLI.md in working directory (run /init to create one)".to_string());
        }
        lines.push(String::new());

        // ── MCP servers ─────────────────────────────────────────────────────
        lines.push("MCP Servers".to_string());
        let mcp_count = ctx.config.mcp_servers.len();
        if mcp_count == 0 {
            lines.push("  • No MCP servers configured".to_string());
        } else {
            lines.push(format!("  ✓ {mcp_count} MCP server(s) configured:"));
            for srv in ctx.config.mcp_servers.iter().take(8) {
                lines.push(format!("    - {}", srv.name));
            }
            if mcp_count > 8 {
                lines.push(format!("    … and {} more", mcp_count - 8));
            }
        }
        lines.push(String::new());

        // ── Hooks ───────────────────────────────────────────────────────────
        lines.push("Hooks".to_string());
        let hook_count: usize = ctx.config.hooks.values().map(|v| v.len()).sum();
        if hook_count == 0 {
            lines.push("  • No hooks configured".to_string());
        } else {
            lines.push(format!(
                "  ✓ {hook_count} hook(s) configured across {} event(s)",
                ctx.config.hooks.len()
            ));
        }
        lines.push(String::new());

        // ── Session / lock ──────────────────────────────────────────────────
        lines.push("Session".to_string());
        let lock_path = config_dir.join("claude.lock");
        if lock_path.exists() {
            lines.push("  ⚠ Lock file exists — another instance may be running".to_string());
        } else {
            lines.push("  ✓ No stale lock file".to_string());
        }

        // Working directory
        if let Ok(cwd) = std::env::current_dir() {
            lines.push(format!("  • Working dir: {}", cwd.display()));
        }
        lines.push(String::new());

        // ── Available tools ─────────────────────────────────────────────────
        lines.push("Built-in Tools".to_string());
        let tool_count = cc_tools::all_tools().len();
        lines.push(format!("  ✓ {tool_count} built-in tools available"));

        CommandResult::Message(lines.join("\n"))
    }
}

// ---- /login --------------------------------------------------------------

#[async_trait]
impl SlashCommand for LoginCommand {
    fn name(&self) -> &str {
        "login"
    }
    fn description(&self) -> &str {
        "Authenticate with Anthropic (OAuth PKCE flow)"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        // `--console` flag → Console/API-key auth; default → Claude.ai subscription auth
        let login_with_claude_ai = !args.contains("--console");
        CommandResult::StartOAuthFlow(login_with_claude_ai)
    }
}

// ---- /logout -------------------------------------------------------------

#[async_trait]
impl SlashCommand for LogoutCommand {
    fn name(&self) -> &str {
        "logout"
    }
    fn description(&self) -> &str {
        "Clear stored OAuth tokens and API key"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        // Clear OAuth tokens file
        if let Err(e) = cc_core::oauth::OAuthTokens::clear().await {
            return CommandResult::Error(format!("Failed to clear OAuth tokens: {}", e));
        }
        // Also clear any API key stored in settings
        let mut settings = cc_core::config::Settings::load().await.unwrap_or_default();
        settings.config.api_key = None;
        if let Err(e) = settings.save().await {
            return CommandResult::Error(format!("Failed to update settings: {}", e));
        }
        ctx.config.api_key = None;
        CommandResult::Message("Logged out. Credentials cleared.".to_string())
    }
}

// ---- /init ---------------------------------------------------------------

#[async_trait]
impl SlashCommand for InitCommand {
    fn name(&self) -> &str {
        "init"
    }
    fn description(&self) -> &str {
        "Initialize a new project with UPPLI.md"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let path = ctx.working_dir.join("UPPLI.md");
        if path.exists() {
            return CommandResult::Message(format!(
                "UPPLI.md already exists at {}",
                path.display()
            ));
        }

        let default_content = "# Project Instructions\n\n\
            Add project-specific instructions and context here.\n\n\
            ## Guidelines\n\n\
            - Describe your project structure\n\
            - Note any coding conventions\n\
            - List important files and their purposes\n";

        match tokio::fs::write(&path, default_content).await {
            Ok(()) => CommandResult::Message(format!("Created UPPLI.md at {}", path.display())),
            Err(e) => CommandResult::Error(format!("Failed to create UPPLI.md: {}", e)),
        }
    }
}

// ---- /review -------------------------------------------------------------

#[async_trait]
impl SlashCommand for ReviewCommand {
    fn name(&self) -> &str {
        "review"
    }
    fn description(&self) -> &str {
        "Review code changes (git diff)"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let target = if args.is_empty() { "HEAD" } else { args.trim() };
        CommandResult::UserMessage(format!(
            "Please review the code changes in `git diff {}`. \
             Look for bugs, security issues, and style problems.",
            target
        ))
    }
}

// ---- /hooks --------------------------------------------------------------

#[async_trait]
impl SlashCommand for HooksCommand {
    fn name(&self) -> &str {
        "hooks"
    }
    fn description(&self) -> &str {
        "Show configured event hooks"
    }
    fn help(&self) -> &str {
        "Usage: /hooks\n\
         Show hooks configured in settings.json under 'hooks'.\n\
         Hooks fire shell commands on events: PreToolUse, PostToolUse, Stop, UserPromptSubmit."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        if ctx.config.hooks.is_empty() {
            return CommandResult::Message(
                "No hooks configured.\n\
                 Add hooks to ~/.uppli/settings.json under the 'hooks' key.\n\
                 Example:\n  \"hooks\": { \"PreToolUse\": [{\"command\": \"echo $STDIN\", \"blocking\": false}] }"
                    .to_string(),
            );
        }

        let mut lines = vec!["Configured hooks:".to_string()];
        for (event, entries) in &ctx.config.hooks {
            lines.push(format!("\n  {:?} ({} entries):", event, entries.len()));
            for e in entries {
                let filter = e.tool_filter.as_deref().unwrap_or("*");
                lines.push(format!(
                    "    - [{}] {} (blocking={})",
                    filter, e.command, e.blocking
                ));
            }
        }

        CommandResult::Message(lines.join("\n"))
    }
}

// ---- /mcp ----------------------------------------------------------------

#[async_trait]
impl SlashCommand for McpCommand {
    fn name(&self) -> &str {
        "mcp"
    }
    fn description(&self) -> &str {
        "Show MCP server status and manage connections"
    }
    fn help(&self) -> &str {
        "Usage: /mcp [list|status|auth <server>|connect <server>|logs <server>|resources|prompts|get-prompt ...]\n\n\
         Manages Model Context Protocol (MCP) servers.\n\
         MCP servers extend Claude with external tools, resources, and prompt templates.\n\n\
         Subcommands:\n\
           /mcp                        — list configured servers with live status\n\
           /mcp list                   — same as above\n\
           /mcp status                 — detailed connection status for all servers\n\
           /mcp auth <server>          — show OAuth auth instructions for a server\n\
           /mcp connect <server>       — reconnect a disconnected server\n\
           /mcp logs <server>          — show recent errors/logs for a server\n\
           /mcp resources [server]     — list resources from connected servers\n\
           /mcp prompts [server]       — list prompt templates from connected servers\n\
           /mcp get-prompt <server> <prompt> [key=value ...]  — expand a prompt template\n\n\
         To add/remove MCP servers, edit ~/.uppli/settings.json\n\
         under the 'mcpServers' key.\n\
         Docs: https://docs.uppli.dev/mcp"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let sub = args.trim();
        let first_word = sub.split_whitespace().next().unwrap_or("");

        // Delegate live-server subcommands (resources/prompts/get-prompt) to the async helper.
        if matches!(first_word, "resources" | "prompts" | "get-prompt") {
            if let Some(result) = McpCommand::handle_live_subcommand(sub, ctx).await {
                return result;
            }
            // Manager not available — fall through to show configured servers
        }

        // /mcp auth <server-name>
        if first_word == "auth" {
            let server_name = sub["auth".len()..].trim();
            if server_name.is_empty() {
                return CommandResult::Error(
                    "Usage: /mcp auth <server-name>\n\
                     Example: /mcp auth my-server"
                        .to_string(),
                );
            }
            return McpCommand::handle_auth(server_name, ctx).await;
        }

        // /mcp tools [server-name]
        if first_word == "tools" {
            let rest = sub["tools".len()..].trim();
            let server_filter = if rest.is_empty() { None } else { Some(rest) };
            return McpCommand::handle_tools(server_filter, ctx);
        }

        // /mcp connect <server-name>
        if first_word == "connect" {
            let server_name = sub["connect".len()..].trim();
            if server_name.is_empty() {
                return CommandResult::Error(
                    "Usage: /mcp connect <server-name>\n\
                     Example: /mcp connect my-server"
                        .to_string(),
                );
            }
            return McpCommand::handle_connect(server_name, ctx).await;
        }

        // /mcp logs <server-name>
        if first_word == "logs" {
            let server_name = sub["logs".len()..].trim();
            if server_name.is_empty() {
                return CommandResult::Error(
                    "Usage: /mcp logs <server-name>\n\
                     Example: /mcp logs my-server"
                        .to_string(),
                );
            }
            return McpCommand::handle_logs(server_name, ctx);
        }

        if ctx.config.mcp_servers.is_empty() {
            return CommandResult::Message(
                "No MCP servers configured.\n\n\
                 To add a MCP server, edit ~/.uppli/settings.json:\n\
                 {\n\
                   \"mcpServers\": [\n\
                     {\n\
                       \"name\": \"my-server\",\n\
                       \"command\": \"npx\",\n\
                       \"args\": [\"-y\", \"@modelcontextprotocol/server-filesystem\", \"/tmp\"]\n\
                     }\n\
                   ]\n\
                 }\n\n\
                 Docs: https://docs.uppli.dev/mcp"
                    .to_string(),
            );
        }

        // /mcp status — detailed status table
        if sub == "status" {
            let mut output = String::from("MCP Server Status\n─────────────────\n");
            for srv in &ctx.config.mcp_servers {
                let kind = match srv.server_type {
                    cc_core::config::McpTransportType::Stdio => "stdio",
                    cc_core::config::McpTransportType::Sse
                    | cc_core::config::McpTransportType::Http => "HTTP/SSE",
                    _ => srv.server_type.as_str(),
                };
                let endpoint = srv
                    .url
                    .as_deref()
                    .or(srv.command.as_deref())
                    .unwrap_or("(unknown)");

                // Fetch live status from the manager if available.
                let live_status = ctx
                    .mcp_manager
                    .as_ref()
                    .map(|m| m.server_status(&srv.name).display())
                    .unwrap_or_else(|| "unknown (manager not active)".to_string());

                output.push_str(&format!(
                    "  {name:20} [{kind:8}] {status}\n    endpoint: {endpoint}\n",
                    name = srv.name,
                    kind = kind,
                    status = live_status,
                    endpoint = endpoint,
                ));
            }
            if ctx.mcp_manager.is_none() {
                output.push_str(
                    "\nNote: MCP manager is not active in this session.\n\
                     Restart Uppli Code to connect to MCP servers.\n\
                     Use /mcp connect <server> to retry a single server.",
                );
            }
            return CommandResult::Message(output);
        }

        // Default: /mcp or /mcp list — show configured servers with live status inline
        let manager = ctx.mcp_manager.as_ref();
        let mut output = format!(
            "Configured MCP Servers ({})\n──────────────────────────\n",
            ctx.config.mcp_servers.len()
        );
        for srv in &ctx.config.mcp_servers {
            let cmd_display = if let Some(ref url) = srv.url {
                format!("url={}", url)
            } else if let Some(ref cmd) = srv.command {
                let args_str = srv.args.join(" ");
                if args_str.is_empty() {
                    cmd.clone()
                } else {
                    format!("{} {}", cmd, args_str)
                }
            } else {
                "(no command)".to_string()
            };

            let status_str = manager
                .map(|m| m.server_status(&srv.name).display())
                .unwrap_or_else(|| "not running".to_string());

            output.push_str(&format!(
                "  {name}  [{status}]\n    type: {type_}  |  {cmd}\n",
                name = srv.name,
                status = status_str,
                type_ = srv.server_type,
                cmd = cmd_display,
            ));
        }
        output.push_str(
            "\nSubcommands: status | auth <server> | connect <server> | logs <server>\n\
             Also: resources | prompts | get-prompt <server> <prompt> [key=val ...]",
        );
        CommandResult::Message(output)
    }
}

impl McpCommand {
    /// Handle `/mcp auth <server>` — initiate OAuth or show auth instructions.
    ///
    /// For HTTP/SSE servers: calls `McpManager::initiate_auth()` to fetch OAuth
    /// metadata, constructs the PKCE authorization URL, attempts to open it in
    /// the system browser, and displays the URL for manual use.
    ///
    /// For stdio servers: shows env-var auth instructions.
    async fn handle_auth(server_name: &str, ctx: &CommandContext) -> CommandResult {
        let srv = match ctx
            .config
            .mcp_servers
            .iter()
            .find(|s| s.name == server_name)
        {
            Some(s) => s,
            None => {
                let configured: Vec<&str> = ctx
                    .config
                    .mcp_servers
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect();
                return CommandResult::Error(format!(
                    "No MCP server named '{}' is configured.\n\
                     Configured servers: {}",
                    server_name,
                    if configured.is_empty() {
                        "(none)".to_string()
                    } else {
                        configured.join(", ")
                    }
                ));
            }
        };

        // If already connected, nothing to do.
        if let Some(manager) = &ctx.mcp_manager {
            use cc_mcp::McpServerStatus;
            match manager.server_status(server_name) {
                McpServerStatus::Connected { tool_count } => {
                    return CommandResult::Message(format!(
                        "MCP server '{}' is already connected ({} tool{} available).\n\
                         No authentication needed.",
                        server_name,
                        tool_count,
                        if tool_count == 1 { "" } else { "s" }
                    ));
                }
                McpServerStatus::Connecting => {
                    return CommandResult::Message(format!(
                        "MCP server '{}' is currently connecting — try again shortly.",
                        server_name
                    ));
                }
                _ => {}
            }
        }

        let is_http = matches!(
            srv.server_type,
            cc_core::config::McpTransportType::Sse | cc_core::config::McpTransportType::Http
        );

        if !is_http {
            // stdio — env-var / API-key auth
            let env_keys: Vec<&str> = srv.env.keys().map(|k| k.as_str()).collect();
            let env_note = if env_keys.is_empty() {
                "No environment variables configured.".to_string()
            } else {
                format!("Configured env vars: {}", env_keys.join(", "))
            };
            let token_note = match cc_mcp::oauth::get_mcp_token(server_name) {
                Some(tok) if !tok.is_expired(60) => " (valid token stored)".to_string(),
                Some(_) => " (stored token is expired)".to_string(),
                None => " (no token stored)".to_string(),
            };
            return CommandResult::Message(format!(
                "MCP Server '{}' (stdio){}\n\
                 {}\n\n\
                 stdio servers authenticate via environment variables (API keys etc.).\n\
                 Add required variables to the 'env' block in ~/.uppli/settings.json,\n\
                 then restart Uppli Code or run /mcp connect {} to reconnect.",
                server_name, token_note, env_note, server_name
            ));
        }

        // HTTP/SSE — use initiate_auth() when the manager is available.
        if let Some(manager) = &ctx.mcp_manager {
            match manager.initiate_auth(server_name).await {
                Ok(auth_url) => {
                    // Best-effort browser open.
                    let _ = open::that(&auth_url);
                    return CommandResult::Message(format!(
                        "MCP OAuth — '{}'\n\
                         Opening browser for authentication...\n\
                         If the browser did not open, visit:\n\n  {}\n\n\
                         After authorizing, the token will be saved to:\n  ~/.uppli/mcp-tokens/{}.json\n\n\
                         Then run /mcp connect {} to reconnect.",
                        server_name, auth_url, server_name, server_name
                    ));
                }
                Err(e) => {
                    let server_url = srv.url.as_deref().unwrap_or("(URL not configured)");
                    return CommandResult::Message(format!(
                        "MCP OAuth — '{}'\n\
                         Could not fetch OAuth metadata: {}\n\n\
                         Manual authentication:\n  Open {} in your browser and complete the OAuth flow.\n\
                         Then run /mcp connect {} to reconnect.",
                        server_name, e, server_url, server_name
                    ));
                }
            }
        }

        // No live manager — static instructions.
        let server_url = srv.url.as_deref().unwrap_or("(URL not configured)");
        let token_note = match cc_mcp::oauth::get_mcp_token(server_name) {
            Some(tok) if !tok.is_expired(60) => " (valid token stored)".to_string(),
            Some(_) => " (stored token is expired)".to_string(),
            None => " (no token stored)".to_string(),
        };
        CommandResult::Message(format!(
            "MCP OAuth Authentication — '{}'{}\n\
             Server URL: {}\n\n\
             To authenticate:\n\
             1. Open the server URL in your browser and complete OAuth\n\
             2. The token is saved to ~/.uppli/mcp-tokens/{}.json\n\
             3. Restart Uppli Code — the token will be used automatically\n\n\
             Token storage: ~/.uppli/mcp-tokens/{}.json",
            server_name, token_note, server_url, server_name, server_name
        ))
    }

    /// Handle `/mcp tools [server]` — list available tools.
    fn handle_tools(server_filter: Option<&str>, ctx: &CommandContext) -> CommandResult {
        let manager = match ctx.mcp_manager.as_ref() {
            Some(m) => m,
            None => {
                return CommandResult::Message(
                    "MCP manager is not active. No tool information available.\n\
                 Restart Uppli Code to connect to MCP servers."
                        .to_string(),
                )
            }
        };

        let all_tools = manager.all_tool_definitions();
        let tools: Vec<_> = if let Some(filter) = server_filter {
            all_tools
                .iter()
                .filter(|(srv, _)| srv.as_str() == filter)
                .collect()
        } else {
            all_tools.iter().collect()
        };

        if tools.is_empty() {
            return CommandResult::Message(if let Some(filter) = server_filter {
                format!(
                    "No tools available from server '{}' (not connected or has no tools).",
                    filter
                )
            } else {
                "No tools available from any connected MCP server.".to_string()
            });
        }

        let title = if let Some(filter) = server_filter {
            format!("MCP Tools — '{}' ({})", filter, tools.len())
        } else {
            format!("MCP Tools — all servers ({})", tools.len())
        };
        let mut out = format!("{}\n{}\n", title, "─".repeat(title.len()));
        let mut last_server = "";
        for (server, tool) in &tools {
            if server.as_str() != last_server && server_filter.is_none() {
                out.push_str(&format!("[{}]\n", server));
                last_server = server.as_str();
            }
            // Strip the "servername_" prefix for display
            let bare = tool
                .name
                .strip_prefix(&format!("{}_", server))
                .unwrap_or(&tool.name);
            let preview: String = tool.description.chars().take(80).collect();
            let ellipsis = if tool.description.len() > 80 {
                "…"
            } else {
                ""
            };
            out.push_str(&format!("  {}\n    {}{}\n", bare, preview, ellipsis));
        }
        CommandResult::Message(out)
    }

    /// Handle `/mcp connect <server>` — attempt to reconnect a server.
    async fn handle_connect(server_name: &str, ctx: &CommandContext) -> CommandResult {
        // Validate that the server is configured.
        if !ctx.config.mcp_servers.iter().any(|s| s.name == server_name) {
            let names: Vec<&str> = ctx
                .config
                .mcp_servers
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            return CommandResult::Error(format!(
                "No MCP server named '{}' is configured.\n\
                 Configured servers: {}",
                server_name,
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names.join(", ")
                }
            ));
        }

        match &ctx.mcp_manager {
            None => {
                // No live manager — give useful instructions.
                CommandResult::Message(format!(
                    "The MCP manager is not running in this session.\n\
                     To connect '{}', restart Uppli Code — servers connect automatically\n\
                     on startup using the configuration in ~/.uppli/settings.json.\n\
                     \n\
                     If the server requires authentication, run /mcp auth {} first.",
                    server_name, server_name
                ))
            }
            Some(manager) => {
                let current = manager.server_status(server_name);
                use cc_mcp::McpServerStatus;
                match current {
                    McpServerStatus::Connected { tool_count } => CommandResult::Message(format!(
                        "MCP server '{}' is already connected ({} tool{} available).",
                        server_name,
                        tool_count,
                        if tool_count == 1 { "" } else { "s" }
                    )),
                    McpServerStatus::Connecting => CommandResult::Message(format!(
                        "MCP server '{}' is already in the process of connecting.\n\
                             Check back in a moment.",
                        server_name
                    )),
                    McpServerStatus::Disconnected { .. } | McpServerStatus::Failed { .. } => {
                        // The McpManager doesn't expose a reconnect method — it's built at
                        // startup.  Inform the user and suggest a restart.
                        CommandResult::Message(format!(
                            "MCP server '{}' is currently disconnected.\n\
                             Status: {}\n\
                             \n\
                             The runtime MCP manager reconnects servers automatically.\n\
                             If the server stays disconnected:\n\
                             1. Check authentication: /mcp auth {}\n\
                             2. Verify the command/URL in ~/.uppli/settings.json\n\
                             3. Restart Uppli Code to force a full reconnect",
                            server_name,
                            manager.server_status(server_name).display(),
                            server_name
                        ))
                    }
                }
            }
        }
    }

    /// Handle `/mcp logs <server>` — show recent error/log information.
    fn handle_logs(server_name: &str, ctx: &CommandContext) -> CommandResult {
        // Validate server name.
        if !ctx.config.mcp_servers.iter().any(|s| s.name == server_name) {
            let names: Vec<&str> = ctx
                .config
                .mcp_servers
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            return CommandResult::Error(format!(
                "No MCP server named '{}' is configured.\n\
                 Configured servers: {}",
                server_name,
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names.join(", ")
                }
            ));
        }

        let mut lines = vec![format!(
            "MCP Server Logs — '{}'\n──────────────────────",
            server_name
        )];

        if let Some(manager) = &ctx.mcp_manager {
            use cc_mcp::McpServerStatus;
            let status = manager.server_status(server_name);
            lines.push(format!("Current status:  {}", status.display()));

            match &status {
                McpServerStatus::Disconnected {
                    last_error: Some(e),
                } => {
                    lines.push(format!("\nLast connection error:\n  {}", e));
                    lines.push(String::new());
                    lines.push("Troubleshooting:".to_string());
                    lines.push(format!(
                        "  /mcp auth {}    — check authentication",
                        server_name
                    ));
                    lines.push(format!(
                        "  /mcp connect {} — attempt reconnect",
                        server_name
                    ));
                }
                McpServerStatus::Failed { error, retry_at } => {
                    lines.push(format!("\nConnection failure:\n  {}", error));
                    let retry_secs = retry_at
                        .saturating_duration_since(std::time::Instant::now())
                        .as_secs();
                    if retry_secs > 0 {
                        lines.push(format!("  Automatic retry in {}s", retry_secs));
                    }
                    let _ = retry_at; // used above
                }
                McpServerStatus::Connected { tool_count } => {
                    lines.push(format!(
                        "\nServer is healthy — {} tool{} available.",
                        tool_count,
                        if *tool_count == 1 { "" } else { "s" }
                    ));
                    // Show catalog info if available.
                    if let Some(catalog) = manager.server_catalog(server_name) {
                        if !catalog.resources.is_empty() {
                            lines.push(format!(
                                "Resources ({}): {}",
                                catalog.resource_count,
                                catalog.resources.join(", ")
                            ));
                        }
                        if !catalog.prompts.is_empty() {
                            lines.push(format!(
                                "Prompts ({}): {}",
                                catalog.prompt_count,
                                catalog.prompts.join(", ")
                            ));
                        }
                    }
                }
                McpServerStatus::Disconnected { last_error: None } => {
                    lines.push("\nServer disconnected cleanly (no error recorded).".to_string());
                    lines.push(format!("Run /mcp connect {} to reconnect.", server_name));
                }
                McpServerStatus::Connecting => {
                    lines.push("\nConnection in progress…".to_string());
                }
            }

            // Show failed server errors from the initial connect_all pass.
            for (name, err) in manager.failed_servers() {
                if name == server_name {
                    lines.push(format!("\nStartup connection error:\n  {}", err));
                    break;
                }
            }
        } else {
            lines.push("MCP manager is not active in this session.".to_string());
            lines.push("Restart Uppli Code to start the MCP runtime.".to_string());
        }

        // Hint about log files.
        lines.push(String::new());
        lines.push(
            "Note: Detailed stdio output from MCP server processes is not\n\
                    captured by the manager. Run the server command directly in a\n\
                    terminal to see its full output."
                .to_string(),
        );

        CommandResult::Message(lines.join("\n"))
    }
}

// Helper: handle async /mcp resources|prompts|get-prompt subcommands via a separate trait impl.
// These need the mcp_manager from CommandContext.
impl McpCommand {
    async fn handle_live_subcommand(sub: &str, ctx: &CommandContext) -> Option<CommandResult> {
        let manager = ctx.mcp_manager.as_ref()?;
        let parts: Vec<&str> = sub.splitn(4, ' ').collect();
        match parts[0] {
            "resources" => {
                let filter = parts.get(1).copied();
                let resources = manager.list_all_resources(filter).await;
                if resources.is_empty() {
                    return Some(CommandResult::Message(
                        "No resources available (servers may not support resources/list)."
                            .to_string(),
                    ));
                }
                let mut out = format!("MCP Resources ({})\n──────────────────\n", resources.len());
                for r in &resources {
                    let server = r.get("server").and_then(|v| v.as_str()).unwrap_or("?");
                    let uri = r.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = r.get("name").and_then(|v| v.as_str()).unwrap_or(uri);
                    let desc = r.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    if desc.is_empty() {
                        out.push_str(&format!("  [{server}] {name}\n    {uri}\n"));
                    } else {
                        out.push_str(&format!("  [{server}] {name} — {desc}\n    {uri}\n"));
                    }
                }
                Some(CommandResult::Message(out))
            }
            "prompts" => {
                let filter = parts.get(1).copied();
                let prompts = manager.list_all_prompts(filter).await;
                if prompts.is_empty() {
                    return Some(CommandResult::Message(
                        "No prompt templates available (servers may not support prompts/list)."
                            .to_string(),
                    ));
                }
                let mut out = format!(
                    "MCP Prompt Templates ({})\n─────────────────────────\n",
                    prompts.len()
                );
                for p in &prompts {
                    let server = p.get("server").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let desc = p.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let args: Vec<String> = p
                        .get("arguments")
                        .and_then(|a| a.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|a| {
                                    a.get("name")
                                        .and_then(|n| n.as_str())
                                        .map(|s| s.to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let args_display = if args.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", args.join(", "))
                    };
                    if desc.is_empty() {
                        out.push_str(&format!("  [{server}] {name}{args_display}\n"));
                    } else {
                        out.push_str(&format!("  [{server}] {name}{args_display} — {desc}\n"));
                    }
                }
                out.push_str("\nUse: /mcp get-prompt <server> <prompt> [key=value ...]\n");
                Some(CommandResult::Message(out))
            }
            "get-prompt" => {
                // /mcp get-prompt <server> <prompt-name> [key=val key2=val2 ...]
                let server = match parts.get(1) {
                    Some(s) => *s,
                    None => {
                        return Some(CommandResult::Error(
                            "Usage: /mcp get-prompt <server> <prompt> [key=value ...]".to_string(),
                        ))
                    }
                };
                let prompt_name = match parts.get(2) {
                    Some(p) => *p,
                    None => {
                        return Some(CommandResult::Error(
                            "Usage: /mcp get-prompt <server> <prompt> [key=value ...]".to_string(),
                        ))
                    }
                };
                let mut args: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                if let Some(kv_str) = parts.get(3) {
                    for kv in kv_str.split_whitespace() {
                        if let Some((k, v)) = kv.split_once('=') {
                            args.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                let arguments = if args.is_empty() { None } else { Some(args) };
                match manager.get_prompt(server, prompt_name, arguments).await {
                    Ok(result) => {
                        let mut injected = String::new();
                        for msg in &result.messages {
                            let text = match &msg.content {
                                cc_mcp::PromptMessageContent::Text { text } => text.clone(),
                                cc_mcp::PromptMessageContent::Image { .. } => "[image]".to_string(),
                                cc_mcp::PromptMessageContent::Resource { resource } => {
                                    resource.to_string()
                                }
                            };
                            injected.push_str(&format!("[{}]: {}\n", msg.role, text));
                        }
                        Some(CommandResult::UserMessage(injected.trim().to_string()))
                    }
                    Err(e) => Some(CommandResult::Error(format!(
                        "Failed to get prompt '{}' from '{}': {}",
                        prompt_name, server, e
                    ))),
                }
            }
            _ => None,
        }
    }
}

// ---- /permissions --------------------------------------------------------

#[async_trait]
impl SlashCommand for PermissionsCommand {
    fn name(&self) -> &str {
        "permissions"
    }
    fn description(&self) -> &str {
        "View or change tool permission settings"
    }
    fn help(&self) -> &str {
        "Usage: /permissions [set <mode>|allow <tool>|deny <tool>|reset]\n\n\
         Modes: default, accept-edits, bypass-permissions, plan\n\n\
         Examples:\n\
           /permissions                    — show current permissions\n\
           /permissions set accept-edits   — auto-accept file edits\n\
           /permissions allow Bash         — allow a specific tool\n\
           /permissions deny Write         — deny a specific tool\n\
           /permissions reset              — clear overrides"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let args = args.trim();

        if args.is_empty() {
            let allowed_display = if ctx.config.allowed_tools.is_empty() {
                "(all tools allowed)".to_string()
            } else {
                ctx.config.allowed_tools.join(", ")
            };
            let denied_display = if ctx.config.disallowed_tools.is_empty() {
                "(none)".to_string()
            } else {
                ctx.config.disallowed_tools.join(", ")
            };
            return CommandResult::Message(format!(
                "Permission Settings\n\
                 ───────────────────\n\
                 Mode:          {:?}\n\
                 Allowed tools: {}\n\
                 Denied tools:  {}\n\n\
                 Use /permissions set <mode> to change the permission mode.\n\
                 Use /permissions allow|deny <tool> to override individual tools.\n\
                 Use /permissions reset to clear all overrides.",
                ctx.config.permission_mode, allowed_display, denied_display,
            ));
        }

        let mut parts = args.splitn(2, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let arg = parts.next().unwrap_or("").trim();

        match sub {
            "set" => {
                let mode = match arg.to_lowercase().as_str() {
                    "default" => cc_core::config::PermissionMode::Default,
                    "accept-edits" | "accept_edits" => cc_core::config::PermissionMode::AcceptEdits,
                    "bypass-permissions" | "bypass_permissions" => {
                        cc_core::config::PermissionMode::BypassPermissions
                    }
                    "plan" => cc_core::config::PermissionMode::Plan,
                    _ => {
                        return CommandResult::Error(
                            "Mode must be: default, accept-edits, bypass-permissions, or plan"
                                .to_string(),
                        )
                    }
                };
                let mut new_config = ctx.config.clone();
                new_config.permission_mode = mode.clone();
                if let Err(e) = save_settings_mutation(|s| s.config.permission_mode = mode.clone())
                {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!("Permission mode set to {:?}.", mode),
                )
            }
            "allow" => {
                if arg.is_empty() {
                    return CommandResult::Error("Usage: /permissions allow <tool>".to_string());
                }
                let tool = arg.to_string();
                let mut new_config = ctx.config.clone();
                if !new_config.allowed_tools.contains(&tool) {
                    new_config.allowed_tools.push(tool.clone());
                }
                new_config.disallowed_tools.retain(|t| t != &tool);
                if let Err(e) = save_settings_mutation(|s| {
                    if !s.config.allowed_tools.contains(&tool) {
                        s.config.allowed_tools.push(tool.clone());
                    }
                    s.config.disallowed_tools.retain(|t| t != &tool);
                }) {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                CommandResult::ConfigChangeMessage(new_config, format!("Allowed tool: {}", tool))
            }
            "deny" => {
                if arg.is_empty() {
                    return CommandResult::Error("Usage: /permissions deny <tool>".to_string());
                }
                let tool = arg.to_string();
                let mut new_config = ctx.config.clone();
                if !new_config.disallowed_tools.contains(&tool) {
                    new_config.disallowed_tools.push(tool.clone());
                }
                new_config.allowed_tools.retain(|t| t != &tool);
                if let Err(e) = save_settings_mutation(|s| {
                    if !s.config.disallowed_tools.contains(&tool) {
                        s.config.disallowed_tools.push(tool.clone());
                    }
                    s.config.allowed_tools.retain(|t| t != &tool);
                }) {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                CommandResult::ConfigChangeMessage(new_config, format!("Denied tool: {}", tool))
            }
            "reset" => {
                let mut new_config = ctx.config.clone();
                new_config.allowed_tools.clear();
                new_config.disallowed_tools.clear();
                new_config.permission_mode = cc_core::config::PermissionMode::Default;
                if let Err(e) = save_settings_mutation(|s| {
                    s.config.allowed_tools.clear();
                    s.config.disallowed_tools.clear();
                    s.config.permission_mode = cc_core::config::PermissionMode::Default;
                }) {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                CommandResult::ConfigChangeMessage(
                    new_config,
                    "Permissions reset to defaults.".to_string(),
                )
            }
            other => CommandResult::Error(format!(
                "Unknown subcommand '{}'. Use: /permissions [set|allow|deny|reset]",
                other
            )),
        }
    }
}

// ---- /plan ---------------------------------------------------------------

#[async_trait]
impl SlashCommand for PlanCommand {
    fn name(&self) -> &str {
        "plan"
    }
    fn description(&self) -> &str {
        "Enter plan mode – model outputs a plan for approval before acting"
    }
    fn help(&self) -> &str {
        "Usage: /plan [description]\n\n\
         Switches to plan mode where the model will create a detailed plan before executing.\n\
         The plan must be approved before any file writes or command executions are performed.\n\
         Use /plan exit to leave plan mode."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        if args.trim() == "exit" {
            return CommandResult::UserMessage(
                "[Exiting plan mode. Resuming normal execution.]".to_string(),
            );
        }
        let task_desc = if args.is_empty() {
            "the current task".to_string()
        } else {
            args.to_string()
        };
        CommandResult::UserMessage(format!(
            "[Entering plan mode for: {}]\n\
             Please create a detailed step-by-step plan. Do not execute any commands or \
             write any files until the plan has been reviewed and approved.",
            task_desc
        ))
    }
}

// ---- /tasks --------------------------------------------------------------

#[async_trait]
impl SlashCommand for TasksCommand {
    fn name(&self) -> &str {
        "tasks"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["bashes"]
    }
    fn description(&self) -> &str {
        "List and manage background tasks"
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        CommandResult::UserMessage(
            "Please list all current tasks using the TaskList tool and show their status."
                .to_string(),
        )
    }
}

// ---- /session ------------------------------------------------------------

#[async_trait]
impl SlashCommand for SessionCommand {
    fn name(&self) -> &str {
        "session"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["remote"]
    }
    fn description(&self) -> &str {
        "Show or manage conversation sessions"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        match args.trim() {
            "list" => {
                let sessions = cc_core::history::list_sessions().await;
                if sessions.is_empty() {
                    CommandResult::Message("No saved sessions found.".to_string())
                } else {
                    let mut output = String::from("Recent sessions:\n\n");
                    for sess in sessions.iter().take(10) {
                        let updated = sess.updated_at.format("%Y-%m-%d %H:%M").to_string();
                        let id_short = &sess.id[..sess.id.len().min(8)];
                        output.push_str(&format!(
                            "  {} | {} | {} messages | {}\n",
                            id_short,
                            updated,
                            sess.messages.len(),
                            sess.title.as_deref().unwrap_or("(untitled)")
                        ));
                    }
                    output.push_str("\nUse /resume <id> to resume a session.");
                    CommandResult::Message(output)
                }
            }
            "" => {
                // If a bridge remote URL is active, show it prominently.
                if let Some(ref url) = ctx.remote_session_url {
                    let border = "─".repeat(url.len().min(60) + 4);
                    let display_url = if url.len() > 60 {
                        format!("{}…", &url[..60])
                    } else {
                        url.clone()
                    };
                    CommandResult::Message(format!(
                        "Remote session active\n\
                         ┌{border}┐\n\
                         │  {display_url}  │\n\
                         └{border}┘\n\n\
                         Open the URL above on any device to connect remotely.\n\
                         Session ID: {}",
                        ctx.session_id,
                    ))
                } else {
                    // Show current session info + recent sessions list.
                    let sessions = cc_core::history::list_sessions().await;
                    let mut output = format!(
                        "Current session\n\
                         ───────────────\n\
                         ID:       {}\n\
                         Title:    {}\n\
                         Messages: {}\n\
                         Model:    {}\n",
                        ctx.session_id,
                        ctx.session_title.as_deref().unwrap_or("(untitled)"),
                        ctx.messages.len(),
                        ctx.config.effective_model()
                    );

                    if !sessions.is_empty() {
                        output.push_str("\nRecent sessions:\n\n");
                        for sess in sessions.iter().take(5) {
                            let updated = sess.updated_at.format("%Y-%m-%d %H:%M").to_string();
                            let id_short = &sess.id[..sess.id.len().min(8)];
                            let marker = if sess.id == ctx.session_id {
                                " ◀ current"
                            } else {
                                ""
                            };
                            output.push_str(&format!(
                                "  {} | {} | {} messages | {}{}\n",
                                id_short,
                                updated,
                                sess.messages.len(),
                                sess.title.as_deref().unwrap_or("(untitled)"),
                                marker,
                            ));
                        }
                        output.push_str(
                            "\nUse /session list for all sessions, /resume <id> to switch.",
                        );
                    }

                    CommandResult::Message(output)
                }
            }
            _ => CommandResult::Error(format!(
                "Unknown subcommand: {}\n\nUsage: /session [list]",
                args
            )),
        }
    }
}

// ---- /thinking -----------------------------------------------------------

#[async_trait]
impl SlashCommand for ThinkingCommand {
    fn name(&self) -> &str {
        "thinking"
    }
    fn description(&self) -> &str {
        "Toggle extended thinking mode"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["think"]
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        // Extended thinking is configured through the model; just inform the user
        CommandResult::Message(
            "Extended thinking is available with your current model.\n\
             You can request thinking by asking to 'think step by step' or \
             'think carefully before answering'."
                .to_string(),
        )
    }
}

// ---- /export -------------------------------------------------------------

#[async_trait]
impl SlashCommand for ExportCommand {
    fn name(&self) -> &str {
        "export"
    }
    fn description(&self) -> &str {
        "Export conversation to a file"
    }
    fn help(&self) -> &str {
        "Usage: /export [filename]\n\
         Export the current conversation as JSON. Defaults to claude_export_<timestamp>.json."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let filename = if args.trim().is_empty() {
            format!(
                "claude_export_{}.json",
                chrono::Utc::now().format("%Y%m%d_%H%M%S")
            )
        } else {
            args.trim().to_string()
        };

        let path = ctx.working_dir.join(&filename);
        let export = serde_json::json!({
            "exported_at": chrono::Utc::now().to_rfc3339(),
            "model": ctx.config.effective_model(),
            "message_count": ctx.messages.len(),
            "messages": ctx.messages.iter().map(|m| serde_json::json!({
                "role": m.role,
                "content": m.get_all_text(),
            })).collect::<Vec<_>>(),
        });

        let json = match serde_json::to_string_pretty(&export) {
            Ok(j) => j,
            Err(e) => return CommandResult::Error(format!("Failed to serialize: {}", e)),
        };

        match std::fs::write(&path, &json) {
            Ok(_) => CommandResult::Message(format!(
                "Conversation exported to {}\n({} messages)",
                path.display(),
                ctx.messages.len()
            )),
            Err(e) => CommandResult::Error(format!("Failed to write {}: {}", filename, e)),
        }
    }
}

// ---- /skills -------------------------------------------------------------

#[async_trait]
impl SlashCommand for SkillsCommand {
    fn name(&self) -> &str {
        "skills"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["skill"]
    }
    fn description(&self) -> &str {
        "List available skills in .uppli/commands/"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let mut found: Vec<String> = Vec::new();
        let dirs = [
            ctx.working_dir.join(".uppli").join("commands"),
            dirs::home_dir()
                .unwrap_or_default()
                .join(".uppli")
                .join("commands"),
        ];

        for dir in &dirs {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "md") {
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            let name = stem.to_string();
                            if !found.contains(&name) {
                                found.push(name);
                            }
                        }
                    }
                }
            }
        }

        // Include skills contributed by installed plugins.
        if let Some(registry) = cc_plugins::global_plugin_registry() {
            for skill_dir in registry.all_skill_paths() {
                if let Ok(entries) = std::fs::read_dir(&skill_dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        // Skills can be individual .md files or subdirs with SKILL.md.
                        if p.is_dir() {
                            if p.join("SKILL.md").exists() || p.join("skill.md").exists() {
                                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                                    let skill_name = name.to_string();
                                    if !found.contains(&skill_name) {
                                        found.push(skill_name);
                                    }
                                }
                            }
                        } else if p.extension().is_some_and(|e| e == "md") {
                            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                                let name = stem.to_string();
                                if !found.contains(&name) {
                                    found.push(name);
                                }
                            }
                        }
                    }
                }
            }
        }

        if found.is_empty() {
            return CommandResult::Message(
                "No skills found.\nCreate .md files in .uppli/commands/ to define skills.\n\
                 Example: .uppli/commands/review.md"
                    .to_string(),
            );
        }

        found.sort();
        CommandResult::Message(format!(
            "Available skills ({}):\n{}",
            found.len(),
            found
                .iter()
                .map(|s| format!("  /{}", s))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

// ---- /rewind -------------------------------------------------------------

#[async_trait]
impl SlashCommand for RewindCommand {
    fn name(&self) -> &str {
        "rewind"
    }
    fn description(&self) -> &str {
        "Interactively select a message to rewind to"
    }
    fn help(&self) -> &str {
        "Usage: /rewind\n\
         Opens an interactive overlay to select the message to rewind to.\n\
         Use ↑↓ to navigate, Enter to select, y/n to confirm."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        if ctx.messages.is_empty() {
            return CommandResult::Message(
                "Nothing to rewind — conversation is empty.".to_string(),
            );
        }
        CommandResult::OpenRewindOverlay
    }
}

// ---- /stats --------------------------------------------------------------

#[async_trait]
impl SlashCommand for StatsCommand {
    fn name(&self) -> &str {
        "stats"
    }
    fn description(&self) -> &str {
        "Show token usage and cost statistics"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let input = ctx.cost_tracker.input_tokens();
        let output = ctx.cost_tracker.output_tokens();
        let turns = ctx.messages.len();
        let model = ctx.config.effective_model();

        CommandResult::Message(format!(
            "Session statistics\n\
             ──────────────────\n\
             Model:          {}\n\
             Messages:       {}\n\
             Input tokens:   {}\n\
             Output tokens:  {}\n\
             Total tokens:   {}",
            model,
            turns,
            input,
            output,
            input + output,
        ))
    }
}

// ---- /files --------------------------------------------------------------

#[async_trait]
impl SlashCommand for FilesCommand {
    fn name(&self) -> &str {
        "files"
    }
    fn description(&self) -> &str {
        "List files referenced in the current conversation"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        use std::collections::HashSet;
        // Scan message content for file paths (simple heuristic)
        let mut files: HashSet<String> = HashSet::new();
        let path_re =
            regex::Regex::new(r#"(?m)([A-Za-z]:[\\/][^\s,;:"'<>]+|/[^\s,;:"'<>]{3,})"#).ok();

        for msg in &ctx.messages {
            let text = msg.get_all_text();
            if let Some(ref re) = path_re {
                for cap in re.captures_iter(&text) {
                    let path = cap[1].trim().to_string();
                    if std::path::Path::new(&path).exists() {
                        files.insert(path);
                    }
                }
            }
        }

        if files.is_empty() {
            return CommandResult::Message(
                "No referenced files detected in the conversation.".to_string(),
            );
        }

        let mut sorted: Vec<String> = files.into_iter().collect();
        sorted.sort();

        CommandResult::Message(format!(
            "Referenced files ({}):\n{}",
            sorted.len(),
            sorted
                .iter()
                .map(|f| format!("  {}", f))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

// ---- /rename -------------------------------------------------------------

#[async_trait]
impl SlashCommand for RenameCommand {
    fn name(&self) -> &str {
        "rename"
    }
    fn description(&self) -> &str {
        "Rename the current session"
    }
    fn help(&self) -> &str {
        "Usage: /rename <new name>"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let name = args.trim();
        if name.is_empty() {
            return CommandResult::Error("Usage: /rename <new name>".to_string());
        }

        CommandResult::RenameSession(name.to_string())
    }
}

// ---- /effort -------------------------------------------------------------

#[async_trait]
impl SlashCommand for EffortCommand {
    fn name(&self) -> &str {
        "effort"
    }
    fn description(&self) -> &str {
        "Set the model's thinking effort (low | normal | high)"
    }
    fn help(&self) -> &str {
        "Usage: /effort [low|normal|high]\n\
         Sets how much computation the model uses for reasoning.\n\
         'high' enables extended thinking with a larger budget."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        match args.trim() {
            "" => CommandResult::Message(
                "Current effort: normal\nUse /effort [low|normal|high] to change.".to_string(),
            ),
            "low" => {
                // Low effort: smaller max_tokens
                ctx.config.max_tokens = Some(4096);
                CommandResult::ConfigChange(ctx.config.clone())
            }
            "normal" => {
                ctx.config.max_tokens = None; // use default
                CommandResult::ConfigChange(ctx.config.clone())
            }
            "high" => {
                ctx.config.max_tokens = Some(32768);
                CommandResult::ConfigChange(ctx.config.clone())
            }
            other => CommandResult::Error(format!(
                "Unknown effort level '{}'. Use: low | normal | high",
                other
            )),
        }
    }
}

// ---- /summary ------------------------------------------------------------

#[async_trait]
impl SlashCommand for SummaryCommand {
    fn name(&self) -> &str {
        "summary"
    }
    fn description(&self) -> &str {
        "Generate a brief summary of the conversation so far"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let count = ctx.messages.len();
        if count == 0 {
            return CommandResult::Message("No messages in conversation yet.".to_string());
        }

        // Ask the model to summarize by injecting a hidden user message
        CommandResult::UserMessage(
            "Please provide a brief (3-5 sentence) summary of our conversation so far, \
             focusing on what has been accomplished and the current state."
                .to_string(),
        )
    }
}

// ---- /commit -------------------------------------------------------------

#[async_trait]
impl SlashCommand for CommitCommand {
    fn name(&self) -> &str {
        "commit"
    }
    fn description(&self) -> &str {
        "Ask Claude to commit staged changes"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let extra = if args.trim().is_empty() {
            String::new()
        } else {
            format!(" with message: {}", args.trim())
        };

        CommandResult::UserMessage(format!(
            "Please commit the currently staged git changes{}. \
             Run `git diff --cached` to see what's staged, \
             write an appropriate commit message following the repository's conventions, \
             and run `git commit`.",
            extra
        ))
    }
}

// ---------------------------------------------------------------------------
// UI settings helpers (stored in ~/.uppli/ui-settings.json)
// These hold things not present in the core Config struct.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct UiSettings {
    #[serde(default)]
    pub editor_mode: Option<String>, // "vim" or "normal"
    #[serde(default)]
    pub fast_mode: Option<bool>,
    #[serde(default)]
    pub voice_enabled: Option<bool>,
    #[serde(default)]
    pub statusline_show_cost: Option<bool>,
    #[serde(default)]
    pub statusline_show_tokens: Option<bool>,
    #[serde(default)]
    pub statusline_show_model: Option<bool>,
    #[serde(default)]
    pub statusline_show_time: Option<bool>,
    #[serde(default)]
    pub prompt_color: Option<String>,
    #[serde(default)]
    pub sandbox_mode: Option<bool>,
}

fn ui_settings_path() -> std::path::PathBuf {
    cc_core::config::Settings::config_dir().join("ui-settings.json")
}

fn load_ui_settings() -> UiSettings {
    let path = ui_settings_path();
    if !path.exists() {
        return UiSettings::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_ui_settings(settings: &UiSettings) -> anyhow::Result<()> {
    let path = ui_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, json)?;
    Ok(())
}

fn mutate_ui_settings<F>(f: F) -> anyhow::Result<UiSettings>
where
    F: FnOnce(&mut UiSettings),
{
    let mut s = load_ui_settings();
    f(&mut s);
    save_ui_settings(&s)?;
    Ok(s)
}

// ---- /remote-control (/rc) -----------------------------------------------

#[async_trait]
impl SlashCommand for RemoteControlCommand {
    fn name(&self) -> &str {
        "remote-control"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["rc"]
    }
    fn description(&self) -> &str {
        "Show or manage the remote control (Bridge) connection"
    }
    fn help(&self) -> &str {
        "Usage: /remote-control [start|stop|status]\n\n\
         The Bridge feature lets you connect your local Uppli Code CLI to the\n\
         Uppli web UI or mobile app.\n\n\
         Subcommands:\n\
         /remote-control          Show current bridge status and connection URL\n\
         /remote-control start    Start the remote-control bridge listener\n\
         /remote-control stop     Stop the bridge listener\n\
         /remote-control status   Show bridge status"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let settings = match cc_core::config::Settings::load().await {
            Ok(s) => s,
            Err(e) => return CommandResult::Error(format!("Failed to load settings: {}", e)),
        };

        let remote_at_startup = settings.remote_control_at_startup;

        match args.trim() {
            "" | "status" => {
                let hostname = hostname::get()
                    .map(|h| h.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "(unknown host)".to_string());

                let bridge_url = std::env::var("CLAUDE_CODE_BRIDGE_URL")
                    .unwrap_or_else(|_| "https://uppli.dev".to_string());

                let token_status = if std::env::var("CLAUDE_CODE_BRIDGE_TOKEN").is_ok()
                    || std::env::var("CLAUDE_BRIDGE_OAUTH_TOKEN").is_ok()
                {
                    "configured via environment variable"
                } else {
                    "not set (required to connect)"
                };

                let startup_status = if remote_at_startup {
                    "enabled at startup"
                } else {
                    "disabled"
                };

                // Active session info from context
                let session_section = if let Some(ref url) = ctx.remote_session_url {
                    format!(
                        "\nActive Session\n\
                         ──────────────\n\
                         Session URL:  {url}\n\
                         Share this URL or QR code with others to let them connect\n\
                         to this Uppli Code session from the Uppli web UI.\n",
                        url = url
                    )
                } else {
                    "\nNo active bridge session in this process.\n".to_string()
                };

                // Device fingerprint (first 12 chars are enough for display)
                let fingerprint = cc_bridge::device_fingerprint();
                let fp_short = &fingerprint[..fingerprint.len().min(12)];

                CommandResult::Message(format!(
                    "Remote Control (Bridge)\n\
                     ═══════════════════════\n\
                     What it does: lets you connect the Uppli web UI or mobile app\n\
                     to this running Uppli Code CLI session on your local machine.\n\
                     All prompts and responses are relayed bidirectionally.\n\
                     \n\
                     Local Machine\n\
                     ─────────────\n\
                     Hostname:     {hostname}\n\
                     Device ID:    {fp_short}… (SHA-256 fingerprint)\n\
                     \n\
                     Bridge Configuration\n\
                     ────────────────────\n\
                     Bridge server:   {bridge_url}\n\
                     Session token:   {token_status}\n\
                     Startup mode:    {startup_status}\n\
                     {session_section}\n\
                     How to connect\n\
                     ──────────────\n\
                     1. Obtain a session token from uppli.dev (Settings → Remote Control)\n\
                     2. Set it:  export CLAUDE_CODE_BRIDGE_TOKEN=<your-token>\n\
                     3. Enable:  /remote-control start\n\
                     4. Restart Uppli Code — the bridge will connect automatically\n\
                     5. Open {bridge_url}/claude-code in your browser\n\
                     \n\
                     Note: Full bridge polling requires server-side session infrastructure.\n\
                     The cc-bridge crate implements the complete protocol (register → poll\n\
                     → events) and is ready to use once a valid session token is provided.\n\
                     \n\
                     Use /remote-control start   to enable bridge at next startup\n\
                     Use /remote-control stop    to disable bridge at startup",
                    hostname = hostname,
                    fp_short = fp_short,
                    bridge_url = bridge_url,
                    token_status = token_status,
                    startup_status = startup_status,
                    session_section = session_section,
                ))
            }
            "start" => {
                if let Err(e) = save_settings_mutation(|s| s.remote_control_at_startup = true) {
                    return CommandResult::Error(format!("Failed to save settings: {}", e));
                }
                let bridge_url = std::env::var("CLAUDE_CODE_BRIDGE_URL")
                    .unwrap_or_else(|_| "https://uppli.dev".to_string());
                let token_note = if std::env::var("CLAUDE_CODE_BRIDGE_TOKEN").is_ok()
                    || std::env::var("CLAUDE_BRIDGE_OAUTH_TOKEN").is_ok()
                {
                    "Session token detected in environment — bridge will connect on next start."
                        .to_string()
                } else {
                    format!(
                        "No session token found.\n\
                         Get a token from {bridge_url} (Settings → Remote Control)\n\
                         then run:  export CLAUDE_CODE_BRIDGE_TOKEN=<token>",
                        bridge_url = bridge_url
                    )
                };
                CommandResult::Message(format!(
                    "Remote control bridge enabled at startup.\n\
                     Restart Uppli Code to activate the bridge connection.\n\n\
                     {token_note}",
                    token_note = token_note
                ))
            }
            "stop" => {
                if let Err(e) = save_settings_mutation(|s| s.remote_control_at_startup = false) {
                    return CommandResult::Error(format!("Failed to save settings: {}", e));
                }
                CommandResult::Message(
                    "Remote control bridge disabled.\n\
                     The bridge will not start on next launch."
                        .to_string(),
                )
            }
            other => CommandResult::Error(format!(
                "Unknown subcommand: '{}'\nUsage: /remote-control [start|stop|status]",
                other
            )),
        }
    }
}

// ---- /remote-env ---------------------------------------------------------

#[async_trait]
impl SlashCommand for RemoteEnvCommand {
    fn name(&self) -> &str {
        "remote-env"
    }
    fn description(&self) -> &str {
        "Show and manage environment variables for remote sessions"
    }
    fn help(&self) -> &str {
        "Usage: /remote-env [set <KEY> <VALUE> | unset <KEY> | list]\n\n\
         Manages env vars stored in config that are forwarded to remote Uppli Code sessions.\n\
         These are persisted to settings under the 'env' key."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let args = args.trim();

        if args.is_empty() || args == "list" {
            if ctx.config.env.is_empty() {
                return CommandResult::Message(
                    "No remote environment variables configured.\n\
                     Use /remote-env set <KEY> <VALUE> to add one."
                        .to_string(),
                );
            }
            let mut lines = vec!["Remote environment variables:".to_string()];
            let mut keys: Vec<_> = ctx.config.env.keys().collect();
            keys.sort();
            for key in keys {
                let val = &ctx.config.env[key];
                // Mask values that look like secrets
                let display = if key.to_uppercase().contains("KEY")
                    || key.to_uppercase().contains("TOKEN")
                    || key.to_uppercase().contains("SECRET")
                    || key.to_uppercase().contains("PASSWORD")
                {
                    format!("{}***", &val[..val.len().min(4)])
                } else {
                    val.clone()
                };
                lines.push(format!("  {} = {}", key, display));
            }
            return CommandResult::Message(lines.join("\n"));
        }

        let mut parts = args.splitn(3, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let key = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();

        match sub {
            "set" => {
                if key.is_empty() || val.is_empty() {
                    return CommandResult::Error(
                        "Usage: /remote-env set <KEY> <VALUE>".to_string(),
                    );
                }
                let key_owned = key.to_string();
                let val_owned = val.to_string();
                if let Err(e) = save_settings_mutation(|s| {
                    s.config.env.insert(key_owned.clone(), val_owned.clone());
                }) {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                let mut new_config = ctx.config.clone();
                new_config.env.insert(key.to_string(), val.to_string());
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!("Set remote env: {} = {}", key, val),
                )
            }
            "unset" | "remove" | "delete" => {
                if key.is_empty() {
                    return CommandResult::Error("Usage: /remote-env unset <KEY>".to_string());
                }
                if !ctx.config.env.contains_key(key) {
                    return CommandResult::Message(format!("Key '{}' is not set.", key));
                }
                let key_owned = key.to_string();
                if let Err(e) = save_settings_mutation(|s| {
                    s.config.env.remove(&key_owned);
                }) {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                let mut new_config = ctx.config.clone();
                new_config.env.remove(key);
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!("Removed remote env var: {}", key),
                )
            }
            other => CommandResult::Error(format!(
                "Unknown subcommand: '{}'\nUsage: /remote-env [list|set <K> <V>|unset <K>]",
                other
            )),
        }
    }
}

// ---- /context ------------------------------------------------------------

#[async_trait]
impl SlashCommand for ContextCommand {
    fn name(&self) -> &str {
        "context"
    }
    fn description(&self) -> &str {
        "Show context window usage (tokens used / available)"
    }
    fn help(&self) -> &str {
        "Usage: /context\n\n\
         Displays the current context window utilization:\n\
         - Estimated tokens consumed by current conversation\n\
         - Context window limit for the active model\n\
         - Percentage used"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let model = ctx.config.effective_model();

        // All current Claude models use a 200K context window.
        let context_window: u64 = 200_000;

        let used_tokens = ctx.cost_tracker.total_tokens();
        let pct = if context_window > 0 {
            (used_tokens as f64 / context_window as f64) * 100.0
        } else {
            0.0
        };

        let bar_width = 40usize;
        let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
        let bar: String = "█".repeat(filled) + &"░".repeat(bar_width.saturating_sub(filled));

        // Estimate approximate message tokens from the message list
        let msg_char_count: usize = ctx.messages.iter().map(|m| m.get_all_text().len()).sum();
        // Rough estimate: ~4 chars per token for message text
        let msg_token_estimate = msg_char_count / 4;

        CommandResult::Message(format!(
            "Context Window Usage\n\
             ────────────────────\n\
             Model:          {model}\n\
             Context window: {window:>10} tokens\n\
             API tokens used:{used:>10} tokens  ({pct:.1}%)\n\
             Est. msg size:  {msg:>10} tokens  (approx)\n\
             Messages:       {msgs:>10}\n\n\
             [{bar}] {pct:.1}%\n\n\
             Use /compact to reduce context usage.",
            model = model,
            window = context_window,
            used = used_tokens,
            pct = pct,
            msg = msg_token_estimate,
            msgs = ctx.messages.len(),
            bar = bar,
        ))
    }
}

// ---- /copy ---------------------------------------------------------------

#[async_trait]
impl SlashCommand for CopyCommand {
    fn name(&self) -> &str {
        "copy"
    }
    fn description(&self) -> &str {
        "Copy the last assistant response to the clipboard"
    }
    fn help(&self) -> &str {
        "Usage: /copy [n]\n\n\
         Copies the most recent assistant response to the system clipboard.\n\
         Optionally pass a number to copy the Nth most-recent response."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let n: usize = args.trim().parse().unwrap_or(1).max(1);

        // Find the Nth most recent assistant message
        let assistant_msgs: Vec<&cc_core::types::Message> = ctx
            .messages
            .iter()
            .rev()
            .filter(|m| m.role == cc_core::types::Role::Assistant)
            .take(n)
            .collect();

        let msg = match assistant_msgs.last() {
            Some(m) => m,
            None => {
                return CommandResult::Message(
                    "No assistant messages found in conversation.".to_string(),
                )
            }
        };

        let text = msg.get_all_text();
        if text.is_empty() {
            return CommandResult::Message("Last assistant message is empty.".to_string());
        }

        // Try system clipboard via arboard
        #[cfg(not(target_os = "linux"))]
        {
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.clone())) {
                Ok(()) => {
                    let preview: String = text.chars().take(80).collect();
                    let ellipsis = if text.len() > 80 { "…" } else { "" };
                    return CommandResult::Message(format!(
                        "Copied {} chars to clipboard.\nPreview: {}{}",
                        text.len(),
                        preview,
                        ellipsis
                    ));
                }
                Err(e) => {
                    tracing::warn!("Clipboard write failed: {}", e);
                    // Fall through to file fallback
                }
            }
        }

        // Fallback: write to a temp file and inform the user
        let tmp_path = std::env::temp_dir().join("claude_copy.md");
        match std::fs::write(&tmp_path, &text) {
            Ok(()) => {
                let preview: String = text.chars().take(80).collect();
                let ellipsis = if text.len() > 80 { "…" } else { "" };
                CommandResult::Message(format!(
                    "Clipboard not available; saved {} chars to {}\nPreview: {}{}",
                    text.len(),
                    tmp_path.display(),
                    preview,
                    ellipsis
                ))
            }
            Err(e) => CommandResult::Error(format!("Failed to copy: {}", e)),
        }
    }
}

// ---- /chrome -------------------------------------------------------------

#[async_trait]
impl SlashCommand for ChromeCommand {
    fn name(&self) -> &str {
        "chrome"
    }
    fn description(&self) -> &str {
        "Chrome DevTools integration — connect Claude to a browser tab"
    }
    fn help(&self) -> &str {
        "Usage: /chrome [url]\n\n\
         Integrates Uppli Code with Google Chrome via the Chrome DevTools Protocol (CDP).\n\n\
         To use:\n\
         1. Launch Chrome with remote debugging:\n\
            chrome --remote-debugging-port=9222\n\
         2. Run /chrome to connect\n\
         3. Claude can then read the DOM, console logs, network requests, etc.\n\n\
         Optional: /chrome <url>  — navigate to a URL after connecting"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let cdp_url = "http://localhost:9222";

        // Try to reach the Chrome debugging endpoint
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap_or_default();

        let chrome_available = client
            .get(format!("{}/json/version", cdp_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if chrome_available {
            let navigate_msg = if !args.trim().is_empty() {
                format!("\n\nNavigating to: {}", args.trim())
            } else {
                String::new()
            };
            CommandResult::Message(format!(
                "Chrome DevTools connected at {cdp_url}{nav}\n\n\
                 Claude can now access the browser context. Try asking:\n\
                 - 'What's on the current page?'\n\
                 - 'Check the browser console for errors'\n\
                 - 'Describe the page structure'",
                cdp_url = cdp_url,
                nav = navigate_msg,
            ))
        } else {
            CommandResult::Message(format!(
                "Chrome DevTools not found at {cdp_url}\n\n\
                 To enable Chrome integration:\n\
                 1. Close Chrome completely\n\
                 2. Relaunch with debugging enabled:\n\n\
                    macOS:   /Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome \\\n\
                             --remote-debugging-port=9222 --no-first-run\n\
                    Windows: chrome.exe --remote-debugging-port=9222 --no-first-run\n\
                    Linux:   google-chrome --remote-debugging-port=9222 --no-first-run\n\n\
                 3. Then run /chrome again\n\n\
                 Note: Do not use your primary Chrome profile for security reasons.\n\
                 Docs: https://docs.uppli.dev/chrome-devtools",
                cdp_url = cdp_url,
            ))
        }
    }
}

// ---- /vim (/vi) ----------------------------------------------------------

#[async_trait]
impl SlashCommand for VimCommand {
    fn name(&self) -> &str {
        "vim"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["vi"]
    }
    fn description(&self) -> &str {
        "Toggle vim keybinding mode on/off"
    }
    fn help(&self) -> &str {
        "Usage: /vim [on|off]\n\n\
         Toggles vim keybinding mode in the REPL input.\n\
         When enabled, use Esc to switch between INSERT and NORMAL modes.\n\n\
         The setting is persisted to ~/.uppli/ui-settings.json."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let current = load_ui_settings();
        let current_mode = current.editor_mode.as_deref().unwrap_or("normal");

        let new_mode = match args.trim() {
            "on" | "vim" => "vim",
            "off" | "normal" => "normal",
            "" => {
                // Toggle
                if current_mode == "vim" {
                    "normal"
                } else {
                    "vim"
                }
            }
            other => {
                return CommandResult::Error(format!(
                    "Unknown argument '{}'. Use: /vim [on|off]",
                    other
                ))
            }
        };

        match mutate_ui_settings(|s| s.editor_mode = Some(new_mode.to_string())) {
            Ok(_) => CommandResult::Message(format!(
                "Editor mode set to {}.\n{}",
                new_mode,
                if new_mode == "vim" {
                    "Use Esc to switch between INSERT and NORMAL modes.\n\
                     Restart the REPL for the change to take effect."
                } else {
                    "Using standard (readline-style) keyboard bindings.\n\
                     Restart the REPL for the change to take effect."
                }
            )),
            Err(e) => CommandResult::Error(format!("Failed to save setting: {}", e)),
        }
    }
}

// ---- /voice --------------------------------------------------------------

#[async_trait]
impl SlashCommand for VoiceCommand {
    fn name(&self) -> &str {
        "voice"
    }
    fn description(&self) -> &str {
        "Toggle voice input mode on/off"
    }
    fn help(&self) -> &str {
        "Usage: /voice [on|off]\n\n\
         Enables or disables voice input (hold-to-talk).\n\
         Voice requires a Claude.ai subscription with the voice scope enabled.\n\
         Setting is persisted to ~/.uppli/ui-settings.json."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let current = load_ui_settings();
        let currently_enabled = current.voice_enabled.unwrap_or(false);

        let enable = match args.trim() {
            "on" | "enable" | "enabled" | "true" | "1" => true,
            "off" | "disable" | "disabled" | "false" | "0" => false,
            "" => !currently_enabled, // toggle
            other => {
                return CommandResult::Error(format!(
                    "Unknown argument '{}'. Use: /voice [on|off]",
                    other
                ))
            }
        };

        match mutate_ui_settings(|s| s.voice_enabled = Some(enable)) {
            Ok(_) => {
                if enable {
                    CommandResult::Message(
                        "Voice recording activated (Alt+V to toggle).\n\
                         Hold the configured hold-to-talk key to record.\n\
                         Voice mode requires a Claude.ai account with voice scope."
                            .to_string(),
                    )
                } else {
                    CommandResult::Message(
                        "Voice recording deactivated (Alt+V to toggle).".to_string(),
                    )
                }
            }
            Err(e) => CommandResult::Error(format!("Failed to save voice setting: {}", e)),
        }
    }
}

// ---- /upgrade ------------------------------------------------------------

#[async_trait]
impl SlashCommand for UpgradeCommand {
    fn name(&self) -> &str {
        "upgrade"
    }
    fn description(&self) -> &str {
        "Check for updates and show upgrade options"
    }
    fn help(&self) -> &str {
        "Usage: /upgrade\n\n\
         Checks GitHub releases for the latest version of Uppli Code.\n\
         If a newer version is available, shows the upgrade command."
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let current = cc_core::constants::APP_VERSION;

        // Check GitHub releases API for latest version
        let client = reqwest::Client::builder()
            .user_agent(format!("claude-code-rust/{}", current))
            .timeout(std::time::Duration::from_secs(8))
            .build();

        let client = match client {
            Ok(c) => c,
            Err(e) => {
                return CommandResult::Message(format!(
                    "Current version: {current}\n\
                     Could not check for updates (HTTP client error: {e})\n\
                     Visit https://github.com/anthropics/claude-code/releases for updates."
                ))
            }
        };

        let resp = client
            .get("https://api.github.com/repos/anthropics/claude-code/releases/latest")
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let json: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);

                let tag = json
                    .get("tag_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .trim_start_matches('v');

                let url = json
                    .get("html_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("https://github.com/anthropics/claude-code/releases");

                if tag == current || tag == "unknown" {
                    CommandResult::Message(format!(
                        "Uppli Code v{current} — you are up to date.\n\
                         Release page: {url}"
                    ))
                } else {
                    CommandResult::Message(format!(
                        "Update available!\n\
                         Current version:  v{current}\n\
                         Latest version:   v{tag}\n\
                         Release page:     {url}\n\n\
                         To upgrade (npm):\n\
                           npm install -g @anthropic-ai/claude-code@latest\n\n\
                         To upgrade (cargo):\n\
                           cargo install claude-code --force"
                    ))
                }
            }
            Ok(r) => {
                let status = r.status();
                CommandResult::Message(format!(
                    "Current version: v{current}\n\
                     Could not check for updates (HTTP {status}).\n\
                     Visit https://github.com/anthropics/claude-code/releases for updates."
                ))
            }
            Err(e) => CommandResult::Message(format!(
                "Current version: v{current}\n\
                 Could not check for updates: {e}\n\
                 Visit https://github.com/anthropics/claude-code/releases for updates."
            )),
        }
    }
}

// ---- /release-notes ------------------------------------------------------

#[async_trait]
impl SlashCommand for ReleaseNotesCommand {
    fn name(&self) -> &str {
        "release-notes"
    }
    fn description(&self) -> &str {
        "Show release notes for the current version"
    }
    fn help(&self) -> &str {
        "Usage: /release-notes [version]\n\n\
         Fetches and displays release notes from GitHub.\n\
         Without an argument, shows notes for the current version."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let current = cc_core::constants::APP_VERSION;
        let version = args.trim();

        let tag = if version.is_empty() {
            format!("v{}", current)
        } else if version.starts_with('v') {
            version.to_string()
        } else {
            format!("v{}", version)
        };

        let client = reqwest::Client::builder()
            .user_agent(format!("claude-code-rust/{}", current))
            .timeout(std::time::Duration::from_secs(8))
            .build();

        let client = match client {
            Ok(c) => c,
            Err(_) => {
                return CommandResult::Message(format!(
                    "Uppli Code {tag} release notes:\n\
                     Visit https://github.com/anthropics/claude-code/releases/tag/{tag}"
                ))
            }
        };

        let url = format!(
            "https://api.github.com/repos/anthropics/claude-code/releases/tags/{}",
            tag
        );

        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => {
                let json: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);

                let body = json
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("No release notes found.");

                let published = json
                    .get("published_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown date");

                let html_url = json.get("html_url").and_then(|v| v.as_str()).unwrap_or("");

                CommandResult::Message(format!(
                    "Release Notes: Uppli Code {tag}\n\
                     Published: {published}\n\
                     URL: {html_url}\n\
                     ─────────────────────────────────\n\
                     {body}"
                ))
            }
            Ok(r) if r.status().as_u16() == 404 => CommandResult::Message(format!(
                "No release found for {tag}.\n\
                 View all releases: https://github.com/anthropics/claude-code/releases"
            )),
            Ok(r) => CommandResult::Message(format!(
                "Could not fetch release notes (HTTP {}).\n\
                 View at: https://github.com/anthropics/claude-code/releases/tag/{}",
                r.status(),
                tag
            )),
            Err(e) => CommandResult::Message(format!(
                "Could not fetch release notes: {e}\n\
                 View at: https://github.com/anthropics/claude-code/releases/tag/{tag}"
            )),
        }
    }
}

// ---- /rate-limit-options -------------------------------------------------

#[async_trait]
impl SlashCommand for RateLimitOptionsCommand {
    fn name(&self) -> &str {
        "rate-limit-options"
    }
    fn description(&self) -> &str {
        "Show rate limit tiers and current rate limit status"
    }
    fn help(&self) -> &str {
        "Usage: /rate-limit-options\n\n\
         Displays available rate limit tiers and the current tier for your account.\n\
         Rate limits depend on your Claude plan (Free, Pro, Max, API)."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        // Try to read from OAuth tokens file to get subscription/tier info
        let tier_info = match cc_core::oauth::OAuthTokens::load().await {
            Some(tokens) => {
                let sub_type = tokens.subscription_type.as_deref().unwrap_or("unknown");
                format!(
                    "Account type:    {}\n\
                     Scopes:          {}",
                    sub_type,
                    if tokens.scopes.is_empty() {
                        "none".to_string()
                    } else {
                        tokens.scopes.join(", ")
                    }
                )
            }
            None => {
                // Check for API key auth
                if ctx.config.resolve_api_key().is_some() {
                    "Account type:    API key (Console)\n\
                     Rate limit tier: Depends on your API plan tier"
                        .to_string()
                } else {
                    "Not logged in. Run /login to see your rate limit tier.".to_string()
                }
            }
        };

        CommandResult::Message(format!(
            "Rate Limit Status\n\
             ─────────────────\n\
             {tier_info}\n\n\
             Available tiers:\n\
             ┌─────────────────────────────────────────────────┐\n\
             │ Free          │ Limited daily usage             │\n\
             │ Pro           │ Higher limits, faster resets    │\n\
             │ Max (5x)      │ 5× Pro limits                   │\n\
             │ Max (20x)     │ 20× Pro limits (highest tier)   │\n\
             │ API / Console │ Usage-billed, no hard cap       │\n\
             └─────────────────────────────────────────────────┘\n\n\
             To upgrade: /upgrade\n\
             Manage billing: https://uppli.dev/settings/billing",
            tier_info = tier_info,
        ))
    }
}

// ---- /statusline ---------------------------------------------------------

#[async_trait]
impl SlashCommand for StatuslineCommand {
    fn name(&self) -> &str {
        "statusline"
    }
    fn description(&self) -> &str {
        "Configure what is shown in the status line"
    }
    fn help(&self) -> &str {
        "Usage: /statusline [show|hide] [cost|tokens|model|time|all]\n\n\
         Controls which items appear in the TUI status bar at the bottom.\n\
         Settings are persisted to ~/.uppli/ui-settings.json.\n\n\
         Examples:\n\
           /statusline               — show current configuration\n\
           /statusline show cost     — show cost in status line\n\
           /statusline hide tokens   — hide token count\n\
           /statusline show all      — show everything\n\
           /statusline hide all      — hide everything"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let args = args.trim();
        let current = load_ui_settings();

        if args.is_empty() {
            return CommandResult::Message(format!(
                "Status line configuration\n\
                 ─────────────────────────\n\
                 Show cost:   {cost}\n\
                 Show tokens: {tokens}\n\
                 Show model:  {model}\n\
                 Show time:   {time}\n\n\
                 Use /statusline [show|hide] [cost|tokens|model|time|all] to change.",
                cost = fmt_bool(current.statusline_show_cost.unwrap_or(true)),
                tokens = fmt_bool(current.statusline_show_tokens.unwrap_or(true)),
                model = fmt_bool(current.statusline_show_model.unwrap_or(true)),
                time = fmt_bool(current.statusline_show_time.unwrap_or(true)),
            ));
        }

        let mut parts = args.splitn(2, ' ');
        let verb = parts.next().unwrap_or("").trim();
        let item = parts.next().unwrap_or("").trim();

        let show = match verb {
            "show" | "enable" | "on" => true,
            "hide" | "disable" | "off" => false,
            _ => {
                return CommandResult::Error(
                    "Usage: /statusline [show|hide] [cost|tokens|model|time|all]".to_string(),
                )
            }
        };

        if item.is_empty() || item == "all" {
            match mutate_ui_settings(|s| {
                s.statusline_show_cost = Some(show);
                s.statusline_show_tokens = Some(show);
                s.statusline_show_model = Some(show);
                s.statusline_show_time = Some(show);
            }) {
                Ok(_) => {
                    return CommandResult::Message(format!(
                        "Status line: all items {}.",
                        if show { "shown" } else { "hidden" }
                    ))
                }
                Err(e) => return CommandResult::Error(format!("Failed to save: {}", e)),
            }
        }

        let result = match item {
            "cost" => mutate_ui_settings(|s| s.statusline_show_cost = Some(show)),
            "tokens" | "token" => mutate_ui_settings(|s| s.statusline_show_tokens = Some(show)),
            "model" => mutate_ui_settings(|s| s.statusline_show_model = Some(show)),
            "time" | "clock" => mutate_ui_settings(|s| s.statusline_show_time = Some(show)),
            other => {
                return CommandResult::Error(format!(
                    "Unknown item '{}'. Use: cost, tokens, model, time, or all.",
                    other
                ))
            }
        };

        match result {
            Ok(_) => CommandResult::Message(format!(
                "Status line: {} {}.",
                item,
                if show { "shown" } else { "hidden" }
            )),
            Err(e) => CommandResult::Error(format!("Failed to save: {}", e)),
        }
    }
}

fn fmt_bool(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}

// ---- /security-review ----------------------------------------------------

#[async_trait]
impl SlashCommand for SecurityReviewCommand {
    fn name(&self) -> &str {
        "security-review"
    }
    fn description(&self) -> &str {
        "Run a security review of the current project"
    }
    fn help(&self) -> &str {
        "Usage: /security-review [path]\n\n\
         Asks Claude to perform a security review of the codebase.\n\
         Analyzes for common vulnerabilities: injection attacks, auth issues,\n\
         secrets exposure, unsafe deserialization, path traversal, etc."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let target = if args.trim().is_empty() {
            ctx.working_dir.display().to_string()
        } else {
            args.trim().to_string()
        };

        CommandResult::UserMessage(format!(
            "Please perform a comprehensive security review of the code in `{target}`.\n\n\
             Focus on identifying:\n\
             1. Injection vulnerabilities (SQL, command, LDAP, XSS, SSTI)\n\
             2. Authentication and authorization flaws\n\
             3. Hardcoded secrets, API keys, or passwords\n\
             4. Insecure deserialization\n\
             5. Path traversal or file inclusion vulnerabilities\n\
             6. Cryptographic weaknesses (weak algorithms, bad IV usage, key reuse)\n\
             7. Dependency vulnerabilities (check for outdated packages)\n\
             8. Race conditions and TOCTOU issues\n\
             9. Information disclosure (verbose errors, debug endpoints)\n\
             10. Any OWASP Top 10 issues relevant to this codebase\n\n\
             For each finding, provide:\n\
             - Severity: Critical/High/Medium/Low/Informational\n\
             - File and line number\n\
             - Description of the vulnerability\n\
             - Proof of concept or reproduction steps\n\
             - Recommended remediation\n\n\
             Start by reading the main source files and any dependency manifests.",
            target = target,
        ))
    }
}

// ---- /terminal-setup -----------------------------------------------------

#[async_trait]
impl SlashCommand for TerminalSetupCommand {
    fn name(&self) -> &str {
        "terminal-setup"
    }
    fn description(&self) -> &str {
        "Help configure your terminal for optimal Uppli Code use"
    }
    fn help(&self) -> &str {
        "Usage: /terminal-setup\n\n\
         Diagnoses your terminal environment and gives recommendations for\n\
         optimal Uppli Code display (font, color support, Unicode, etc.)."
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let mut checks: Vec<String> = Vec::new();

        // Check TERM variable
        let term = std::env::var("TERM").unwrap_or_default();
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

        // Terminal identification
        let terminal_name = if !term_program.is_empty() {
            term_program.clone()
        } else {
            term.clone()
        };
        checks.push(format!("Terminal:      {}", terminal_name));

        // Color depth
        let color_depth = if colorterm == "truecolor" || colorterm == "24bit" {
            "24-bit true color (optimal)"
        } else if term.contains("256color") || colorterm == "256color" {
            "256 colors (good)"
        } else if !term.is_empty() {
            "Basic colors (limited)"
        } else {
            "Unknown"
        };
        checks.push(format!("Colors:        {}", color_depth));

        // Check if UNICODE is likely supported
        let lang = std::env::var("LANG").unwrap_or_default();
        let lc_all = std::env::var("LC_ALL").unwrap_or_default();
        let unicode_env =
            lang.to_lowercase().contains("utf") || lc_all.to_lowercase().contains("utf");
        checks.push(format!(
            "Unicode/UTF-8: {}",
            if unicode_env {
                "likely supported (LANG/LC_ALL contains UTF)"
            } else {
                "check LANG env var"
            }
        ));

        // Check for known good terminals
        let is_good_terminal = matches!(
            term_program.to_lowercase().as_str(),
            "iterm.app" | "iterm2" | "hyper" | "warp" | "alacritty" | "kitty" | "wezterm"
        ) || term_program.to_lowercase().contains("vscode")
            || term_program.to_lowercase().contains("terminal");

        checks.push(format!(
            "Terminal type: {}",
            if is_good_terminal {
                "well-known terminal (good)"
            } else {
                "verify settings below"
            }
        ));

        // Shell detection
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
        checks.push(format!("Shell:         {}", shell));

        // Check for Nerd Fonts (heuristic: environment variable set by some terminals)
        let nerd_font =
            std::env::var("NERD_FONT").is_ok() || std::env::var("TERM_NERD_FONT").is_ok();

        CommandResult::Message(format!(
            "Terminal Setup Diagnostic\n\
             ─────────────────────────\n\
             {checks}\n\n\
             Recommendations for optimal Uppli Code experience:\n\
             ─────────────────────────────────────────────────\n\
             1. Font: Use a Nerd Font for box-drawing characters and icons\n\
                {nerd_hint}\n\
                Download: https://www.nerdfonts.com/\n\
             2. Color: Enable 24-bit true color:\n\
                export COLORTERM=truecolor\n\
             3. Unicode: Ensure UTF-8 locale:\n\
                export LANG=en_US.UTF-8\n\
             4. Recommended terminals:\n\
                - WezTerm (all platforms)\n\
                - Alacritty (all platforms)\n\
                - Kitty (macOS/Linux)\n\
                - Windows Terminal (Windows)\n\
                - iTerm2 (macOS)\n\
             5. Set terminal to unlimited scrollback for long conversations",
            checks = checks.join("\n  "),
            nerd_hint = if nerd_font {
                "[ok] Nerd Font detected"
            } else {
                "[!] Nerd Font not detected — box-drawing may appear broken"
            },
        ))
    }
}

// ---- /extra-usage --------------------------------------------------------

#[async_trait]
impl SlashCommand for ExtraUsageCommand {
    fn name(&self) -> &str {
        "extra-usage"
    }
    fn description(&self) -> &str {
        "Show detailed usage statistics: calls, cache, tools"
    }
    fn help(&self) -> &str {
        "Usage: /extra-usage\n\n\
         Displays extended usage statistics beyond /cost:\n\
         - API call count\n\
         - Cache hit/miss ratio\n\
         - Token breakdown by type\n\
         - Effective cost per call"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let input = ctx.cost_tracker.input_tokens();
        let output = ctx.cost_tracker.output_tokens();
        let cache_creation = ctx.cost_tracker.cache_creation_tokens();
        let cache_read = ctx.cost_tracker.cache_read_tokens();
        let total = ctx.cost_tracker.total_tokens();
        // Estimate API calls from messages (each assistant message ~ 1 API call)
        let api_calls = ctx
            .messages
            .iter()
            .filter(|m| m.role == cc_core::types::Role::Assistant)
            .count();
        let api_calls = api_calls.max(1); // at least 1 if we have any data

        // Cache efficiency
        let cache_total = cache_creation + cache_read;
        let cache_hit_pct = if cache_total > 0 {
            (cache_read as f64 / cache_total as f64) * 100.0
        } else {
            0.0
        };

        let tokens_per_call = if api_calls > 0 {
            total / api_calls as u64
        } else {
            0
        };

        CommandResult::Message(format!(
            "Detailed Usage Statistics\n\
             ─────────────────────────\n\
             API calls:           {api_calls}\n\
             Avg tokens/call:     {tokens_per_call}\n\n\
             Token Breakdown:\n\
               Input tokens:      {input:>10}\n\
               Output tokens:     {output:>10}\n\
               Cache creation:    {cache_creation:>10}\n\
               Cache read:        {cache_read:>10}\n\
               Total tokens:      {total:>10}\n\n\
             Cache Performance:\n\
               Cache hit rate:    {cache_hit_pct:.1}%\n\
               Cache efficiency:  {cache_eff}\n\n\
             For cost in USD, see your provider dashboard.",
            api_calls = api_calls,
            tokens_per_call = tokens_per_call,
            input = input,
            output = output,
            cache_creation = cache_creation,
            cache_read = cache_read,
            total = total,
            cache_hit_pct = cache_hit_pct,
            cache_eff = if cache_hit_pct > 70.0 {
                "Excellent"
            } else if cache_hit_pct > 40.0 {
                "Good"
            } else if cache_total > 0 {
                "Low — prompts may not be stable enough to cache"
            } else {
                "No cache activity"
            },
        ))
    }
}

// ---- /advisor ------------------------------------------------------------

#[async_trait]
impl SlashCommand for AdvisorCommand {
    fn name(&self) -> &str {
        "advisor"
    }
    fn description(&self) -> &str {
        "Set or unset the server-side advisor model"
    }
    fn help(&self) -> &str {
        "Usage: /advisor [<model>|off|unset]\n\n\
         Sets the advisor model used for server-side suggestions.\n\
         Examples:\n\
           /advisor deepseek-reasoner  — set advisor model\n\
           /advisor off               — disable the advisor\n\
           /advisor                   — show current advisor setting"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let arg = args.trim();
        let settings_dir = cc_core::config::Settings::config_dir();
        let settings_path = settings_dir.join("settings.json");

        // Read or create settings JSON
        let mut settings_val: serde_json::Value = settings_path
            .exists()
            .then(|| std::fs::read_to_string(&settings_path).ok())
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        match arg {
            "" => {
                let current = settings_val
                    .get("advisorModel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(not set)");
                CommandResult::Message(format!("Advisor model: {current}"))
            }
            "off" | "unset" | "none" => {
                settings_val
                    .as_object_mut()
                    .map(|m| m.remove("advisorModel"));
                if let Ok(json) = serde_json::to_string_pretty(&settings_val) {
                    let _ = std::fs::write(&settings_path, json);
                }
                CommandResult::Message("Advisor model unset.".to_string())
            }
            model => {
                // Basic validation: must look like a model identifier
                if model.starts_with("claude-") || model.contains('/') {
                    settings_val["advisorModel"] = serde_json::Value::String(model.to_string());
                    if let Ok(json) = serde_json::to_string_pretty(&settings_val) {
                        let _ = std::fs::write(&settings_path, json);
                    }
                    CommandResult::Message(format!("Advisor model set to: {model}"))
                } else {
                    CommandResult::Message(format!(
                        "Unknown model '{model}'. Model IDs should start with 'claude-'.\n\
                         Use /model to see available models."
                    ))
                }
            }
        }
    }
}

// ---- /install-slack-app --------------------------------------------------

#[async_trait]
impl SlashCommand for InstallSlackAppCommand {
    fn name(&self) -> &str {
        "install-slack-app"
    }
    fn description(&self) -> &str {
        "Install the Uppli Code Slack integration"
    }
    fn help(&self) -> &str {
        "Usage: /install-slack-app\n\n\
         Opens instructions for installing the Uppli Code Slack app.\n\
         Requires a Claude for Enterprise subscription."
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        CommandResult::Message(
            "Uppli Code Slack Integration\n\
             ─────────────────────────────\n\
             To install Uppli Code in Slack:\n\n\
             1. Ensure you have a Claude for Enterprise subscription\n\
             2. Visit your Anthropic Console → Integrations → Slack\n\
             3. Click \"Add to Slack\" and authorize the app\n\
             4. Invite @Claude to any channel with: /invite @Claude\n\n\
             In Slack, you can then:\n\
             • Mention @Claude to ask questions in any channel\n\
             • Use /claude for direct commands\n\
             • Share code snippets for review\n\n\
             See: https://docs.uppli.dev/slack"
                .to_string(),
        )
    }
}

// ---- /fast (/speed) ------------------------------------------------------

#[async_trait]
impl SlashCommand for FastCommand {
    fn name(&self) -> &str {
        "fast"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["speed"]
    }
    fn description(&self) -> &str {
        "Toggle fast mode (uses a faster/cheaper model)"
    }
    fn help(&self) -> &str {
        "Usage: /fast [on|off]\n\n\
         Fast mode switches to a faster, more economical model variant\n\
         (claude-haiku) for quick responses. Toggle without argument to switch.\n\
         The setting is persisted to ~/.uppli/ui-settings.json."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let current = load_ui_settings();
        let currently_on = current.fast_mode.unwrap_or(false);

        let enable = match args.trim() {
            "on" | "enable" | "true" | "1" => true,
            "off" | "disable" | "false" | "0" => false,
            "" => !currently_on,
            other => {
                return CommandResult::Error(format!(
                    "Unknown argument '{}'. Use: /fast [on|off]",
                    other
                ))
            }
        };

        if let Err(e) = mutate_ui_settings(|s| s.fast_mode = Some(enable)) {
            return CommandResult::Error(format!("Failed to save setting: {}", e));
        }

        let fast_model = ""; // provider-driven
        let normal_model = ctx.config.model.as_deref().unwrap_or("unknown");

        if enable {
            let mut new_config = ctx.config.clone();
            new_config.model = Some(fast_model.to_string());
            CommandResult::ConfigChangeMessage(
                new_config,
                format!(
                    "Fast mode ON. Using {} for quicker, cheaper responses.\n\
                     Use /fast off to return to {}.",
                    fast_model, normal_model
                ),
            )
        } else {
            let mut new_config = ctx.config.clone();
            // Restore default / saved model
            new_config.model = None;
            CommandResult::ConfigChangeMessage(
                new_config,
                "Fast mode OFF. Restored to provider default model.".to_string(),
            )
        }
    }
}

// ---- /think-back ---------------------------------------------------------

#[async_trait]
impl SlashCommand for ThinkBackCommand {
    fn name(&self) -> &str {
        "think-back"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["thinkback"]
    }
    fn description(&self) -> &str {
        "Show thinking traces from previous responses in this session"
    }
    fn help(&self) -> &str {
        "Usage: /think-back [n]\n\n\
         Displays the thinking/reasoning traces from the most recent model responses.\n\
         Pass a number to show the Nth most recent thinking block."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let n: usize = args.trim().parse().unwrap_or(1).max(1);

        // Scan messages for thinking blocks
        let thinking_blocks: Vec<(usize, String)> = ctx
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == cc_core::types::Role::Assistant)
            .filter_map(|(idx, m)| {
                let blocks = m.get_thinking_blocks();
                if blocks.is_empty() {
                    return None;
                }
                let thinking: String = blocks
                    .iter()
                    .filter_map(|b| {
                        if let cc_core::types::ContentBlock::Thinking { thinking, .. } = b {
                            Some(thinking.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if thinking.is_empty() {
                    None
                } else {
                    Some((idx, thinking))
                }
            })
            .collect();

        if thinking_blocks.is_empty() {
            return CommandResult::Message(
                "No thinking traces found in this session.\n\
                 Thinking traces appear when the model uses extended thinking mode.\n\
                 Try asking Claude to 'think step by step' or 'think carefully'."
                    .to_string(),
            );
        }

        // Show the Nth most recent (1-indexed)
        let total = thinking_blocks.len();
        let target_idx = total.saturating_sub(n);
        let (msg_idx, trace) = &thinking_blocks[target_idx];

        CommandResult::Message(format!(
            "Thinking trace ({n} of {total} found, from message {msg}):\n\
             ─────────────────────────────────────\n\
             {trace}\n\
             ─────────────────────────────────────\n\
             Use /think-back <n> to see older traces.",
            n = n,
            total = total,
            msg = msg_idx + 1,
            trace = trace,
        ))
    }
}

// ---- /thinkback-play -----------------------------------------------------

#[async_trait]
impl SlashCommand for ThinkBackPlayCommand {
    fn name(&self) -> &str {
        "thinkback-play"
    }
    fn description(&self) -> &str {
        "Replay a thinking trace as an animated walkthrough"
    }
    fn help(&self) -> &str {
        "Usage: /thinkback-play [n]\n\n\
         Replays a previous thinking trace, formatted for easy reading.\n\
         Pass a number to replay the Nth most recent trace."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let n: usize = args.trim().parse().unwrap_or(1).max(1);

        let thinking_blocks: Vec<String> = ctx
            .messages
            .iter()
            .filter(|m| m.role == cc_core::types::Role::Assistant)
            .filter_map(|m| {
                let blocks = m.get_thinking_blocks();
                if blocks.is_empty() {
                    return None;
                }
                let t: String = blocks
                    .iter()
                    .filter_map(|b| {
                        if let cc_core::types::ContentBlock::Thinking { thinking, .. } = b {
                            Some(thinking.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            })
            .collect();

        if thinking_blocks.is_empty() {
            return CommandResult::Message(
                "No thinking traces to replay in this session.".to_string(),
            );
        }

        let total = thinking_blocks.len();
        let idx = total.saturating_sub(n);
        let trace = &thinking_blocks[idx];

        // Format the trace with step numbering
        let steps: Vec<&str> = trace.split('\n').filter(|l| !l.trim().is_empty()).collect();
        let mut formatted = format!(
            "Thinking Trace Replay ({}/{total})\n\
             ══════════════════════════════════\n",
            n,
            total = total
        );
        for (i, step) in steps.iter().enumerate() {
            formatted.push_str(&format!("  Step {}: {}\n", i + 1, step));
        }
        formatted.push_str("══════════════════════════════════\n");
        formatted.push_str(&format!(
            "{} steps shown. Use /think-back for raw traces.",
            steps.len()
        ));

        CommandResult::Message(formatted)
    }
}

// ---- /feedback (standalone, supplements BugCommand alias) ----------------

#[async_trait]
impl SlashCommand for FeedbackCommand {
    fn name(&self) -> &str {
        "report"
    }
    fn aliases(&self) -> Vec<&str> {
        vec![]
    }
    fn description(&self) -> &str {
        "Open the GitHub issues page to report a bug or request a feature"
    }
    fn hidden(&self) -> bool {
        true
    } // surfaced via BugCommand alias; hidden to avoid duplicate
    fn help(&self) -> &str {
        "Usage: /report [description]\n\n\
         Opens the GitHub issues tracker. If a description is provided,\n\
         it is shown as a suggested pre-fill for the issue body."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let url = "https://github.com/anthropics/claude-code/issues/new";
        let report = args.trim();
        let display_url = if report.is_empty() {
            url.to_string()
        } else {
            // Append as a body query param
            format!("{}?body={}", url, urlencoding::encode(report))
        };

        match open_with_system(&display_url) {
            Ok(_) => CommandResult::Message(format!("Opened issue tracker: {}", url)),
            Err(_) => CommandResult::Message(format!("Please visit {} to submit a report.", url)),
        }
    }
}

// ---- /color (full implementation) ----------------------------------------

#[async_trait]
impl SlashCommand for ColorSetCommand {
    fn name(&self) -> &str {
        "color-set"
    }
    fn hidden(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Internal: set prompt color — use /color instead"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let color = args.trim();
        if color.is_empty() {
            let current = load_ui_settings();
            return CommandResult::Message(format!(
                "Current prompt color: {}\n\
                 Use /color <name|#RRGGBB|default> to change it.\n\n\
                 Named colors: red, green, blue, yellow, cyan, magenta, white, orange, purple",
                current.prompt_color.as_deref().unwrap_or("default"),
            ));
        }

        let normalized = if color == "default" {
            None
        } else {
            // Validate hex or named color
            let known_colors = [
                "red", "green", "blue", "yellow", "cyan", "magenta", "white", "orange", "purple",
                "pink", "gray", "grey",
            ];
            let is_hex = color.starts_with('#')
                && (color.len() == 4 || color.len() == 7)
                && color[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !is_hex && !known_colors.contains(&color.to_lowercase().as_str()) {
                return CommandResult::Error(format!(
                    "Unknown color '{}'. Use a color name (red, green, …) or a hex code (#RGB or #RRGGBB).",
                    color
                ));
            }
            Some(color.to_string())
        };

        match mutate_ui_settings(|s| s.prompt_color = normalized.clone()) {
            Ok(_) => CommandResult::Message(format!(
                "Prompt color set to {}.\n\
                 Restart the REPL for the change to take effect.",
                normalized.as_deref().unwrap_or("default")
            )),
            Err(e) => CommandResult::Error(format!("Failed to save color: {}", e)),
        }
    }
}

// ---- /share --------------------------------------------------------------

#[async_trait]
impl SlashCommand for ShareCommand {
    fn name(&self) -> &str {
        "share"
    }
    fn description(&self) -> &str {
        "Create a shareable URL for the current session"
    }
    fn help(&self) -> &str {
        "Usage: /share\n\n\
         Attempts to create a public share link for the current conversation\n\
         by calling the Anthropic share API.\n\n\
         Requires authentication with Uppli OAuth. If you are not\n\
         authenticated, use /login first."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        // Resolve auth credential
        let auth = ctx.config.resolve_auth_async().await;

        let Some((credential, use_bearer)) = auth else {
            return CommandResult::Message(
                "Session sharing is available when authenticated with Uppli OAuth.\n\
                 Use /login to sign in."
                    .to_string(),
            );
        };

        // Build the request body: serialize the message list as JSON
        let messages_json = match serde_json::to_value(&ctx.messages) {
            Ok(v) => v,
            Err(e) => {
                return CommandResult::Error(format!("Failed to serialize session messages: {}", e))
            }
        };

        let body = serde_json::json!({
            "session_id": ctx.session_id,
            "title": ctx.session_title,
            "messages": messages_json,
        });

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => return CommandResult::Error(format!("Failed to build HTTP client: {}", e)),
        };

        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/v1".to_string());
        let url = format!("{}/api/claude_code/share_session", base_url);

        let req = if use_bearer {
            client.post(&url).bearer_auth(&credential)
        } else {
            client.post(&url).header("x-api-key", &credential)
        };

        let resp = req
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let json: serde_json::Value = match r.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        return CommandResult::Error(format!(
                            "Failed to parse share API response: {}",
                            e
                        ))
                    }
                };
                let share_url = json
                    .get("share_url")
                    .or_else(|| json.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                match share_url {
                    Some(u) => CommandResult::Message(format!(
                        "Session shared successfully!\nShare URL: {}",
                        u
                    )),
                    None => CommandResult::Error(
                        "Share API returned success but no URL was found in the response."
                            .to_string(),
                    ),
                }
            }
            Ok(r) => {
                let status = r.status();
                let body_text = r.text().await.unwrap_or_default();
                CommandResult::Error(format!(
                    "Share API returned error {}: {}",
                    status, body_text
                ))
            }
            Err(e) => CommandResult::Error(format!(
                "Failed to contact share API: {}\n\
                 Session sharing is available when authenticated with Uppli OAuth.",
                e
            )),
        }
    }
}

// ---- /teleport -----------------------------------------------------------

#[async_trait]
impl SlashCommand for TeleportCommand {
    fn name(&self) -> &str {
        "teleport"
    }
    fn description(&self) -> &str {
        "Teleport to a different session or branch point"
    }
    fn help(&self) -> &str {
        "Usage: /teleport\n\n\
         Teleports to a remote session when a bridge connection is active.\n\n\
         When connected to a remote session (via /remote-control), this command\n\
         jumps to the latest state of that remote session.\n\n\
         For local-only sessions: shows the current session ID and explains\n\
         that a bridge connection is required."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        if let Some(ref remote_url) = ctx.remote_session_url {
            CommandResult::Message(format!(
                "Teleporting to remote session...\n\
                 Remote URL: {}\n\
                 Session ID: {}\n\n\
                 Use your browser or the Uppli app to continue from this point.",
                remote_url, ctx.session_id
            ))
        } else {
            CommandResult::Message(format!(
                "Teleport requires an active remote session bridge.\n\
                 Use /session to view connection info.\n\n\
                 Current session ID: {}\n\
                 To enable bridge: /remote-control start",
                ctx.session_id
            ))
        }
    }
}

// ---- /btw ----------------------------------------------------------------

#[async_trait]
impl SlashCommand for BtwCommand {
    fn name(&self) -> &str {
        "btw"
    }
    fn description(&self) -> &str {
        "Ask a side question without adding it to conversation history"
    }
    fn help(&self) -> &str {
        "Usage: /btw <question>\n\n\
         Submits a background question to the model without it becoming part of\n\
         the main conversation context. The response is shown inline but not\n\
         stored in the message history.\n\n\
         Example:\n\
           /btw what is the capital of France?"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let question = args.trim();
        if question.is_empty() {
            return CommandResult::Error(
                "Usage: /btw <question>  — provide a question after /btw".to_string(),
            );
        }

        // Surface as a special user message tagged as a side-question so the
        // REPL/TUI can handle it as a non-history query. We inject a system tag
        // that tells the backend to answer but not record the exchange.
        CommandResult::UserMessage(format!(
            "[/btw side-question — answer inline, do not store in history]: {}",
            question
        ))
    }
}

// ---- /ctx-viz (context visualizer) ---------------------------------------

#[async_trait]
impl SlashCommand for CtxVizCommand {
    fn name(&self) -> &str {
        "ctx-viz"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["context-visualizer", "ctx"]
    }
    fn description(&self) -> &str {
        "Visualize context window usage breakdown by category"
    }
    fn help(&self) -> &str {
        "Usage: /ctx-viz\n\n\
         Shows a detailed breakdown of how the context window is being used:\n\
         - System prompt token estimate\n\
         - Conversation messages token estimate\n\
         - Tool results token estimate\n\
         - Total vs context window limit"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let model = ctx.config.effective_model().to_string();
        let context_window: u64 = 200_000; // all current Claude models

        // Estimate system prompt tokens: rough chars/4 approximation
        // Build a minimal system prompt to estimate its size.
        let sys_prompt_chars: usize = ctx
            .config
            .custom_system_prompt
            .as_deref()
            .map(|s| s.len())
            .unwrap_or(2400 * 4); // fallback: ~2400 tokens worth
        let sys_prompt_tokens = (sys_prompt_chars / 4).max(1) as u64;

        // Estimate conversation tokens from messages
        let (conv_chars, tool_chars): (usize, usize) =
            ctx.messages.iter().fold((0, 0), |(conv, tool), msg| {
                let text = msg.get_all_text();
                // Heuristic: if the message looks like a tool result, count separately
                if msg.role == cc_core::types::Role::User && text.starts_with('[') {
                    (conv, tool + text.len())
                } else {
                    (conv + text.len(), tool)
                }
            });

        let conv_tokens = (conv_chars / 4) as u64;
        let tool_tokens = (tool_chars / 4) as u64;
        let total_tokens = sys_prompt_tokens + conv_tokens + tool_tokens;
        let pct = (total_tokens as f64 / context_window as f64) * 100.0;

        let bar_width = 40usize;
        let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
        let bar = "█".repeat(filled) + &"░".repeat(bar_width.saturating_sub(filled));

        CommandResult::Message(format!(
            "Context Window Usage\n\
             ────────────────────────────────────────\n\
             Model:            {model}\n\
             System prompt:    ~{sys:>7} tokens\n\
             Conversation:     ~{conv:>7} tokens\n\
             Tool results:     ~{tool:>7} tokens\n\
             ────────────────────────────────────────\n\
             Total:            ~{total:>7} / {window} tokens ({pct:.1}%)\n\
             [{bar}] {pct:.1}%\n\n\
             Use /compact to reduce context usage.",
            model = model,
            sys = sys_prompt_tokens,
            conv = conv_tokens,
            tool = tool_tokens,
            total = total_tokens,
            window = context_window,
            pct = pct,
            bar = bar,
        ))
    }
}

// ---- /sandbox-toggle -----------------------------------------------------

#[async_trait]
impl SlashCommand for SandboxToggleCommand {
    fn name(&self) -> &str {
        "sandbox-toggle"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["sandbox"]
    }
    fn description(&self) -> &str {
        "Enable or disable sandboxed execution of shell commands"
    }
    fn help(&self) -> &str {
        "Usage: /sandbox-toggle [on|off]\n\n\
         Toggles sandboxed execution of bash/shell commands.\n\
         When sandbox mode is enabled, shell commands run in an isolated\n\
         environment to prevent unintended side effects.\n\n\
         With no argument: toggle the current state.\n\
         With 'on' or 'off': set explicitly.\n\n\
         Note: A restart is recommended for full effect."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        // Read current sandbox state from ui-settings
        let current_ui = load_ui_settings();
        let currently_enabled = current_ui.sandbox_mode.unwrap_or(false);

        let enable = match args.trim() {
            "on" | "enable" | "enabled" | "true" | "1" => true,
            "off" | "disable" | "disabled" | "false" | "0" => false,
            "" => !currently_enabled,
            other => {
                return CommandResult::Error(format!(
                    "Unknown argument '{}'. Use: /sandbox-toggle [on|off]",
                    other
                ))
            }
        };

        match mutate_ui_settings(|s| s.sandbox_mode = Some(enable)) {
            Ok(_) => {
                let state = if enable { "enabled" } else { "disabled" };
                CommandResult::Message(format!(
                    "Sandbox mode {}. Restart recommended for full effect.",
                    state
                ))
            }
            Err(e) => CommandResult::Error(format!("Failed to save sandbox setting: {}", e)),
        }
    }
}

// ---- /heapdump -----------------------------------------------------------

#[async_trait]
impl SlashCommand for HeapdumpCommand {
    fn name(&self) -> &str {
        "heapdump"
    }
    fn description(&self) -> &str {
        "Show process memory and diagnostic information"
    }
    fn help(&self) -> &str {
        "Usage: /heapdump\n\n\
         Displays a diagnostic snapshot of the current process:\n\
         process ID, platform, architecture, and available memory info.\n\
         On Linux, reads /proc/self/status for RSS/VmPeak figures.\n\
         On other platforms, reports what is available from the OS."
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let pid = std::process::id();
        let platform = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("  Process ID : {}", pid));
        lines.push(format!("  Platform   : {}", platform));
        lines.push(format!("  Arch       : {}", arch));

        // On Linux, pull memory figures from /proc/self/status
        #[cfg(target_os = "linux")]
        {
            match std::fs::read_to_string("/proc/self/status") {
                Ok(status) => {
                    for line in status.lines() {
                        let key = line.split(':').next().unwrap_or("").trim();
                        if matches!(key, "VmPeak" | "VmRSS" | "VmSize" | "VmData" | "Threads") {
                            let value = line.split(':').nth(1).unwrap_or("").trim();
                            lines.push(format!("  {:10} : {}", key, value));
                        }
                    }
                }
                Err(e) => {
                    lines.push(format!("  (could not read /proc/self/status: {})", e));
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            lines.push("  Memory stats: not available on this platform".to_string());
            lines.push("  (Linux /proc/self/status required for detailed figures)".to_string());
        }

        let body = lines.join("\n");
        CommandResult::Message(format!(
            "Heap Diagnostic\n\
             ─────────────────────────────\n\
             {body}"
        ))
    }
}

// ---- /insights -----------------------------------------------------------

#[async_trait]
impl SlashCommand for InsightsCommand {
    fn name(&self) -> &str {
        "insights"
    }
    fn description(&self) -> &str {
        "Generate a session analysis report with conversation statistics"
    }
    fn help(&self) -> &str {
        "Usage: /insights\n\n\
         Analyses the current conversation and prints a statistics report:\n\
         turn count, token usage, tools invoked, most-used tool, and more."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let messages = &ctx.messages;

        // Count turns (user / assistant pairs)
        let user_turns: usize = messages
            .iter()
            .filter(|m| matches!(m.role, cc_core::types::Role::User))
            .count();
        let assistant_turns: usize = messages
            .iter()
            .filter(|m| matches!(m.role, cc_core::types::Role::Assistant))
            .count();
        let total_turns = user_turns.min(assistant_turns);

        // Count tool_use blocks and track frequency
        let mut tool_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for msg in messages {
            for block in msg.get_tool_use_blocks() {
                if let cc_core::types::ContentBlock::ToolUse { name, .. } = block {
                    *tool_counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
        }
        let total_tool_calls: usize = tool_counts.values().sum();
        let most_frequent_tool = tool_counts
            .iter()
            .max_by_key(|(_, &v)| v)
            .map(|(k, v)| format!("{} ({} calls)", k, v))
            .unwrap_or_else(|| "none".to_string());

        // Token stats from cost_tracker
        let input_tokens = ctx.cost_tracker.input_tokens();
        let output_tokens = ctx.cost_tracker.output_tokens();
        let total_tokens = ctx.cost_tracker.total_tokens();

        let avg_tokens_per_turn = if total_turns > 0 {
            total_tokens / total_turns as u64
        } else {
            0
        };

        CommandResult::Message(format!(
            "Session Insights\n\
             ──────────────────────────────────────\n\
             Conversation\n\
             ├─ User turns          : {user_turns}\n\
             ├─ Assistant turns     : {assistant_turns}\n\
             └─ Completed exchanges : {total_turns}\n\
             \n\
             Tokens\n\
             ├─ Input               : {input_tokens}\n\
             ├─ Output              : {output_tokens}\n\
             ├─ Total               : {total_tokens}\n\
             └─ Avg per exchange    : {avg_tokens_per_turn}\n\
             \n\
             Tools\n\
             ├─ Total calls         : {total_tool_calls}\n\
             └─ Most used           : {most_frequent_tool}",
            user_turns = user_turns,
            assistant_turns = assistant_turns,
            total_turns = total_turns,
            input_tokens = input_tokens,
            output_tokens = output_tokens,
            total_tokens = total_tokens,
            avg_tokens_per_turn = avg_tokens_per_turn,
            total_tool_calls = total_tool_calls,
            most_frequent_tool = most_frequent_tool,
        ))
    }
}

// ---- /ultrareview --------------------------------------------------------

#[async_trait]
impl SlashCommand for UltrareviewCommand {
    fn name(&self) -> &str {
        "ultrareview"
    }
    fn description(&self) -> &str {
        "Run an exhaustive multi-dimensional code review"
    }
    fn help(&self) -> &str {
        "Usage: /ultrareview [path]\n\n\
         Runs a comprehensive code review that goes beyond /review and\n\
         /security-review. Covers: security (OWASP Top 10), performance,\n\
         maintainability, test coverage, error handling, API design,\n\
         documentation, accessibility, and architectural concerns.\n\
         Each finding is tagged by category and severity."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let target = if args.trim().is_empty() {
            ctx.working_dir.display().to_string()
        } else {
            args.trim().to_string()
        };

        CommandResult::UserMessage(format!(
            "Please perform an **ultra-comprehensive code review** of the code in `{target}`.\n\n\
             This review must go beyond a standard review and cover ALL of the following dimensions:\n\n\
             ## 1. Security (OWASP Top 10 + extras)\n\
             - Injection vulnerabilities (SQL, command, LDAP, XSS, SSTI, CRLF)\n\
             - Broken authentication / session management\n\
             - Sensitive data exposure (secrets, PII, tokens in logs or source)\n\
             - XML/JSON External Entity (XXE) processing\n\
             - Broken access control and privilege escalation paths\n\
             - Security misconfiguration (default creds, open ports, verbose errors)\n\
             - Cross-site scripting (Stored, Reflected, DOM-based)\n\
             - Insecure deserialization\n\
             - Using components with known vulnerabilities (outdated deps)\n\
             - Insufficient logging and monitoring\n\
             - Path traversal and file inclusion\n\
             - Race conditions, TOCTOU, deadlocks\n\
             - Cryptographic weaknesses (weak algorithms, key reuse, bad IV)\n\
             - Supply chain / dependency confusion risks\n\n\
             ## 2. Performance\n\
             - Algorithmic complexity: O(n²) or worse in hot paths\n\
             - Unnecessary allocations, copies, or clones\n\
             - Database N+1 query patterns\n\
             - Missing indexes on frequently queried fields\n\
             - Blocking I/O in async contexts\n\
             - Unbounded loops or recursion\n\
             - Memory leaks or resource leaks (file handles, sockets)\n\
             - Caching opportunities\n\n\
             ## 3. Maintainability & Code Quality\n\
             - Functions / methods exceeding 50 lines\n\
             - Deep nesting (>4 levels)\n\
             - Duplicated logic (DRY violations)\n\
             - Magic numbers and strings without named constants\n\
             - Misleading names (variables, functions, types)\n\
             - Dead code and unused imports\n\
             - Overly complex conditionals\n\
             - Coupling: tight coupling between unrelated modules\n\n\
             ## 4. Error Handling\n\
             - Swallowed errors (empty catch blocks, `unwrap()` without context)\n\
             - Panic-able paths in library code\n\
             - Missing input validation at trust boundaries\n\
             - Unclear error messages that hinder debugging\n\
             - Error type inconsistency across the codebase\n\n\
             ## 5. Test Coverage\n\
             - Missing unit tests for critical logic\n\
             - Missing integration tests for external boundaries\n\
             - Tests with no assertions\n\
             - Tests that are brittle (time-dependent, order-dependent)\n\
             - Missing negative / edge-case tests\n\
             - Mocking strategy concerns\n\n\
             ## 6. API Design\n\
             - Unclear or inconsistent naming conventions\n\
             - Functions with too many parameters (>5)\n\
             - Mutable global state\n\
             - Missing or incorrect use of visibility modifiers\n\
             - Breaking changes risk in public interfaces\n\
             - Lack of builder or fluent patterns where appropriate\n\n\
             ## 7. Documentation\n\
             - Missing doc comments on public items\n\
             - Outdated or misleading comments\n\
             - Undocumented panics, unsafe blocks, or invariants\n\
             - Missing README or high-level architectural overview\n\n\
             ## 8. Architectural Concerns\n\
             - Single Responsibility Principle violations\n\
             - Circular dependencies\n\
             - Missing abstraction layers\n\
             - Hardcoded configuration that should be externalised\n\
             - Observability gaps (missing tracing, metrics, structured logs)\n\n\
             ## Output Format\n\
             For **every** finding, provide:\n\
             - **Category** (from the dimensions above)\n\
             - **Severity**: Critical / High / Medium / Low / Informational\n\
             - **File** and **line number** (if applicable)\n\
             - **Description** of the issue\n\
             - **Impact**: what can go wrong\n\
             - **Recommended fix** with a code snippet where helpful\n\n\
             Start by reading the main source files, dependency manifests, and any CI/CD configuration.\n\
             Group findings by severity (Critical first). Conclude with a prioritised action plan.",
            target = target,
        ))
    }
}

// ---- Named-command slash adapters ----------------------------------------

#[async_trait]
impl SlashCommand for NamedCommandAdapter {
    fn name(&self) -> &str {
        self.slash_name
    }

    fn aliases(&self) -> Vec<&str> {
        self.slash_aliases.to_vec()
    }

    fn description(&self) -> &str {
        self.slash_description
    }

    fn help(&self) -> &str {
        self.slash_help
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        execute_named_command_from_slash(self.target_name, args, ctx)
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Return all built-in slash commands.
pub fn all_commands() -> Vec<Box<dyn SlashCommand>> {
    vec![
        Box::new(HelpCommand),
        Box::new(ClearCommand),
        Box::new(CompactCommand),
        Box::new(CostCommand),
        Box::new(ExitCommand),
        Box::new(ModelCommand),
        Box::new(ConfigCommand),
        Box::new(ColorCommand),
        Box::new(PluginCommand),
        Box::new(VersionCommand),
        Box::new(ResumeCommand),
        Box::new(ReloadPluginsCommand),
        Box::new(StatusCommand),
        Box::new(DiffCommand),
        Box::new(MemoryCommand),
        Box::new(BugCommand),
        Box::new(UsageCommand),
        Box::new(DoctorCommand),
        Box::new(LoginCommand),
        Box::new(LogoutCommand),
        Box::new(InitCommand),
        Box::new(ReviewCommand),
        Box::new(HooksCommand),
        Box::new(McpCommand),
        Box::new(PermissionsCommand),
        Box::new(PlanCommand),
        Box::new(TasksCommand),
        Box::new(SessionCommand),
        Box::new(ThinkingCommand),
        Box::new(ThemeCommand),
        Box::new(OutputStyleCommand),
        Box::new(KeybindingsCommand),
        Box::new(PrivacySettingsCommand),
        // New commands
        Box::new(ExportCommand),
        Box::new(SkillsCommand),
        Box::new(RewindCommand),
        Box::new(StatsCommand),
        Box::new(FilesCommand),
        Box::new(RenameCommand),
        Box::new(EffortCommand),
        Box::new(SummaryCommand),
        Box::new(CommitCommand),
        Box::new(NamedCommandAdapter {
            slash_name: "add-dir",
            target_name: "add-dir",
            slash_aliases: &[],
            slash_description: "Add a directory to Uppli Code's allowed workspace paths",
            slash_help: "Usage: /add-dir <path>",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "agents",
            target_name: "agents",
            slash_aliases: &[],
            slash_description: "Manage and configure sub-agents",
            slash_help: "Usage: /agents [list|create|edit|delete] [name]",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "branch",
            target_name: "branch",
            slash_aliases: &[],
            slash_description: "Create a branch of the current conversation at this point",
            slash_help: "Usage: /branch [create|switch|list] [name]",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "tag",
            target_name: "tag",
            slash_aliases: &[],
            slash_description: "Toggle a searchable tag on the current session",
            slash_help: "Usage: /tag [list|add|remove] [tag]",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "passes",
            target_name: "passes",
            slash_aliases: &[],
            slash_description: "Share a free week of Uppli Code with friends",
            slash_help: "Usage: /passes",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "ide",
            target_name: "ide",
            slash_aliases: &[],
            slash_description: "Manage IDE integrations and show status",
            slash_help: "Usage: /ide [status|connect|disconnect|open]",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "pr-comments",
            target_name: "pr-comments",
            slash_aliases: &[],
            slash_description: "Get comments from a GitHub pull request",
            slash_help: "Usage: /pr-comments <PR-number>",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "desktop",
            target_name: "desktop",
            slash_aliases: &[],
            slash_description: "Open the Uppli Code desktop app",
            slash_help: "Usage: /desktop",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "mobile",
            target_name: "mobile",
            slash_aliases: &[],
            slash_description: "Set up Uppli Code on mobile",
            slash_help: "Usage: /mobile",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "install-github-app",
            target_name: "install-github-app",
            slash_aliases: &[],
            slash_description: "Set up Claude GitHub Actions for a repository",
            slash_help: "Usage: /install-github-app",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "web-setup",
            target_name: "remote-setup",
            slash_aliases: &["remote-setup"],
            slash_description: "Configure a remote Uppli Code environment",
            slash_help: "Usage: /web-setup",
        }),
        Box::new(NamedCommandAdapter {
            slash_name: "stickers",
            target_name: "stickers",
            slash_aliases: &[],
            slash_description: "View collected stickers",
            slash_help: "Usage: /stickers",
        }),
        // Batch-1 new commands
        Box::new(RemoteControlCommand),
        Box::new(RemoteEnvCommand),
        Box::new(ContextCommand),
        Box::new(CopyCommand),
        Box::new(ChromeCommand),
        Box::new(VimCommand),
        Box::new(VoiceCommand),
        Box::new(UpgradeCommand),
        Box::new(ReleaseNotesCommand),
        Box::new(RateLimitOptionsCommand),
        Box::new(StatuslineCommand),
        Box::new(SecurityReviewCommand),
        Box::new(TerminalSetupCommand),
        Box::new(ExtraUsageCommand),
        Box::new(FastCommand),
        Box::new(ThinkBackCommand),
        Box::new(ThinkBackPlayCommand),
        Box::new(FeedbackCommand),
        Box::new(ColorSetCommand),
        // New commands: share, teleport, btw, ctx-viz, sandbox-toggle
        Box::new(ShareCommand),
        Box::new(TeleportCommand),
        Box::new(BtwCommand),
        Box::new(CtxVizCommand),
        Box::new(SandboxToggleCommand),
        // Advisor and Slack integration
        Box::new(AdvisorCommand),
        Box::new(InstallSlackAppCommand),
        // Diagnostics / analysis
        Box::new(HeapdumpCommand),
        Box::new(InsightsCommand),
        Box::new(UltrareviewCommand),
    ]
}

/// Find a command by name or alias.
pub fn find_command(name: &str) -> Option<Box<dyn SlashCommand>> {
    let name = name.trim_start_matches('/');
    all_commands()
        .into_iter()
        .find(|c| c.name() == name || c.aliases().contains(&name))
}

/// Build `HelpEntry` values for all non-hidden commands, suitable for
/// populating `HelpOverlay::commands` at startup.
pub fn build_help_entries() -> Vec<cc_tui::overlays::HelpEntry> {
    all_commands()
        .iter()
        .filter(|c| !c.hidden())
        .map(|c| cc_tui::overlays::HelpEntry {
            name: c.name().to_string(),
            aliases: c.aliases().join(", "),
            description: c.description().to_string(),
            category: command_category(c.name()).to_string(),
        })
        .collect()
}

/// Execute a slash command string (with leading /).
pub async fn execute_command(input: &str, ctx: &mut CommandContext) -> Option<CommandResult> {
    if !cc_tui::input::is_slash_command(input) {
        return None;
    }
    let (name, args) = cc_tui::input::parse_slash_command(input);

    // First check built-in commands.
    if let Some(cmd) = find_command(name) {
        return Some(cmd.execute(args, ctx).await);
    }

    // Then check plugin-defined slash commands.
    let project_dir = ctx.working_dir.clone();
    let registry = cc_plugins::load_plugins(&project_dir, &[]).await;
    let cmd_name = name.trim_start_matches('/');
    for cmd_def in registry.all_command_defs() {
        if cmd_def.name == cmd_name {
            let adapter = PluginSlashCommandAdapter { def: cmd_def };
            return Some(adapter.execute(args, ctx).await);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Named commands module (top-level `claude <name>` subcommands)
// ---------------------------------------------------------------------------
pub mod named_commands;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cc_core::cost::CostTracker;

    fn make_ctx() -> CommandContext {
        CommandContext {
            config: cc_core::config::Config::default(),
            cost_tracker: CostTracker::new(),
            messages: vec![],
            working_dir: std::path::PathBuf::from("."),
            session_id: "test-session".to_string(),
            session_title: None,
            remote_session_url: None,
            mcp_manager: None,
        }
    }

    // ---- Command registry tests ---------------------------------------------

    #[test]
    fn test_all_commands_non_empty() {
        assert!(!all_commands().is_empty());
    }

    #[test]
    fn test_all_commands_have_unique_names() {
        let mut names = std::collections::HashSet::new();
        for cmd in all_commands() {
            assert!(
                names.insert(cmd.name().to_string()),
                "Duplicate command name: {}",
                cmd.name()
            );
        }
    }

    #[test]
    fn test_find_command_by_name() {
        assert!(find_command("help").is_some());
        assert!(find_command("clear").is_some());
        assert!(find_command("exit").is_some());
        assert!(find_command("model").is_some());
        assert!(find_command("version").is_some());
    }

    #[test]
    fn test_find_command_with_slash_prefix() {
        // find_command should strip the leading / before lookup
        assert!(find_command("/help").is_some());
        assert!(find_command("/clear").is_some());
    }

    #[test]
    fn test_find_command_by_alias() {
        // /help has aliases "h" and "?"
        assert!(find_command("h").is_some());
        assert!(find_command("?").is_some());
        // /clear has alias "c"
        assert!(find_command("c").is_some());
        assert!(find_command("settings").is_some());
        assert!(find_command("continue").is_some());
        assert!(find_command("bug").is_some());
        assert!(find_command("bashes").is_some());
        assert!(find_command("remote").is_some());
        assert!(find_command("remote-setup").is_some());
    }

    #[test]
    fn test_find_command_not_found() {
        assert!(find_command("nonexistent_command_xyz").is_none());
    }

    #[test]
    fn test_core_commands_present() {
        let expected = [
            "help",
            "clear",
            "compact",
            "cost",
            "exit",
            "model",
            "config",
            "version",
            "status",
            "diff",
            "memory",
            "hooks",
            "permissions",
            "plan",
            "tasks",
            "session",
            "login",
            "logout",
            "feedback",
            "usage",
            "plugin",
            "reload-plugins",
            "add-dir",
            "agents",
            "branch",
            "tag",
            "passes",
            "ide",
            "pr-comments",
            "desktop",
            "mobile",
            "install-github-app",
            "web-setup",
            "stickers",
        ];
        for name in &expected {
            assert!(
                find_command(name).is_some(),
                "Expected command '{}' not in all_commands()",
                name
            );
        }
    }

    // ---- Command execution tests --------------------------------------------

    #[tokio::test]
    async fn test_clear_command_returns_clear_conversation() {
        let mut ctx = make_ctx();
        let cmd = find_command("clear").unwrap();
        let result = cmd.execute("", &mut ctx).await;
        assert!(matches!(result, CommandResult::ClearConversation));
    }

    #[tokio::test]
    async fn test_exit_command_returns_exit() {
        let mut ctx = make_ctx();
        let cmd = find_command("exit").unwrap();
        let result = cmd.execute("", &mut ctx).await;
        assert!(matches!(result, CommandResult::Exit));
    }

    #[tokio::test]
    async fn test_version_command_returns_message() {
        let mut ctx = make_ctx();
        let cmd = find_command("version").unwrap();
        let result = cmd.execute("", &mut ctx).await;
        assert!(matches!(result, CommandResult::Message(_)));
        if let CommandResult::Message(msg) = result {
            assert!(
                msg.contains("claude") || msg.contains("Claude") || msg.contains('.'),
                "Version message should contain version number, got: {}",
                msg
            );
        }
    }

    #[tokio::test]
    async fn test_cost_command_returns_message() {
        let mut ctx = make_ctx();
        let cmd = find_command("cost").unwrap();
        let result = cmd.execute("", &mut ctx).await;
        assert!(matches!(result, CommandResult::Message(_)));
    }

    #[tokio::test]
    async fn test_login_command_starts_oauth_flow() {
        let mut ctx = make_ctx();
        let cmd = find_command("login").unwrap();
        // Default (no --console) → login_with_claude_ai = true
        let result = cmd.execute("", &mut ctx).await;
        assert!(matches!(result, CommandResult::StartOAuthFlow(true)));
    }

    #[tokio::test]
    async fn test_login_command_console_flag() {
        let mut ctx = make_ctx();
        let cmd = find_command("login").unwrap();
        let result = cmd.execute("--console", &mut ctx).await;
        assert!(matches!(result, CommandResult::StartOAuthFlow(false)));
    }

    #[tokio::test]
    async fn test_help_command_returns_message() {
        let mut ctx = make_ctx();
        let cmd = find_command("help").unwrap();
        let result = cmd.execute("", &mut ctx).await;
        // help returns either Message or Silent
        assert!(
            matches!(result, CommandResult::Message(_) | CommandResult::Silent),
            "help should return Message or Silent"
        );
    }

    #[tokio::test]
    async fn test_web_setup_proxy_executes_named_command() {
        let mut ctx = make_ctx();
        let cmd = find_command("web-setup").unwrap();
        let result = cmd.execute("", &mut ctx).await;
        assert!(matches!(result, CommandResult::Message(_)));
    }

    #[test]
    fn test_split_command_args_preserves_quoted_segments() {
        assert_eq!(
            split_command_args("create \"agent alpha\" 'second value'"),
            vec![
                "create".to_string(),
                "agent alpha".to_string(),
                "second value".to_string(),
            ]
        );
    }
}
