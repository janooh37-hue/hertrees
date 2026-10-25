//! User-owned Markdown prompt templates, plus the two built-in picker choices.

use crate::config;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BLANK: &str = "blank";
pub const CUSTOM: &str = "custom";

const PLAN_PROMPT: &str = "You are in the git worktree {path} (repo {repo}, branch {branch}, base {base}, PR {pr}).\n\nStudy the code and this branch's changes against {base}, then write a step-by-step implementation plan to PLAN.md: goal, steps, files to touch, risks, and how to verify.\nDo not change code yet; wait for my review.\n";
const RESEARCH_PROMPT: &str = "You are in the git worktree {path} (repo {repo}, branch {branch}, base {base}, PR {pr}).\n\nResearch the codebase for this branch's work: map the relevant modules and data flow, note constraints and open questions, and write the findings to RESEARCH.md with file:line references.\nDo not change code.\n";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preset {
    pub name: String,
    pub prompt: Option<String>,
    pub path: Option<PathBuf>,
    /// `ask: true`: ⏎ opens the prompt editor under this preset instead of sending it.
    pub ask: bool,
    /// `agent: KIND`: the agent selected first while this preset is chosen.
    pub agent: Option<String>,
    pub description: Option<String>,
    /// From the repository's `.hertrees/presets/`, not the user's own directory.
    pub repo: bool,
}

pub fn dir() -> Option<PathBuf> {
    config::config_dir().map(|directory| directory.join("presets"))
}

/// A repository's shared presets, committed with it.
pub fn repo_dir(repo_root: &str) -> PathBuf {
    Path::new(repo_root).join(".hertrees").join("presets")
}

/// Split an optional settings block off the prompt:
///
/// ```text
/// ---
/// ask: true
/// agent: codex
/// description: one line for the picker
/// ---
/// prompt…
/// ```
///
/// Unknown keys are ignored; without a closing `---` the whole file is the prompt.
fn parse(content: &str, preset: &mut Preset) -> String {
    let content = content.trim_start_matches('\u{feff}');
    let mut lines = content.split_inclusive('\n');
    if lines.next().map(str::trim) == Some("---") {
        let mut offset = content.find('\n').map(|i| i + 1).unwrap_or(content.len());
        let mut settings = Vec::new();
        for line in lines {
            offset += line.len();
            if line.trim() == "---" {
                for (key, value) in settings {
                    match key {
                        "ask" => preset.ask = matches!(value, "true" | "yes" | "on"),
                        "agent" => preset.agent = Some(value.to_ascii_lowercase()).filter(|v| !v.is_empty()),
                        "description" => preset.description = Some(value.to_string()).filter(|v| !v.is_empty()),
                        _ => {}
                    }
                }
                return content[offset..].trim().to_string();
            }
            if let Some((key, value)) = line.split_once(':') {
                settings.push((key.trim(), value.trim()));
            }
        }
    }
    content.trim().to_string()
}

/// Seed once, not on every load: removing either default is a lasting choice.
pub fn seed(dir: &Path) -> Result<(), String> {
    if fs::symlink_metadata(dir).is_ok() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    create(dir, "plan", PLAN_PROMPT)?;
    create(dir, "research", RESEARCH_PROMPT)?;
    Ok(())
}

pub fn files(dir: &Path) -> Vec<Preset> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut presets: Vec<_> = entries.flatten().filter_map(|entry| {
        let path = entry.path();
        if path.extension()?.to_str()? != "md" || !path.is_file() {
            return None;
        }
        let stem = path.file_stem()?.to_str()?;
        check_name(stem).ok()?;
        let mut preset = Preset { name: stem.to_string(), ..Preset::default() };
        let content = fs::read(&path).ok()?;
        preset.prompt = Some(parse(&String::from_utf8_lossy(&content), &mut preset));
        preset.path = Some(path);
        Some(preset)
    }).collect();
    presets.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    presets
}

/// Blank, the user's presets and the repository's (the user's win a name
/// clash), then Custom.
pub fn load(dir: Option<&Path>, repo_root: Option<&str>) -> Vec<Preset> {
    let mut found = Vec::new();
    if let Some(dir) = dir {
        let _ = seed(dir);
        found = files(dir);
    }
    if let Some(root) = repo_root.filter(|root| !root.is_empty()) {
        for mut preset in files(&repo_dir(root)) {
            if !found.iter().any(|own: &Preset| own.name.eq_ignore_ascii_case(&preset.name)) {
                preset.repo = true;
                found.push(preset);
            }
        }
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    let mut presets = vec![Preset { name: BLANK.into(), prompt: Some(String::new()), ..Preset::default() }];
    presets.extend(found);
    presets.push(Preset { name: CUSTOM.into(), ..Preset::default() });
    presets
}

pub fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a preset needs a name".into());
    }
    if name.chars().count() > 40 {
        return Err("preset names must be at most 40 characters".into());
    }
    if name.contains('/') || name.contains('\0') || name.starts_with('.') {
        return Err("preset names cannot start with . or contain / or NUL".into());
    }
    if name.eq_ignore_ascii_case(BLANK) || name.eq_ignore_ascii_case(CUSTOM) {
        return Err(format!("“{}” is a built-in preset", name));
    }
    Ok(name.to_string())
}

