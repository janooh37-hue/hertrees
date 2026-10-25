//! User configuration.
//!
//! Read from `$HERDR_PLUGIN_CONFIG_DIR/config.json`. Unknown keys are ignored,
//! and a broken file falls back to defaults instead of taking the picker down.
//! The display keys (`preview`, `mouse`, `agent_icons`, `refresh_ms`, `colors`,
//! `selection_background`, `wordmark`) are documented in the README.

use crate::pyjson::{self, Map, Value};
use crate::pystr;
use crate::wordmark::Style;
use std::path::{Path, PathBuf};

pub const SCOPES: &[&str] = &["all", "nopr", "open", "draft", "merged", "closed", "agents"];

/// Shared by the popup and headless commands, which have no plugin environment.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory));
    }
    let root = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()).map(PathBuf::from)
        .or_else(|| crate::pyos::home().map(|home| PathBuf::from(home).join(".config")))?;
    Some(root.join("herdr/plugins/config/hertrees"))
}

/// The plugin's private state directory (PR cache, deferred prompts): Herdr's
/// `HERDR_PLUGIN_STATE_DIR`, or the same location headless commands would get.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("HERDR_PLUGIN_STATE_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory));
    }
    let root = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()).map(PathBuf::from)
        .or_else(|| crate::pyos::home().map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(root.join("herdr/plugins/hertrees"))
}


#[derive(Clone, Debug)]
pub struct Config {
    pub preview: Value,
    pub mouse: bool,
    pub agent_icons: Value,
    pub refresh_ms: i128,
    pub colors: Map,
    pub selection_background: Value,
    /// Agent kinds offered, in order; empty means every installed supported agent.
    pub agents: Vec<String>,
    /// Agent selected first in the agent picker.
    pub default_agent: Option<String>,
    pub pr_cache_ttl_s: i64,
    /// The filter the picker opens with: one of `SCOPES`.
    pub scope: &'static str,
    /// Where and how the name is drawn; unknown values give the default.
    pub wordmark: Style,
}

/// Python's `int(value)` for whatever JSON put there; None where it raises.
fn py_int(value: &Value) -> Option<i128> {
    match value {
        Value::Int(i) => Some(*i),
        Value::Bool(b) => Some(*b as i128),
        Value::Float(f) if f.is_finite() => Some(f.trunc() as i128),
        Value::Str(s) => pystr::parse_int(s),
        _ => None,
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| pystr::lower(pystr::strip(s)))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}


impl Default for Config {
    fn default() -> Self {
        Config::from_values(&Map::new())
    }
}

impl Config {
    pub fn from_values(values: &Map) -> Config {
        let get = |key: &str| values.get(key);
        let or = |key: &str, default: Value| get(key).cloned().unwrap_or(default);
        let refresh_ms = match get("refresh_ms") {
            None => 900,
            Some(value) => py_int(value).map(|v| v.max(200)).unwrap_or(900),
        };
        Config {
            preview: or("preview", Value::str("auto")),
            mouse: get("mouse").map(|v| v.truthy()).unwrap_or(true),
            agent_icons: or("agent_icons", Value::str("auto")),
            refresh_ms,
            colors: match get("colors") {
                Some(Value::Dict(m)) => m.clone(),
                _ => Map::new(),
            },
            selection_background: or("selection_background", Value::str("auto")),
            agents: strings(get("agents")),
            default_agent: get("default_agent")
                .and_then(|v| v.as_str())
                .map(|s| pystr::lower(pystr::strip(s)))
                .filter(|s| !s.is_empty()),
            pr_cache_ttl_s: get("pr_cache_ttl_s").and_then(py_int).map(|v| v.clamp(0, 86_400) as i64).unwrap_or(60),
            scope: get("scope")
                .and_then(|v| v.as_str())
                .and_then(|name| SCOPES.iter().copied().find(|scope| *scope == name))
                .unwrap_or("all"),
            wordmark: get("wordmark")
                .and_then(|v| v.as_str())
                .and_then(|name| Style::parse(&pystr::lower(pystr::strip(name))))
                .unwrap_or(Style::Search),
        }
    }

    pub fn path() -> Option<PathBuf> {
        Some(config_dir()?.join("config.json"))
    }

    pub fn load() -> Config {
        let Some(path) = Config::path() else { return Config::default() };
        let Ok(data) = std::fs::read(&path) else { return Config::default() };
        let Ok(text) = String::from_utf8(data) else { return Config::default() };
        match pyjson::loads(&text) {
            Ok(Value::Dict(map)) => Config::from_values(&map),
            _ => Config::default(),
        }
    }

    /// Sets one key in config.json, keeping every other key and their order.
    /// A file that is not a JSON object is left alone and reported.
    pub fn store(key: &str, value: Value) -> Result<PathBuf, String> {
        let path = Config::path().ok_or("no configuration directory available")?;
        store_in(&path, key, value)?;
        Ok(path)
    }

    /// Whether the preview is showing before anyone presses ^o.
    pub fn preview_starts_on(&self) -> bool {
        !self.preview.is_false()
    }

