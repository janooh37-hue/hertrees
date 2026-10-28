//! Prompts, agent launches and cleanup rules, shared by both multiplexers.
use crate::model::{AgentHere, PrInfo, PrState, Project, Worktree};
use crate::mux::{Mux, Started};

// Herdr's supported kinds, the most used first: the agent picker's default order.
const KINDS: &[&str] = &["claude", "codex", "omp", "opencode", "pi", "gemini", "cursor", "copilot", "amp", "grok", "kimi", "qwen", "devin", "agy", "cline", "mastracode", "kiro", "droid", "hermes", "kilo", "qodercli", "letta", "maki", "muse"];

/// Supported, executable agents in configuration order (or the order of `KINDS`).
pub fn installed_agents(preferred: &[String]) -> Vec<String> {
    let candidates: Vec<&str> = if preferred.is_empty() { KINDS.to_vec() } else { preferred.iter().map(String::as_str).collect() };
    let mut result = Vec::new();
    for kind in candidates {
        if !KINDS.contains(&kind) || result.iter().any(|s| s == kind) { continue; }
        let executable = if kind == "cursor" { "cursor-agent" } else { kind };
        if crate::opener::on_path(executable) {
            result.push(kind.to_owned());
        }
    }
    result
}

/// Placeholders hertrees fills itself, with what each becomes. Any other
/// `{name}` in a prompt is a field the user types a value for before it is sent.
pub const PLACEHOLDERS: &[(&str, &str)] = &[
    ("input", "your own text, where the preset puts it"),
    ("branch", "the worktree's branch"),
    ("base", "the branch it was made from"),
    ("path", "the worktree's directory"),
    ("repo", "the repository's name"),
    ("pr", "#number and URL, or none"),
    ("pr_title", "the pull request's title"),
    ("pr_body", "the pull request's description"),
    ("commits", "the branch's commits since base"),
    ("diffstat", "files changed since base"),
];

/// Filled only when a prompt uses them, at start time: they run git or gh.
const LAZY: &[&str] = &["pr_body", "commits", "diffstat"];

/// Placeholder values by name; names not set here stay as typed.
#[derive(Clone, Debug, Default)]
pub struct PromptVars {
    values: Vec<(String, String)>,
}

impl PromptVars {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.values.iter_mut().find(|(key, _)| key == name) {
            Some(slot) => slot.1 = value,
            None => self.values.push((name.to_string(), value)),
        }
    }
}

/// Shared picker/CLI variables; a new checkout keeps `{path}` until creation.
pub fn prompt_vars(project: &Project, wt: &Worktree, base: Option<&str>, info: Option<&PrInfo>) -> PromptVars {
    let pr = info.and_then(|info| info.pr.as_ref());
    let mut vars = PromptVars::default();
    vars.set("branch", wt.title());
    vars.set("base", base.or(project.default_branch.as_deref()).unwrap_or("HEAD"));
    vars.set("path", wt.path.clone());
    vars.set("repo", project.repo_name.clone());
    vars.set("pr", pr.map(|pr| format!("#{} {}", pr.number, pr.url)).unwrap_or_else(|| "none".into()));
    vars.set("pr_title", pr.map(|pr| pr.title.clone()).unwrap_or_else(|| "none".into()));
    vars
}

/// Fill the git/gh placeholders `prompt` uses. A checkout that does not
/// exist yet (a new worktree) has no commits, changes or PR of its own.
pub fn fill_lazy(vars: &mut PromptVars, prompt: &str, checkout: Option<&str>, pr_url: Option<&str>) {
    let used = names(prompt);
    let base = vars.get("base").unwrap_or("HEAD").to_string();
    let run = |argv: &[&str]| -> Option<String> {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let output = crate::proc::run(&argv, 15.0).ok().filter(|output| output.success)?;
        Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
    };
    for name in LAZY.iter().filter(|name| used.iter().any(|used| used == *name)) {
        let value = match (*name, checkout, pr_url) {
            ("commits", Some(path), _) => run(&["git", "-C", path, "log", "--oneline", "--no-decorate", "-n", "50", &format!("{base}..HEAD")]),
            ("diffstat", Some(path), _) => run(&["git", "-C", path, "diff", "--stat", &format!("{base}...HEAD")]),
            ("pr_body", _, Some(url)) => run(&["gh", "pr", "view", url, "--json", "body", "--jq", ".body"]),
            _ => Some(String::new()),
        };
        let value = match value {
            Some(text) if text.trim().is_empty() => "none".to_string(),
            Some(text) => text,
            None => "unavailable".to_string(),
        };
        vars.set(name, value);
    }
}

