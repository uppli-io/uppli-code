// effort.rs — EffortLevel enum and associated helpers.
//
// Maps to src/utils/effort.ts in the TypeScript source.  The Rust port
// retains only the subset of logic that is useful in a non-browser / non-GrowthBook
// environment: the level → thinking-budget / temperature / glyph mappings.
//
// The thinking-budget and temperature values must match the TypeScript source
// exactly because they are passed to the Anthropic API.

// ---------------------------------------------------------------------------
// EffortLevel enum
// ---------------------------------------------------------------------------

/// The four named effort levels supported by Uppli Code.
///
/// Matches the `EffortLevel` type from `src/entrypoints/sdk/runtimeTypes.ts`
/// / `src/utils/effort.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffortLevel {
    /// Quick, straightforward implementation with minimal overhead.
    Low,
    /// Balanced approach with standard implementation and testing.
    Medium,
    /// Comprehensive implementation with extensive testing and documentation.
    High,
    /// Maximum capability with deepest reasoning (64K thinking tokens).
    Max,
}

impl EffortLevel {
    /// Parse an effort level from its string representation.
    ///
    /// Accepts lowercase strings: `"low"`, `"medium"`, `"high"`, `"max"`.
    /// Returns `None` for any other value.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    /// The lowercase string name of this effort level.
    ///
    /// Round-trips with `from_str`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// Return the extended-thinking budget in tokens for this effort level,
    /// or `None` if thinking should be disabled.
    ///
    /// Adapted for DeepSeek R2 which supports up to 64K thinking tokens:
    ///   Low    → 8 000   (light reasoning)
    ///   Medium → 16 000
    ///   High   → 32 000
    ///   Max    → 64 000  (full DeepSeek reasoning capacity)
    pub fn thinking_budget_tokens(&self) -> Option<u32> {
        match self {
            Self::Low => Some(8_000),
            Self::Medium => Some(16_000),
            Self::High => Some(32_000),
            Self::Max => Some(64_000),
        }
    }

    /// Return the temperature override for this effort level, or `None` to
    /// use the model's default.
    ///
    /// Values mirror the TypeScript source:
    ///   Low    → Some(0.0) — deterministic, cheap
    ///   Medium → None      — model default
    ///   High   → None      — model default
    ///   Max    → None      — model default
    pub fn temperature(&self) -> Option<f32> {
        match self {
            Self::Low => Some(0.0),
            Self::Medium | Self::High | Self::Max => None,
        }
    }

    /// A single Unicode glyph used to represent this effort level in the TUI.
    ///
    /// Glyphs mirror the TypeScript EffortCallout / status-bar rendering:
    ///   Low    → "○"  (empty circle)
    ///   Medium → "◐"  (half circle)
    ///   High   → "●"  (filled circle)
    ///   Max    → "◉"  (circled circle)
    pub fn glyph(&self) -> &'static str {
        match self {
            Self::Low => "○",
            Self::Medium => "◐",
            Self::High => "●",
            Self::Max => "◉",
        }
    }

    /// Human-readable description of this effort level.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Low => "Quick, straightforward implementation with minimal overhead",
            Self::Medium => "Balanced approach with standard implementation and testing",
            Self::High => "Comprehensive implementation with extensive testing and documentation",
            Self::Max => "Maximum capability with deepest reasoning (64K thinking tokens)",
        }
    }
}

impl std::fmt::Display for EffortLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_roundtrips() {
        for level in [
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Max,
        ] {
            let parsed = EffortLevel::from_str(level.as_str());
            assert_eq!(
                parsed,
                Some(level),
                "from_str({:?}) should round-trip",
                level
            );
        }
    }

    #[test]
    fn from_str_case_insensitive() {
        assert_eq!(EffortLevel::from_str("LOW"), Some(EffortLevel::Low));
        assert_eq!(EffortLevel::from_str("Medium"), Some(EffortLevel::Medium));
        assert_eq!(EffortLevel::from_str("HIGH"), Some(EffortLevel::High));
        assert_eq!(EffortLevel::from_str("Max"), Some(EffortLevel::Max));
    }

    #[test]
    fn from_str_unknown_returns_none() {
        assert_eq!(EffortLevel::from_str("ultra"), None);
        assert_eq!(EffortLevel::from_str(""), None);
        assert_eq!(EffortLevel::from_str("3"), None);
    }

    #[test]
    fn thinking_budget_deepseek() {
        assert_eq!(EffortLevel::Low.thinking_budget_tokens(), Some(8_000));
        assert_eq!(EffortLevel::Medium.thinking_budget_tokens(), Some(16_000));
        assert_eq!(EffortLevel::High.thinking_budget_tokens(), Some(32_000));
        assert_eq!(EffortLevel::Max.thinking_budget_tokens(), Some(64_000));
    }

    #[test]
    fn temperature_matches_ts() {
        // Low → 0.0 (deterministic)
        assert_eq!(EffortLevel::Low.temperature(), Some(0.0));
        // All others → None (model default)
        assert_eq!(EffortLevel::Medium.temperature(), None);
        assert_eq!(EffortLevel::High.temperature(), None);
        assert_eq!(EffortLevel::Max.temperature(), None);
    }

    #[test]
    fn glyphs_match_ts() {
        assert_eq!(EffortLevel::Low.glyph(), "○");
        assert_eq!(EffortLevel::Medium.glyph(), "◐");
        assert_eq!(EffortLevel::High.glyph(), "●");
        assert_eq!(EffortLevel::Max.glyph(), "◉");
    }

    #[test]
    fn display_matches_as_str() {
        for level in [
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Max,
        ] {
            assert_eq!(format!("{}", level), level.as_str());
        }
    }
}
