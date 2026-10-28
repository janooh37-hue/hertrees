//! Optional Herdr Agent Icons Max vendor marks, compatible with herdr-radar,
//! and Nerd Font marks for the rows the font has none for.
//!
//! Codepoints (font 1.8.0, herdr-radar v1.5.0):
//! https://github.com/hhdebb/herdr-radar/blob/v1.5.0/tools/codepoints.toml
//! The popup never installs fonts or changes terminal configuration.

use crate::pyjson::Value;
use crate::pyos;
use crate::pystr;
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread::JoinHandle;

pub const FAMILY: &str = "Herdr Agent Icons Max";
pub const VENDORS: &[&str] = &[
    "claude", "codex", "opencode", "omp", "cline", "mastracode", "kimi", "kilo", "maki", "pi",
    "hermes", "cursor", "copilot", "deepseek", "gemini", "gpt", "qwen", "grok", "agy", "kiro",
    "amp", "devin", "qodercli", "glm", "kimchi", "muse", "crush",
];

pub fn logo(index: usize) -> char {
    char::from_u32(0xE1A0 + index as u32).unwrap()
}

thread_local! {
    static FONT: Cell<Option<bool>> = const { Cell::new(None) };
}

fn env_or(name: &str) -> Option<String> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(|v| v.to_string_lossy().into_owned())
}

fn has_font_file(directory: &PathBuf) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else { return false };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        #[cfg(windows)]
        let name = name.to_lowercase();
        #[cfg(windows)]
        let (prefix, suffix) = ("herdragenticonsmax", ".ttf");
        #[cfg(not(windows))]
        let (prefix, suffix) = ("HerdrAgentIconsMax", ".ttf");
        if name.len() >= prefix.len() + suffix.len() && name.starts_with(prefix) && name.ends_with(suffix) {
            return true;
        }
    }
    false
}

/// The user and system font folders this platform installs fonts into.
fn font_dirs() -> Vec<PathBuf> {
    let home = PathBuf::from(pyos::path_home());
    if cfg!(target_os = "macos") {
        vec![home.join("Library/Fonts"), PathBuf::from("/Library/Fonts")]
    } else if cfg!(windows) {
        let local = env_or("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData/Local"));
        let system = env_or("SYSTEMROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:/Windows"));
        vec![local.join("Microsoft/Windows/Fonts"), system.join("Fonts")]
    } else {
        let data = env_or("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
        vec![data.join("fonts"), home.join(".fonts")]
    }
}

fn detect() -> bool {
    if font_dirs().iter().any(has_font_file) {
        return true;
    }
    if cfg!(target_os = "linux") {
        if let Some(output) = crate::proc::run_capture(
            &["fc-match".into(), "--format".into(), "%{family}".into(), FAMILY.into()],
            1.0,
        ) {
            let text = String::from_utf8_lossy(&output.stdout);
            let families: Vec<&str> = text.split(',').map(pystr::strip).collect();
            return output.success && families.contains(&FAMILY);
        }
    }
    false
}

/// A detection already running on another thread, for the next caller.
static PREFETCH: Mutex<Option<JoinHandle<bool>>> = Mutex::new(None);

/// Start detecting now, while the caller does other startup work.
///
/// `fc-match` costs several milliseconds and nothing it reads depends on
/// what happens meanwhile, so the popup overlaps it with the snapshot fetch.
/// The answer is the same one `font_available` would have computed.
pub fn prefetch() {
    let handle = std::thread::spawn(detect);
    if let Ok(mut slot) = PREFETCH.lock() {
        *slot = Some(handle);
    }
}

fn prefetched() -> Option<bool> {
    let handle = PREFETCH.lock().ok()?.take()?;
    handle.join().ok()
}

/// Best-effort local detection, once per popup; remote fonts need opt-in.
pub fn font_available() -> bool {
    FONT.with(|f| {
        if let Some(known) = f.get() {
            return known;
        }
        let found = prefetched().unwrap_or_else(detect);
        f.set(Some(found));
        found
    })
}

pub fn enabled(mode: &Value) -> bool {
    match mode.as_str() {
        Some("font") => true,
        Some("auto") => font_available(),
        _ => false,
    }
}

/// Glyphs before worktree, space and tab rows, so the three levels read apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreeIcons {
    pub worktree: &'static str,
    pub space: &'static str,
    pub tab: &'static str,
}

/// Nerd Font: git-branch, window, tab.
pub const NERD: TreeIcons = TreeIcons { worktree: "\u{f418}", space: "\u{eb7f}", tab: "\u{f04e9}" };
pub const UNICODE: TreeIcons = TreeIcons { worktree: "⎇", space: "◫", tab: "▭" };

/// Values of the `icons` setting, in the order documented.
pub const TREE_MODES: &[&str] = &["auto", "nerd", "unicode", "off"];

/// A Nerd Font covers the branch glyph: fontconfig knows, else a font file
/// named like one. Detected where the popup runs, like the agent logos.
fn detect_nerd() -> bool {
    if cfg!(target_os = "linux") {
        let argv = ["fc-list".to_string(), ":charset=f418".to_string(), "family".to_string()];
        if let Some(output) = crate::proc::run_capture(&argv, 1.0) {
            return output.success && !pystr::strip(&String::from_utf8_lossy(&output.stdout)).is_empty();
        }
    }
    font_dirs().iter().any(|directory| {
        std::fs::read_dir(directory).map(|entries| entries.flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.contains("NerdFont") || name.contains("Nerd Font")
        })).unwrap_or(false)
    })
}