    /// Whether the popup is wide enough to split a preview column off it.
    pub fn fits_preview(&self, available_width: isize, chosen: bool) -> bool {
        if chosen || self.preview.is_true() || self.preview.is_false() {
            return available_width >= 60;
        }
        available_width >= 92
    }
}

fn store_in(path: &Path, key: &str, value: Value) -> Result<(), String> {
    let mut values = match std::fs::read(path) {
        Ok(data) => match String::from_utf8(data).ok().map(|text| pyjson::loads(&text)) {
            Some(Ok(Value::Dict(map))) => map,
            _ => return Err("config.json is not a JSON object; left unchanged".into()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(error) => return Err(format!("cannot read config.json: {}", error)),
    };
    values.insert(key, value);
    let text = format!("{}\n", pyjson::dumps_indent(&Value::Dict(values), 2));
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let result = std::fs::write(&temporary, &text).and_then(|()| std::fs::rename(&temporary, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    };
    write().map_err(|error| format!("cannot save config.json: {}", error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(json: &str) -> Config {
        Config::from_values(pyjson::loads(json).unwrap().as_dict().unwrap())
    }

    #[test]
    fn configuration_directory_follows_plugin_xdg_and_home_precedence() {
        use crate::testutil::{env_lock, EnvVars};
        let _lock = env_lock();
        let mut env = EnvVars::new();
        env.set("HOME", "/home/example").set("XDG_CONFIG_HOME", "/xdg").set("HERDR_PLUGIN_CONFIG_DIR", "/plugin");
        assert_eq!(config_dir(), Some(PathBuf::from("/plugin")));
        assert_eq!(Config::path(), Some(PathBuf::from("/plugin/config.json")));
        env.set("HERDR_PLUGIN_CONFIG_DIR", "");
        assert_eq!(config_dir(), Some(PathBuf::from("/xdg/herdr/plugins/config/hertrees")));
        env.set("XDG_CONFIG_HOME", "");
        assert_eq!(config_dir(), Some(PathBuf::from("/home/example/.config/herdr/plugins/config/hertrees")));
    }

    #[test]
    fn state_directory_follows_plugin_xdg_and_home_precedence() {
        use crate::testutil::{env_lock, EnvVars};
        let _lock = env_lock();
        let mut env = EnvVars::new();
        env.set("HOME", "/home/example").set("XDG_STATE_HOME", "/xdg").set("HERDR_PLUGIN_STATE_DIR", "/plugin");
        assert_eq!(state_dir(), Some(PathBuf::from("/plugin")));
        env.set("HERDR_PLUGIN_STATE_DIR", "");
        assert_eq!(state_dir(), Some(PathBuf::from("/xdg/herdr/plugins/hertrees")));
        env.set("XDG_STATE_HOME", "");
        assert_eq!(state_dir(), Some(PathBuf::from("/home/example/.local/state/herdr/plugins/hertrees")));
    }


    #[test]
    fn agents_and_scope_are_normalised() {
        let config = with(r#"{"agents": [" Claude ", "", 3, "codex"], "default_agent": "OMP", "scope": "merged"}"#);
        assert_eq!(config.agents, ["claude", "codex"]);
        assert_eq!(config.default_agent.as_deref(), Some("omp"));
        assert_eq!(config.scope, "merged");
        assert_eq!(with(r#"{"scope": "Merged"}"#).scope, "all");
    }

    #[test]
    fn wordmark_accepts_each_style_and_defaults_to_the_search_line() {
        let cases = [
            ("search", Style::Search), ("pixel", Style::Pixel), ("pixel-small", Style::PixelSmall),
            ("braille", Style::Braille), ("sextant", Style::Sextant), ("icon", Style::Icon), ("off", Style::Off),
            (" Pixel ", Style::Pixel),
        ];
        for (name, style) in cases {
            assert_eq!(with(&format!(r#"{{"wordmark": "{}"}}"#, name)).wordmark, style, "{name}");
        }
        assert_eq!(Config::default().wordmark, Style::Search);
        for json in [r#"{"wordmark": "border"}"#, r#"{"wordmark": ""}"#, r#"{"wordmark": 3}"#, r#"{"wordmark": null}"#] {
            assert_eq!(with(json).wordmark, Style::Search, "{json}");
        }
    }

    #[test]
    fn store_sets_one_key_and_keeps_the_rest_in_order() {
        let tmp = crate::testutil::TempDir::new();
        let path = tmp.path().join("nested/config.json");
        store_in(&path, "wordmark", Value::str("pixel")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\n  \"wordmark\": \"pixel\"\n}\n");
        std::fs::write(&path, r##"{"scope": "open", "wordmark": "icon", "colors": {"accent": "#fff"}}"##).unwrap();
        store_in(&path, "wordmark", Value::str("off")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let config = with(&text);
        assert_eq!((config.scope, config.wordmark), ("open", Style::Off));
        assert!(text.find("scope") < text.find("wordmark") && text.find("wordmark") < text.find("colors"), "{text}");
        for broken in ["{not json", "[1, 2]"] {
            std::fs::write(&path, broken).unwrap();
            assert!(store_in(&path, "wordmark", Value::str("pixel")).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        }
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temporary file is left");
    }
}