/// `{name}` tokens: a lowercase letter, then letters, digits or `_`. Braces
/// around anything else (JSON, code, `{ spaced }`) are plain text.
fn token(rest: &str) -> Option<&str> {
    let inner = rest.strip_prefix('{')?;
    let end = inner.find('}')?;
    let name = &inner[..end];
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    valid.then_some(name)
}

/// Every placeholder name in `template`, once each, in order of appearance.
pub fn names(template: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for (index, _) in template.match_indices('{') {
        if let Some(name) = token(&template[index..]) {
            if !found.iter().any(|known| known == name) {
                found.push(name.to_string());
            }
        }
    }
    found
}

/// The names in `template` hertrees cannot fill: the user types these.
pub fn fields(template: &str) -> Vec<String> {
    names(template).into_iter().filter(|name| !PLACEHOLDERS.iter().any(|(known, _)| known == name)).collect()
}

/// A preset plus the user's own text: at `{input}` when the preset has it,
/// else below the preset after a blank line.
pub fn compose(template: &str, addition: &str) -> String {
    let addition = addition.trim();
    if template.contains("{input}") {
        return template.replace("{input}", addition);
    }
    match (template.trim_end(), addition) {
        (template, "") => template.to_string(),
        ("", addition) => addition.to_string(),
        (template, addition) => format!("{template}\n\n{addition}"),
    }
}

