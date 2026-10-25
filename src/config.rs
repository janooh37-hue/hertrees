//! User configuration.
//!
//! Read from `$HERDR_PLUGIN_CONFIG_DIR/config.json`. Unknown keys are ignored,
//! and a broken file falls back to defaults instead of taking the picker down.
//! The display keys (`preview`, `mouse`, `agent_icons`, `refresh_ms`, `colors`,
//! `selection_background`) mean exactly what they mean in herdr-bar.

use crate::pyjson::{self, Map, Value};
use crate::pystr;
use std::path::PathBuf;

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
    fn agents_and_scope_are_normalised() {
        let config = with(r#"{"agents": [" Claude ", "", 3, "codex"], "default_agent": "OMP", "scope": "merged"}"#);
        assert_eq!(config.agents, ["claude", "codex"]);
        assert_eq!(config.default_agent.as_deref(), Some("omp"));
        assert_eq!(config.scope, "merged");
        assert_eq!(with(r#"{"scope": "Merged"}"#).scope, "all");
    }
}
