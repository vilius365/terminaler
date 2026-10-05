use terminaler_dynamic::{FromDynamic, ToDynamic};

pub const BRIEF_BAR_MIN_ROWS: u8 = 1;
pub const BRIEF_BAR_MAX_ROWS: u8 = 4;
pub const BRIEF_BAR_MIN_POLL_SECONDS: u64 = 5;

/// Always-visible strip across the top of the window with one cell per live
/// Claude Code session (name, goal, current step). The feed is an external
/// command that prints a JSON array on stdout.
#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub struct BriefBarConfig {
    /// Master switch. Defaults to false: the strip reserves window space.
    #[dynamic(default)]
    pub enabled: bool,

    /// argv run on every poll; must print the JSON array on stdout.
    #[dynamic(default)]
    pub command: Vec<String>,

    /// Seconds between polls (minimum 5).
    #[dynamic(default = "default_poll_interval_seconds")]
    pub poll_interval_seconds: u64,

    /// Seconds before a poll is killed.
    #[dynamic(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,

    /// Fixed number of text rows reserved (1..=4). The strip never changes
    /// height as sessions come and go.
    #[dynamic(default = "default_rows")]
    pub rows: u8,
}

fn default_poll_interval_seconds() -> u64 {
    20
}

fn default_timeout_seconds() -> u64 {
    10
}

fn default_rows() -> u8 {
    3
}

impl Default for BriefBarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: vec![],
            poll_interval_seconds: default_poll_interval_seconds(),
            timeout_seconds: default_timeout_seconds(),
            rows: default_rows(),
        }
    }
}

impl BriefBarConfig {
    /// Rows clamped to the supported range.
    pub fn effective_rows(&self) -> u8 {
        self.rows.clamp(BRIEF_BAR_MIN_ROWS, BRIEF_BAR_MAX_ROWS)
    }

    /// Pixel height reserved for the strip: `rows` lines plus padding.
    pub fn strip_height_px(&self, cell_height: f32) -> f32 {
        self.effective_rows() as f32 * cell_height + BRIEF_BAR_PADDING_PX
    }
}

/// Vertical padding added to the strip on top of its text rows.
pub const BRIEF_BAR_PADDING_PX: f32 = 4.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rows_is_three() {
        assert_eq!(BriefBarConfig::default().rows, 3);
        assert!(!BriefBarConfig::default().enabled);
    }

    fn parse(json: &str) -> anyhow::Result<BriefBarConfig> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        let dynamic = crate::json_to_dynamic(&value);
        Ok(BriefBarConfig::from_dynamic(
            &dynamic,
            terminaler_dynamic::FromDynamicOptions {
                unknown_fields: terminaler_dynamic::UnknownFieldAction::Deny,
                deprecated_fields: terminaler_dynamic::UnknownFieldAction::Deny,
            },
        )?)
    }

    #[test]
    fn parses_documented_shape() {
        let cfg = parse(
            r#"{
            "enabled": true,
            "command": ["ssh", "-o", "BatchMode=yes", "devbox", "~/.claude/scripts/brief", "--json"],
            "poll_interval_seconds": 20,
            "timeout_seconds": 10,
            "rows": 2
        }"#,
        )
        .unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.command.len(), 6);
        assert_eq!(cfg.rows, 2);
        // Omitted rows falls back to the default of 3.
        assert_eq!(parse(r#"{"enabled": true}"#).unwrap().rows, 3);
    }

    #[test]
    fn rejects_camel_case_keys() {
        assert!(parse(r#"{"pollIntervalSeconds": 20}"#).is_err());
    }

    #[test]
    fn height_calc() {
        let mut c = BriefBarConfig::default();
        assert_eq!(c.strip_height_px(20.0), 3.0 * 20.0 + 4.0);
        c.rows = 0;
        assert_eq!(c.strip_height_px(10.0), 14.0);
        c.rows = 9;
        assert_eq!(c.strip_height_px(10.0), 44.0);
    }
}