pub fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.md", name))
}

fn collision(name: &str) -> String {
    format!("a preset named “{}” already exists", name)
}

pub fn create(dir: &Path, name: &str, prompt: &str) -> Result<PathBuf, String> {
    let name = check_name(name)?;
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let path = path(dir, &name);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists { collision(&name) } else { error.to_string() }
    })?;
    if let Err(error) = file.write_all(prompt.as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(error.to_string());
    }
    Ok(path)
}

pub fn rename(dir: &Path, old: &str, new: &str) -> Result<PathBuf, String> {
    check_name(old)?;
    let new = check_name(new)?;
    let source = path(dir, &old);
    let target = path(dir, &new);
    if old != new && old.eq_ignore_ascii_case(&new) {
        fs::rename(&source, &target).map_err(|error| error.to_string())?;
        return Ok(target);
    }
    // Linking reserves the new name atomically, including against dangling symlinks.
    match fs::hard_link(&source, &target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Err(collision(&new)),
        Err(_) => {
            rename_without_link(&source, &target, &new)?;
            return Ok(target);
        }
    }
    if let Err(error) = fs::remove_file(&source) {
        let _ = fs::remove_file(&target);
        return Err(error.to_string());
    }
    Ok(target)
}

fn rename_without_link(source: &Path, target: &Path, name: &str) -> Result<(), String> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(collision(name));
    }
    fs::rename(source, target).map_err(|error| error.to_string())
}

pub fn delete(dir: &Path, name: &str) -> Result<(), String> {
    check_name(name)?;
    fs::remove_file(path(dir, name)).map_err(|error| error.to_string())
}

const HISTORY_LIMIT: usize = 50;

fn history_path() -> Option<PathBuf> {
    config::state_dir().map(|dir| dir.join("prompt-history.json"))
}

