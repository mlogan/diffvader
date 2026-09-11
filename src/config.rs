//! The user config file: `key = value` lines, read once at startup. Command-line flags
//! override it. Kept dependency-free (no TOML crate) because it is six keys.

use std::path::{Path, PathBuf};

use crate::diff::WhitespaceMode;

#[derive(Debug, Default, PartialEq)]
pub struct Config {
    /// A font file path or a family name (see `font::resolve`).
    pub font: Option<String>,
    pub font_pt: Option<f32>,
    pub tab_width: Option<u32>,
    pub light: Option<bool>,
    pub whitespace: Option<WhitespaceMode>,
    pub agent: Option<String>,
}

pub const TEMPLATE: &str = "\
# diffvader configuration. Command-line flags override these settings.
# Lines starting with # are comments; keys are `key = value`.

# Font: a file path, or a family name looked up in the usual font directories
# (~/Library/Fonts, /Library/Fonts, /System/Library/Fonts). Default: SF Mono, then Menlo.
#font = JetBrains Mono
#font = /Users/me/Library/Fonts/FiraCode-Regular.ttf

# Font size in points (6-48).
#font-size = 13

# Tab stop width (1-16).
#tab-width = 4

# Color theme: dark or light.
#theme = dark

# Whitespace mode: exact, eol, change or all (like git diff's --ignore-* options).
#whitespace = exact

# AI agent for `e`: claude, codex, gemini, or a command line ({prompt} marks the prompt).
#agent = claude
";

/// `$DIFFVADER_CONFIG`, else `$XDG_CONFIG_HOME/diffvader/config`, else
/// `~/.config/diffvader/config`.
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("DIFFVADER_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("diffvader").join("config")
}

/// A missing file is an empty config; a malformed one is an error naming the line.
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    let mut c = Config::default();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {}: expected `key = value`", n + 1));
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        let bad = |what: &str| format!("line {}: {key} must be {what}", n + 1);
        match key {
            "font" => c.font = Some(value.to_string()),
            "font-size" | "font_size" => {
                c.font_pt = Some(value.parse().map_err(|_| bad("a number"))?)
            }
            "tab-width" | "tab_width" => {
                c.tab_width = Some(value.parse().map_err(|_| bad("an integer"))?)
            }
            "theme" => {
                c.light = Some(match value {
                    "light" => true,
                    "dark" => false,
                    _ => return Err(bad("dark or light")),
                })
            }
            "whitespace" => {
                c.whitespace = Some(match value {
                    "exact" | "none" => WhitespaceMode::Exact,
                    "eol" | "ignore-eol" => WhitespaceMode::IgnoreEol,
                    "change" | "ignore-space-change" => WhitespaceMode::IgnoreChange,
                    "all" | "ignore-all-space" => WhitespaceMode::IgnoreAll,
                    _ => return Err(bad("exact, eol, change or all")),
                })
            }
            "agent" => c.agent = Some(value.to_string()),
            _ => return Err(format!("line {}: unknown key `{key}`", n + 1)),
        }
    }
    Ok(c)
}

/// Writes the commented template unless a config already exists.
pub fn init(path: &Path) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, TEMPLATE).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys() {
        let c = parse(
            "# comment\n\nfont = JetBrains Mono\nfont-size = 14.5\ntab-width=8\ntheme = light\nwhitespace = change\nagent = \"claude\"\n",
        )
        .unwrap();
        assert_eq!(c.font.as_deref(), Some("JetBrains Mono"));
        assert_eq!(c.font_pt, Some(14.5));
        assert_eq!(c.tab_width, Some(8));
        assert_eq!(c.light, Some(true));
        assert_eq!(c.whitespace, Some(WhitespaceMode::IgnoreChange));
        assert_eq!(c.agent.as_deref(), Some("claude"));
    }

    #[test]
    fn template_parses_to_defaults() {
        assert_eq!(parse(TEMPLATE).unwrap(), Config::default());
    }

    #[test]
    fn errors_name_the_line() {
        assert!(parse("font = x\nnope").unwrap_err().starts_with("line 2:"));
        assert!(parse("theme = blue").unwrap_err().contains("dark or light"));
        assert!(parse("size = 3").unwrap_err().contains("unknown key"));
    }
}
