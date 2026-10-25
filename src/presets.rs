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

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub prompt: Option<String>,
    pub path: Option<PathBuf>,
}

pub fn dir() -> Option<PathBuf> {
    config::config_dir().map(|directory| directory.join("presets"))
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
        let name = stem.to_string();
        let content = fs::read(&path).ok()?;
        Some(Preset { name, prompt: Some(String::from_utf8_lossy(&content).trim().to_string()), path: Some(path) })
    }).collect();
    presets.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    presets
}

pub fn load(dir: Option<&Path>) -> Vec<Preset> {
    let mut presets = vec![Preset { name: BLANK.into(), prompt: Some(String::new()), path: None }];
    if let Some(dir) = dir {
        let _ = seed(dir);
        presets.extend(files(dir));
    }
    presets.push(Preset { name: CUSTOM.into(), prompt: None, path: None });
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
        assert_eq!(load(Some(&dir)).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["blank", "plan", "research", "custom"]);
        delete(&dir, "plan").unwrap();
        assert_eq!(load(Some(&dir)).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["blank", "research", "custom"]);
        let existing = tmp.path().join("existing");
        fs::create_dir(&existing).unwrap();
        seed(&existing).unwrap();
        assert!(files(&existing).is_empty());
    }

    #[test]
    fn files_are_sorted_valid_markdown_and_read_lossily() {
        let tmp = TempDir::new();
        for name in ["z", "beta", "Beta", "Alpha", "BLANK", "Custom", ".hidden", " spaced "] {
            fs::write(path(tmp.path(), name), b" \nfirst\n\nsecond\xff \n").unwrap();
        }
        fs::create_dir(path(tmp.path(), "directory")).unwrap();
        fs::write(tmp.path().join("other.txt"), "ignored").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(path(tmp.path(), "z"), path(tmp.path(), "linked")).unwrap();
        let presets = files(tmp.path());
        let names: Vec<_> = presets.iter().map(|p| p.name.as_str()).collect();
        #[cfg(unix)]
        assert_eq!(names, [" spaced ", "Alpha", "Beta", "beta", "linked", "z"]);
        #[cfg(not(unix))]
        assert_eq!(names, [" spaced ", "Alpha", "Beta", "beta", "z"]);
        assert_eq!(presets[0].prompt.as_deref(), Some("first\n\nsecond�"));
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