/// Texts typed in the popup's prompt editor, oldest first.
pub fn history() -> Vec<String> {
    let Some(text) = history_path().and_then(|path| fs::read_to_string(path).ok()) else { return Vec::new() };
    match crate::pyjson::loads(&text) {
        Ok(crate::pyjson::Value::List(items)) => items.iter().filter_map(|item| item.as_str()).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// Keep `text` as the newest history entry: once, at most `HISTORY_LIMIT` kept.
pub fn remember(text: &str) {
    let text = text.trim();
    let Some(path) = history_path().filter(|_| !text.is_empty()) else { return };
    let mut entries = history();
    entries.retain(|entry| entry != text);
    entries.push(text.to_string());
    let skip = entries.len().saturating_sub(HISTORY_LIMIT);
    let list = crate::pyjson::Value::List(entries.into_iter().skip(skip).map(crate::pyjson::Value::Str).collect());
    let Some(parent) = path.parent() else { return };
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let written = fs::create_dir_all(parent)
        .and_then(|()| fs::write(&temporary, crate::pyjson::dumps(&list, false)))
        .and_then(|()| fs::rename(&temporary, &path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
}

pub fn open_editor(path: &Path) -> Result<(), String> {
    let editor = ["VISUAL", "EDITOR"].iter().find_map(|key| std::env::var(key).ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| {
            let nvim = std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).any(|dir| {
                let file = dir.join("nvim");
                crate::opener::is_executable_file(&file)
            })).unwrap_or(false);
            if nvim { "nvim".into() } else { "vi".into() }
        });
    let status = Command::new("sh").arg("-c").arg(format!("{} \"$1\"", editor)).arg("sh").arg(path)
        .status().map_err(|error| error.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("editor exited with {}", status)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn defaults_are_seeded_only_for_a_missing_directory() {
        let tmp = TempDir::new();
        let dir = tmp.path().join("presets");
        assert_eq!(load(Some(&dir), None).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["blank", "plan", "research", "custom"]);
        delete(&dir, "plan").unwrap();
        assert_eq!(load(Some(&dir), None).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["blank", "research", "custom"]);
        let existing = tmp.path().join("existing");
        fs::create_dir(&existing).unwrap();
        seed(&existing).unwrap();
        assert!(files(&existing).is_empty());
    }

    #[test]
    fn files_are_sorted_valid_markdown_and_read_lossily() {
        let tmp = TempDir::new();
        // Distinct letters, not case variants: macOS file systems are case-insensitive.
        for name in ["z", "bravo", "Beta", "Alpha", "BLANK", "Custom", ".hidden", " spaced "] {
            fs::write(path(tmp.path(), name), b" \nfirst\n\nsecond\xff \n").unwrap();
        }
        fs::create_dir(path(tmp.path(), "directory")).unwrap();
        fs::write(tmp.path().join("other.txt"), "ignored").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(path(tmp.path(), "z"), path(tmp.path(), "linked")).unwrap();
        let presets = files(tmp.path());
        let names: Vec<_> = presets.iter().map(|p| p.name.as_str()).collect();
        #[cfg(unix)]
        assert_eq!(names, [" spaced ", "Alpha", "Beta", "bravo", "linked", "z"]);
        #[cfg(not(unix))]
        assert_eq!(names, [" spaced ", "Alpha", "Beta", "bravo", "z"]);
        assert_eq!(presets[0].prompt.as_deref(), Some("first\n\nsecond�"));
    }

    #[test]
    fn a_settings_block_sets_ask_agent_and_description() {
        let tmp = TempDir::new();
        fs::write(path(tmp.path(), "rules"), "---\nask: true\nagent: Codex\ndescription: house rules\nother: x\n---\n\nFollow the rules.\n").unwrap();
        fs::write(path(tmp.path(), "plain"), "---\nnot closed\nstill prompt").unwrap();
        fs::write(path(tmp.path(), "quiet"), "---\nask: no\n---\nhello").unwrap();
        let presets = files(tmp.path());
        let rules = presets.iter().find(|p| p.name == "rules").unwrap();
        assert_eq!((rules.ask, rules.agent.as_deref(), rules.description.as_deref()), (true, Some("codex"), Some("house rules")));
        assert_eq!(rules.prompt.as_deref(), Some("Follow the rules."));
        let plain = presets.iter().find(|p| p.name == "plain").unwrap();
        assert_eq!((plain.ask, plain.prompt.as_deref()), (false, Some("---\nnot closed\nstill prompt")));
        let quiet = presets.iter().find(|p| p.name == "quiet").unwrap();
        assert_eq!((quiet.ask, quiet.prompt.as_deref()), (false, Some("hello")));
    }

    #[test]
    fn repository_presets_join_the_list_and_lose_name_clashes() {
        let tmp = TempDir::new();
        let own = tmp.path().join("own");
        fs::create_dir(&own).unwrap();
        create(&own, "review", "mine").unwrap();
        let root = tmp.path().join("repo");
        create(&repo_dir(root.to_str().unwrap()), "Review", "theirs").unwrap();
        create(&repo_dir(root.to_str().unwrap()), "conventions", "shared").unwrap();
        let presets = load(Some(&own), root.to_str());
        let summary: Vec<_> = presets.iter().map(|p| (p.name.as_str(), p.prompt.as_deref(), p.repo)).collect();
        assert_eq!(summary, [("blank", Some(""), false), ("conventions", Some("shared"), true), ("review", Some("mine"), false), ("custom", None, false)]);
    }

    #[test]
    fn names_are_trimmed_bounded_and_not_reserved() {
        assert_eq!(check_name("  review notes  ").unwrap(), "review notes");
        assert!(check_name(&"界".repeat(40)).is_ok());
        for name in ["".into(), " ".into(), "a/b".into(), "a\0b".into(), ".hidden".into(), "Blank".into(), "CUSTOM".into(), "界".repeat(41)] {
            assert!(check_name(&name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn create_and_rename_never_overwrite_existing_files() {
        let tmp = TempDir::new();
        let dir = tmp.path().join("nested/presets");
        let old = create(&dir, " old ", "first\nsecond").unwrap();
        assert_eq!(create(&dir, "old", "replacement").unwrap_err(), "a preset named “old” already exists");
        create(&dir, "taken", "keep").unwrap();
        assert!(rename(&dir, "old", "taken").is_err());
        assert_eq!(fs::read_to_string(&old).unwrap(), "first\nsecond");
        assert_eq!(fs::read_to_string(path(&dir, "taken")).unwrap(), "keep");
        let renamed = rename(&dir, "old", "new").unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(renamed).unwrap(), "first\nsecond");
        delete(&dir, "new").unwrap();
        assert!(!path(&dir, "new").exists());
    }

    #[test]
    fn case_only_rename_changes_the_name() {
        let tmp = TempDir::new();
        create(tmp.path(), "notes", "keep").unwrap();
        let renamed = rename(tmp.path(), "notes", "Notes").unwrap();
        assert_eq!(renamed, path(tmp.path(), "Notes"));
        assert_eq!(fs::read_to_string(&renamed).unwrap(), "keep");
        let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, [std::ffi::OsString::from("Notes.md")]);
    }

    #[test]
    fn fallback_rename_refuses_existing_targets() {
        let tmp = TempDir::new();
        let source = create(tmp.path(), "source", "original").unwrap();
        let target = create(tmp.path(), "target", "keep").unwrap();
        assert_eq!(rename_without_link(&source, &target, "target").unwrap_err(), collision("target"));
        assert_eq!(fs::read_to_string(&source).unwrap(), "original");
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        fs::remove_file(&target).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(tmp.path().join("missing"), &target).unwrap();
            assert_eq!(rename_without_link(&source, &target, "target").unwrap_err(), collision("target"));
            fs::remove_file(&target).unwrap();
        }
        rename_without_link(&source, &target, "target").unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    }
}