/// Expand known placeholders once, without interpreting substituted text.
pub fn expand_prompt(template: &str, vars: &PromptVars) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        match token(rest).and_then(|name| Some((name.len() + 2, vars.get(name)?))) {
            Some((length, value)) => {
                out.push_str(value);
                rest = &rest[length..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn agent_name(kind: &str, branch: &str, taken: &[String]) -> String {
    let mut slug = String::with_capacity(32);
    for c in kind.chars().chain(std::iter::once('-')).chain(branch.chars()) {
        let c = c.to_ascii_lowercase();
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' { c } else { '-' };
        if c != '-' || !slug.ends_with('-') { slug.push(c); }
    }
    if !slug.starts_with(|c: char| c.is_ascii_lowercase()) { slug.insert_str(0, "agent-"); }
    slug.truncate(32);
    if !taken.contains(&slug) { return slug; }
    for n in 2u64.. {
        let suffix = format!("-{n}");
        let prefix = &slug[..slug.len().min(32 - suffix.len())];
        let candidate = format!("{prefix}{suffix}");
        if !taken.contains(&candidate) { return candidate; }
    }
    unreachable!()
}

pub fn validate_branch(repo_root: &str, name: &str) -> Result<(), String> {
    // @{-n} is accepted by git as a checkout shorthand, not a literal new name.
    if name.is_empty() || name.starts_with("@{-") { return Err("Enter a literal branch name".into()); }
    let args = ["git", "-C", repo_root, "check-ref-format", "--branch", name].map(str::to_owned);
    match crate::proc::run(&args, 5.0) {
        Ok(output) if output.success => Ok(()),
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("Invalid branch name").to_owned()),
        Err(crate::proc::RunError::Spawn(e)) => Err(format!("Cannot validate branch: {e}")),
        Err(crate::proc::RunError::Timeout) => Err("Branch validation timed out".into()),
    }
}

fn usable(wt: &Worktree) -> Result<(), String> {
    if wt.bare { Err("Cannot open a bare worktree".into()) } else if wt.prunable { Err("Worktree is missing or prunable".into()) } else { Ok(()) }
}

/// Refuse a first prompt the backend cannot deliver before anything is created.
fn check_prompt(mux: &dyn Mux, kind: &str, prompt: Option<&str>) -> Result<(), String> {
    if prompt.is_some_and(|prompt| !prompt.is_empty()) { mux.can_prompt(kind) } else { Ok(()) }
}

fn launch(mux: &mut dyn Mux, mut started: Started, branch: &str, kind: &str, prompt: Option<&str>, taken: &[String], progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    started.agent = agent_name(kind, branch, taken);
    progress(&format!("starting {kind}…"));
    mux.launch(started, kind, prompt.filter(|prompt| !prompt.is_empty()), progress)
}

pub fn start_existing(mux: &mut dyn Mux, project: &Project, wt: &Worktree, kind: &str, prompt: Option<&str>, taken_names: &[String], progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    check_prompt(mux, kind, prompt)?;
    let branch = wt.branch.as_deref().unwrap_or("detached");
    let started = open_tab(mux, project, wt, progress)?;
    launch(mux, started, branch, kind, prompt, taken_names, progress)
}

/// A fresh tab in the worktree's space, opening the space when it is closed:
/// its pane is a shell until an agent starts in it.
pub fn open_tab(mux: &mut dyn Mux, project: &Project, wt: &Worktree, progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    usable(wt)?;
    progress(&format!("opening {}…", wt.branch.as_deref().unwrap_or("detached")));
    mux.open_tab(project, wt)
}

/// `prompt` may still hold `{path}` until the new checkout has been created.
/// `label` names the new space; it is named after the branch without one.
#[allow(clippy::too_many_arguments)]
pub fn start_new(mux: &mut dyn Mux, project: &Project, branch: &str, base: Option<&str>, label: Option<&str>, kind: &str, prompt: Option<&str>, taken_names: &[String], progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    check_prompt(mux, kind, prompt)?;
    progress(&format!("opening {branch}…"));
    let started = create_shell(mux, project, branch, base, label)?;
    let prompt = prompt.map(|p| p.replace("{path}", &started.path));
    launch(mux, started, branch, kind, prompt.as_deref(), taken_names, progress)
}

/// New worktree with only its shell: no agent. `focus` afterwards shows it.
pub fn create_shell(mux: &mut dyn Mux, project: &Project, branch: &str, base: Option<&str>, label: Option<&str>) -> Result<Started, String> {
    validate_branch(&project.repo_root, branch)?;
    let path = crate::project::new_worktree_path(project, branch);
    let path = path.to_str().ok_or("Worktree path is not valid UTF-8")?;
    crate::project::ignore_worktrees(&project.repo_root)?;
    mux.create_worktree(project, branch, base, label.filter(|label| !label.trim().is_empty()), path)
}

/// Why a worktree cannot be cleaned up now, if anything stops it. `dirty` is
/// its count of uncommitted changes, None while unknown; `source_checkout` the
/// checkout the caller works from. The popup and the
/// `clean` command both refuse on this.
pub fn cleanup_blocker(wt: &Worktree, source_checkout: &str, agents: &[AgentHere], dirty: Option<u32>) -> Option<String> {
    if wt.is_main || wt.bare {
        return Some("the main checkout stays".into());
    }
    if wt.prunable {
        return Some("its folder is missing: git worktree prune clears it".into());
    }
    if source_checkout == wt.path {
        return Some("you are working from it".into());
    }
    if agents.iter().any(|agent| matches!(agent.status.as_str(), "working" | "blocked")) {
        return Some("an agent is still working in it".into());
    }
    // tmux cannot tell a busy agent from an idle one: any live agent keeps it.
    if agents.iter().any(|agent| agent.status == "running") {
        return Some("an agent is still running in it".into());
    }
    match dirty {
        None => Some("its git status is still loading".into()),
        Some(0) => None,
        Some(dirty) => Some(format!("it has {} uncommitted change{}", dirty, if dirty == 1 { "" } else { "s" })),
    }
}

/// The branch goes too only when GitHub merged it at exactly this commit:
/// anything committed since, or a PR that was closed, keeps the branch.
pub fn deletes_branch(wt: &Worktree, info: &PrInfo) -> bool {
    match (&info.pr, info.state) {
        (Some(pr), PrState::Merged) => !pr.head_oid.is_empty() && pr.head_oid == wt.head,
        _ => false,
    }
}

/// What `remove_worktree` did: a status line, and whether the branch went.
#[derive(Clone, Debug, PartialEq)]
pub struct Removed {
    pub message: String,
    pub branch_deleted: bool,
}

/// Clean up a finished worktree: close its space, remove the checkout, and
/// with `delete_branch` the local branch too. Never forced: the backend and
/// git both refuse a checkout with changes.
pub fn remove_worktree(mux: &mut dyn Mux, project: &Project, wt: &Worktree, delete_branch: bool) -> Result<Removed, String> {
    if wt.is_main || wt.bare {
        return Err("the main checkout is never removed".into());
    }
    mux.remove_checkout(project, wt)?;
    let title = wt.title();
    let (message, branch_deleted) = match (&wt.branch, delete_branch) {
        (Some(branch), true) => match git(&project.repo_root, &["branch", "-D", "--", branch]) {
            Ok(()) => (format!("removed {title} and its branch"), true),
            Err(error) => (format!("removed {title}; branch kept: {error}"), false),
        },
        _ => (format!("removed {title}"), false),
    };
    Ok(Removed { message, branch_deleted })
}

pub fn git(repo_root: &str, args: &[&str]) -> Result<(), String> {
    let mut argv = vec!["git".to_owned(), "-C".to_owned(), repo_root.to_owned()];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    match crate::proc::run(&argv, 30.0) {
        Ok(output) if output.success => Ok(()),
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("git failed").trim_start_matches("fatal: ").to_owned()),
        Err(crate::proc::RunError::Spawn(e)) => Err(format!("cannot run git: {e}")),
        Err(crate::proc::RunError::Timeout) => Err("git timed out".into()),
    }
}

pub fn jump(mux: &mut dyn Mux, project: &Project, wt: &Worktree) -> Result<(), String> {
    usable(wt)?;
    mux.jump(project, wt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_valid_and_unique() {
        assert_eq!(agent_name("Claude", "Feature/A//B", &[]), "claude-feature-a-b");
        assert_eq!(agent_name("codex", "123", &[]), "codex-123");
        assert_eq!(agent_name("123", "99", &[]), "agent-123-99");
        let name = agent_name("claude", &"A".repeat(80), &[]);
        assert_eq!(name.len(), 32);
        let second = agent_name("claude", &"A".repeat(80), &[name.clone()]);
        assert_eq!(second.len(), 32);
        assert!(second.ends_with("-2"));
        assert_eq!(agent_name("omp", "main", &["omp-main".into(), "omp-main-2".into()]), "omp-main-3");
    }
    #[test]
    fn names_fields_and_compose() {
        let template = "Fix {ticket} on {branch} ({ticket}). JSON {\"a\": 1} and { spaced } and {Upper} stay.";
        assert_eq!(names(template), ["ticket", "branch"]);
        assert_eq!(fields(template), ["ticket"]);
        assert_eq!(compose("Rules.\n", "  do it  "), "Rules.\n\ndo it");
        assert_eq!(compose("Rules.", ""), "Rules.");
        assert_eq!(compose("", "only mine"), "only mine");
        assert_eq!(compose("Task: {input}\nThen {input} again", " x "), "Task: x\nThen x again");
        assert_eq!(compose("Task: {input}", ""), "Task: ");
    }

    #[test]
    fn lazy_placeholders_run_only_when_used() {
        let mut vars = PromptVars::default();
        vars.set("base", "main");
        fill_lazy(&mut vars, "{branch} only", Some("/nonexistent"), None);
        assert_eq!(vars.get("commits"), None);
        fill_lazy(&mut vars, "{commits} {diffstat} {pr_body}", None, None);
        assert_eq!((vars.get("commits"), vars.get("diffstat"), vars.get("pr_body")), (Some("none"), Some("none"), Some("none")));
        fill_lazy(&mut vars, "{commits}", Some("/nonexistent/checkout"), None);
        assert_eq!(vars.get("commits"), Some("unavailable"));
    }

    #[test]
    fn removal_without_a_space_uses_git_and_never_forces() {
        let _guard = crate::testutil::env_lock();
        let temp = crate::testutil::TempDir::new();
        let repo = temp.path().join("repo");
        let repo = repo.to_str().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-C", repo]).args(args).status().unwrap();
            assert!(status.success(), "{args:?}");
        };
        std::fs::create_dir(repo).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        let linked = format!("{repo}/.worktrees/feat");
        git(&["worktree", "add", "-q", "-b", "feat", &linked]);
        let project = Project { repo_root: repo.into(), ..Project::default() };
        let wt = Worktree { path: linked.clone(), branch: Some("feat".into()), ..Worktree::default() };
        let mut client = crate::client::HerdrClient::new(Some("/nonexistent/hertrees-test".into()), 0.001, true);

        std::fs::write(format!("{linked}/notes"), "keep me").unwrap();
        assert!(remove_worktree(&mut client, &project, &wt, true).unwrap_err().contains("untracked"));
        assert!(std::path::Path::new(&linked).join("notes").exists());
        std::fs::remove_file(format!("{linked}/notes")).unwrap();

        assert_eq!(remove_worktree(&mut client, &project, &wt, true).unwrap(), Removed { message: "removed feat and its branch".into(), branch_deleted: true });
        assert!(!std::path::Path::new(&linked).exists());
        let branches = std::process::Command::new("git").args(["-C", repo, "branch", "--list", "feat"]).output().unwrap();
        assert!(branches.stdout.is_empty());
        let main = Worktree { path: repo.into(), branch: Some("main".into()), is_main: true, ..Worktree::default() };
        assert!(remove_worktree(&mut client, &project, &main, true).is_err());
    }

    #[test]
    fn prompts_preserve_unknown_and_do_not_reexpand() {
        let mut vars = PromptVars::default();
        for (name, value) in [("branch", "feature/{base}"), ("base", "main"), ("path", "/tmp/repo"), ("repo", "repo"), ("pr", "none")] {
            vars.set(name, value);
        }
        assert_eq!(expand_prompt("{branch} {base} {path} {repo} {pr} {unknown} {", &vars), "feature/{base} main /tmp/repo repo none {unknown} {");
        assert_eq!(expand_prompt("", &vars), "");
    }
}
