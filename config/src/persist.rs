//! Minimal, comment-preserving persistence of individual settings back into
//! the user's JSONC config file.
//!
//! The app config is JSONC (comments allowed) and is normally hand-edited, so
//! we deliberately avoid a serde round-trip (which would strip every comment).
//! Instead we surgically set a single top-level key.

use anyhow::Context;

/// Persist the chosen `color_scheme` into the active config file on disk,
/// preserving existing comments and formatting.
///
/// The file watcher (or an explicit `config::reload()`) will pick up the
/// change and apply it live.
pub fn persist_color_scheme(name: &str) -> anyhow::Result<()> {
    let loaded = crate::Config::load();
    let path = loaded
        .file_name
        .context("no config file path available to persist color scheme")?;

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config file {}", path.display()))?;

    let updated = set_top_level_string(&content, "color_scheme", name);

    std::fs::write(&path, updated)
        .with_context(|| format!("writing config file {}", path.display()))?;

    log::info!("Persisted color_scheme = {:?} to {}", name, path.display());
    Ok(())
}

/// Escape a string for embedding inside a JSON double-quoted literal.
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Set (or insert) a top-level string `key` = `value` in a JSONC document,
/// preserving comments and formatting.
///
/// - If an active (non-commented) `"key": ...` line exists, its whole line is
///   rewritten (indentation preserved) to the new value.
/// - Otherwise a new `    "key": "value",` line is inserted right after the
///   opening `{` of the top-level object.
///
/// Commented-out occurrences (lines whose first non-space token is `//`) are
/// ignored, so the example line in the default config never confuses us.
fn set_top_level_string(content: &str, key: &str, value: &str) -> String {
    let escaped = json_escape(value);
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let key_token = format!("\"{key}\"");

    let mut lines: Vec<String> = content
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();

    // 1. Rewrite an existing active key line, preserving whether it ended with
    //    a trailing comma (so a last-key with no comma stays valid JSON).
    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with(&key_token) {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            // Scheme values never contain `//`, so this safely drops any inline
            // trailing comment while leaving the structural text intact.
            let code = line.split("//").next().unwrap_or(line).trim_end();
            let comma = if code.ends_with(',') { "," } else { "" };
            *line = format!("{indent}\"{key}\": \"{escaped}\"{comma}");
            return lines.join(newline);
        }
    }

    // 2. Insert after the first opening brace of the top-level object. A comma
    //    is needed only when the object already has other active keys (ours is
    //    inserted first, so every existing key follows it); an otherwise-empty
    //    object (e.g. the all-commented default) must get no trailing comma.
    let comma = if object_has_active_content(content) { "," } else { "" };
    for i in 0..lines.len() {
        let trimmed = lines[i].trim();
        if trimmed == "{" || trimmed.ends_with('{') {
            lines.insert(i + 1, format!("    \"{key}\": \"{escaped}\"{comma}"));
            return lines.join(newline);
        }
    }

    // 3. Degenerate input with no object opener: leave it unchanged.
    lines.join(newline)
}

/// True if the top-level object contains any non-comment content between its
/// outermost braces.
fn object_has_active_content(content: &str) -> bool {
    let stripped = crate::jsonc::strip_jsonc_comments(content);
    if let (Some(open), Some(close)) = (stripped.find('{'), stripped.rfind('}')) {
        if open < close {
            return !stripped[open + 1..close].trim().is_empty();
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_value(content: &str, key: &str) -> Option<String> {
        let stripped = crate::jsonc::strip_jsonc_comments(content);
        let v: serde_json::Value = serde_json::from_str(&stripped).expect("valid JSON after edit");
        v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
    }

    #[test]
    fn replaces_existing_active_value() {
        let input = "{\n    \"font_size\": 12.0,\n    \"color_scheme\": \"old scheme\",\n    \"scrollback_lines\": 10000\n}\n";
        let out = set_top_level_string(input, "color_scheme", "Tokyo Night");
        assert_eq!(parsed_value(&out, "color_scheme").as_deref(), Some("Tokyo Night"));
        // Untouched keys survive.
        assert_eq!(parsed_value(&out, "scrollback_lines"), None); // number, not str
        assert!(out.contains("\"font_size\": 12.0"));
        // Only one active color_scheme line.
        assert_eq!(out.matches("\"color_scheme\":").count(), 1);
    }

    #[test]
    fn inserts_when_only_commented_example_present() {
        // Mirrors the shape of the generated default config.
        let input = "{\n    // \"color_scheme\": \"Gruvbox Dark (Gogh)\",\n    // \"scrollback_lines\": 10000,\n}\n";
        let out = set_top_level_string(input, "color_scheme", "Catppuccin Mocha");
        assert_eq!(
            parsed_value(&out, "color_scheme").as_deref(),
            Some("Catppuccin Mocha")
        );
        // The commented example line is preserved.
        assert!(out.contains("// \"color_scheme\""));
    }

    #[test]
    fn inserts_when_absent() {
        let input = "{\n    \"font_size\": 12.0\n}\n";
        let out = set_top_level_string(input, "color_scheme", "Solarized Dark (Gogh)");
        assert_eq!(
            parsed_value(&out, "color_scheme").as_deref(),
            Some("Solarized Dark (Gogh)")
        );
    }

    #[test]
    fn scheme_name_with_special_chars_stays_valid() {
        let input = "{\n    \"color_scheme\": \"x\"\n}\n";
        // A pathological name with a quote and backslash must be escaped.
        let out = set_top_level_string(input, "color_scheme", "we\"ir\\d");
        assert_eq!(parsed_value(&out, "color_scheme").as_deref(), Some("we\"ir\\d"));
    }

    #[test]
    fn applying_twice_is_stable() {
        let input = "{\n    \"color_scheme\": \"a\"\n}\n";
        let once = set_top_level_string(input, "color_scheme", "Nord (Gogh)");
        let twice = set_top_level_string(&once, "color_scheme", "Nord (Gogh)");
        assert_eq!(once, twice);
        assert_eq!(parsed_value(&twice, "color_scheme").as_deref(), Some("Nord (Gogh)"));
    }

    #[test]
    fn preserves_crlf_line_endings() {
        let input = "{\r\n    \"color_scheme\": \"a\"\r\n}\r\n";
        let out = set_top_level_string(input, "color_scheme", "b");
        assert!(out.contains("\r\n"), "CRLF endings should be preserved");
        assert_eq!(parsed_value(&out, "color_scheme").as_deref(), Some("b"));
    }
}