static NERD_PREFETCH: Mutex<Option<JoinHandle<bool>>> = Mutex::new(None);

/// Start the Nerd Font check while the popup loads the project.
pub fn prefetch_nerd() {
    let handle = std::thread::spawn(detect_nerd);
    if let Ok(mut slot) = NERD_PREFETCH.lock() {
        *slot = Some(handle);
    }
}

/// `icons`: "auto" (default) uses Nerd Font glyphs when one is installed and
/// plain Unicode otherwise; "nerd" and "unicode" force a set; "off" draws none.
pub fn tree_icons(mode: Option<&str>) -> Option<TreeIcons> {
    match mode.unwrap_or("auto") {
        "off" => None,
        "nerd" => Some(NERD),
        "unicode" => Some(UNICODE),
        _ => {
            let prefetched = NERD_PREFETCH.lock().ok().and_then(|mut slot| slot.take()).and_then(|handle| handle.join().ok());
            Some(if prefetched.unwrap_or_else(detect_nerd) { NERD } else { UNICODE })
        }
    }
}

/// The agent kind as the icon and color tables know it.
pub fn agent_kind(agent: &str) -> String {
    let lowered = pystr::lower(pystr::strip(agent));
    let mut kind = lowered.split(':').next().unwrap_or("").to_string();
    if kind.ends_with(" code") {
        kind.truncate(kind.len() - 5);
    }
    kind
}

pub fn logo_for(agent: Option<&str>) -> Option<char> {
    let kind = agent_kind(agent.unwrap_or(""));
    let kind = match kind.as_str() {
        "cursor-agent" => "cursor",
        "open-code" => "opencode",
        other => other,
    };
    VENDORS.iter().position(|v| *v == kind).map(logo)
}

/// Nerd Font: terminal, duck (yazi's mascot), robot.
const NERD_SHELL: char = '\u{f489}';
const NERD_YAZI: char = '\u{f01e5}';
const NERD_AGENT: char = '\u{f06a9}';

/// The mark before an agent picker or agent row: the vendor logo when the
/// font has one, else (with a Nerd Font) a terminal for `shell`, a duck for
/// `yazi` and a robot for any other agent, so new kinds line up with the rest.
pub fn mark_for(agent: Option<&str>, nerd: bool) -> Option<char> {
    let agent = agent.filter(|a| !a.trim().is_empty())?;
    logo_for(Some(agent)).or_else(|| {
        nerd.then(|| match agent_kind(agent).as_str() {
            "shell" => NERD_SHELL,
            "yazi" => NERD_YAZI,
            _ => NERD_AGENT,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::Value;

    fn s(text: &str) -> Value {
        Value::Str(text.into())
    }

    #[test]
    fn auto_falls_back_without_font() {
        // The detection result is cached per thread; seed it instead of
        // probing this machine's fonts.
        FONT.with(|f| f.set(Some(false)));
        assert!(!enabled(&s("auto")));
        assert!(enabled(&s("font")));
        FONT.with(|f| f.set(Some(true)));
        assert!(enabled(&s("auto")));
        assert!(!enabled(&s("none")));
        assert!(!enabled(&s("invalid")));
        FONT.with(|f| f.set(None));
    }

    #[test]
    fn tree_icon_modes() {
        assert_eq!(tree_icons(Some("off")), None);
        assert_eq!(tree_icons(Some("nerd")), Some(NERD));
        assert_eq!(tree_icons(Some("unicode")), Some(UNICODE));
        for icons in [NERD, UNICODE] {
            for glyph in [icons.worktree, icons.space, icons.tab] {
                assert_eq!(crate::textutil::display_width(glyph), 1, "{glyph:?}");
            }
        }
    }

    #[test]
    fn aliases_and_unknown_agents() {
        assert_eq!(logo_for(Some("Codex")), Some('\u{e1a1}'));
        assert_eq!(logo_for(Some("Claude Code")), Some('\u{e1a0}'));
        assert_eq!(logo_for(Some("cursor-agent")), logo_for(Some("cursor")));
        assert_eq!(logo_for(Some("open-code")), logo_for(Some("opencode")));
        assert_eq!(logo_for(Some("codex:model")), logo_for(Some("codex")));
        assert_eq!(logo_for(Some("unrecognised")), None);
        assert_eq!(logo_for(None), None);
        assert_eq!(logo_for(Some("muse")), Some('\u{e1b9}'));
        assert_eq!(logo_for(Some("crush")), Some('\u{e1ba}'));
    }

    #[test]
    fn nerd_marks_fill_in_where_the_font_has_no_logo() {
        assert_eq!(mark_for(Some("claude"), true), logo_for(Some("claude")), "a vendor logo wins");
        assert_eq!(mark_for(Some("shell"), true), Some(NERD_SHELL));
        assert_eq!(mark_for(Some("yazi"), true), Some(NERD_YAZI));
        assert_eq!(mark_for(Some("droid"), true), Some(NERD_AGENT), "kinds without a logo get the robot");
        assert_eq!(mark_for(Some("yazi"), false), None, "no Nerd Font, no fallback");
        assert_eq!(mark_for(None, true), None);
        for mark in [NERD_SHELL, NERD_YAZI, NERD_AGENT] {
            assert_eq!(crate::textutil::display_width(&mark.to_string()), 1, "{mark:?}");
        }
    }
}
