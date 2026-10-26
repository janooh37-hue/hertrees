//! Agent launches and checkout navigation through herdr.
use crate::client::{dict, HerdrClient};
use crate::model::{AgentHere, PrInfo, PrState, Project, Worktree};
use crate::pyjson::{Map, Value};
use std::time::{Duration, Instant};

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

#[derive(Clone, Debug)]
pub struct Started { pub workspace_id: String, pub tab_id: String, pub pane_id: String, pub agent: String, pub path: String, pub prompt_deferred: bool }

fn call(client: &mut HerdrClient, method: &str, params: Value, argv: &[&str]) -> Result<Map, String> {
    client.call(method, params, argv).map_err(|e| format!("{method}: {e}"))
}
fn id(result: &Map, object: &str, key: &str) -> Result<String, String> {
    result.get(object).and_then(|v| v.get(key).as_str()).filter(|s| !s.is_empty()).map(str::to_owned)
        .ok_or_else(|| format!("herdr response missing {object}.{key}"))
}
fn location(result: &Map, path: &str) -> Result<Started, String> {
    Ok(Started { workspace_id: id(result, "workspace", "workspace_id")?, tab_id: id(result, "tab", "tab_id")?, pane_id: id(result, "root_pane", "pane_id")?, agent: String::new(), path: path.into(), prompt_deferred: false })
}
fn launch(client: &mut HerdrClient, mut started: Started, branch: &str, kind: &str, prompt: Option<&str>, taken: &[String], progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    started.agent = agent_name(kind, branch, taken);
    progress(&format!("starting {kind}…"));
    let old_timeout = client.timeout;
    client.timeout = 60.0;
    let result = call(client, "agent.start", dict(&[("name", Value::str(&started.agent)), ("kind", Value::str(kind)), ("pane_id", Value::str(&started.pane_id)), ("timeout_ms", Value::Int(45000))]), &["agent", "start", &started.agent, "--kind", kind, "--pane", &started.pane_id, "--timeout", "45000"]);
    client.timeout = old_timeout;
    match result {
        Ok(_) => {}
        // The agent runs but stopped at a startup question (folder trust, login,
        // update notice): it keeps its name, and the prompt waits for it below.
        Err(error) if error.contains("blocked during startup") || error.contains("not ready") => {}
        Err(error) => return Err(error),
    }
    if let Some(prompt) = prompt.filter(|p| !p.is_empty()) {
        match wait_ready(client, &started.agent, Duration::from_secs(8)) {
            Readiness::Ready => {
                progress("sending prompt…");
                send_prompt(client, &started.agent, prompt)?;
            }
            Readiness::Gone => return Err(format!("{kind} exited before it took the prompt")),
            // Blocked at startup (e.g. Claude's folder-trust question) or slow:
            // a detached sender delivers it once the user has answered.
            Readiness::Blocked | Readiness::NotYet => {
                defer_prompt(&started.agent, prompt)?;
                started.prompt_deferred = true;
            }
        }
    }
    Ok(started)
}

/// Submit and confirm the agent picked it up: herdr reports `agent_prompt_stalled`
/// when no working state follows within 5s, instead of a silent loss.
fn send_prompt(client: &mut HerdrClient, agent: &str, prompt: &str) -> Result<(), String> {
    let old_timeout = client.timeout;
    client.timeout = 20.0;
    let wait = dict(&[("until", Value::List(vec![Value::str("working")])), ("timeout_ms", Value::Int(10_000))]);
    let result = call(
        client,
        "agent.prompt",
        dict(&[("target", Value::str(agent)), ("text", Value::str(prompt)), ("wait", wait)]),
        &["agent", "prompt", agent, "--wait", "--until", "working", "--timeout", "10000", "--", prompt],
    );
    client.timeout = old_timeout;
    result.map(|_| ())
}

/// Prompt an agent that is already running. An idle agent gets it now; a busy
/// one, or one waiting on a question, gets it from the detached sender once it
/// is idle again. Returns whether delivery was deferred.
pub fn send_to(client: &mut HerdrClient, agent: &str, prompt: &str) -> Result<bool, String> {
    match wait_ready(client, agent, Duration::from_secs(1)) {
        Readiness::Ready => send_prompt(client, agent, prompt).map(|()| false),
        Readiness::Gone => Err("that agent is no longer running".into()),
        Readiness::Blocked | Readiness::NotYet => defer_prompt(agent, prompt).map(|()| true),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Readiness {
    /// Idle at its input, startup finished: a prompt will land.
    Ready,
    /// Waiting on the user (a startup question, an approval).
    Blocked,
    /// No such agent any more: it exited, or the pane was closed.
    Gone,
    NotYet,
}

fn readiness(agent: &Value) -> Readiness {
    match agent.get("agent_status").as_str() {
        Some("blocked") => Readiness::Blocked,
        Some("idle" | "done")
            if !agent.get("launch_pending").truthy() && !matches!(agent.get("interactive_ready"), Value::Bool(false)) =>
        {
            Readiness::Ready
        }
        _ => Readiness::NotYet,
    }
}

fn wait_ready(client: &mut HerdrClient, agent: &str, limit: Duration) -> Readiness {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        let result = call(client, "agent.wait", dict(&[
            ("target", Value::str(agent)), ("timeout_ms", Value::Int(500)),
            ("until", Value::List(vec![Value::str("idle"), Value::str("done"), Value::str("blocked")])),
        ]), &["agent", "wait", agent, "--until", "idle", "--until", "done", "--until", "blocked", "--timeout", "500"]);
        match result {
            Ok(result) => {
                if let Some(state) = result.get("agent").map(readiness) {
                    if state != Readiness::NotYet {
                        return state;
                    }
                }
            }
            Err(error) if error.contains("not found") => return Readiness::Gone,
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Readiness::NotYet
}

fn defer_prompt(agent: &str, prompt: &str) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = crate::config::state_dir().filter(|dir| dir.is_absolute()).unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot save deferred prompt: {e}"))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let path = dir.join(format!("hertrees-prompt-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        crate::host::owner_only(&mut options);
        let mut file = options.open(&path).map_err(|e| format!("Cannot save deferred prompt: {e}"))?;
        file.write_all(prompt.as_bytes()).map_err(|e| format!("Cannot save deferred prompt: {e}"))?;
        let executable = std::env::current_exe().map_err(|e| format!("Cannot defer prompt: {e}"))?;
        let mut command = std::process::Command::new(executable);
        command.arg("--send-prompt").arg(agent).arg(&path)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        crate::host::detach(&mut command);
        command.spawn().map_err(|e| format!("Cannot defer prompt: {e}"))?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&path); }
    result
}

/// Detached prompt sender; args are the agent name and private prompt-file path.
pub fn send_prompt_main(args: &[String]) -> i32 {
    if args.len() != 2 { return 2; }
    let path = std::path::Path::new(&args[1]);
    let result = (|| {
        let prompt = std::fs::read_to_string(path).map_err(|_| ())?;
        let mut client = HerdrClient::from_env();
        let deadline = Instant::now() + Duration::from_secs(15 * 60);
        let mut was_blocked = false;
        let mut stalls = 0;
        while Instant::now() < deadline {
            match wait_ready(&mut client, &args[0], Duration::from_secs(2)) {
                Readiness::Ready if was_blocked => {
                    // Answering a startup question redraws the agent's screen:
                    // let it settle, then look again before typing.
                    was_blocked = false;
                    std::thread::sleep(Duration::from_secs(2));
                }
                Readiness::Ready => match send_prompt(&mut client, &args[0], &prompt) {
                    Ok(()) => return Ok(()),
                    Err(error) if error.contains("stalled") && stalls == 0 => {
                        stalls += 1;
                        std::thread::sleep(Duration::from_secs(2));
                    }
                    Err(_) => return Err(()),
                },
                Readiness::Blocked => {
                    was_blocked = true;
                    std::thread::sleep(Duration::from_millis(500));
                }
                Readiness::NotYet => std::thread::sleep(Duration::from_millis(500)),
                Readiness::Gone => return Err(()),
            }
        }
        Err(())
    })();
    let _ = std::fs::remove_file(path);
    if result.is_ok() { 0 } else { 1 }
}

fn usable(wt: &Worktree) -> Result<(), String> {
    if wt.bare { Err("Cannot open a bare worktree".into()) } else if wt.prunable { Err("Worktree is missing or prunable".into()) } else { Ok(()) }
}

pub fn start_existing(client: &mut HerdrClient, project: &Project, wt: &Worktree, kind: &str, prompt: Option<&str>, taken_names: &[String], progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    let branch = wt.branch.as_deref().unwrap_or("detached");
    let started = open_tab(client, project, wt, progress)?;
    launch(client, started, branch, kind, prompt, taken_names, progress)
}

/// A fresh tab in the worktree's space, opening the space when it is closed:
/// its pane is a shell until an agent starts in it.
pub fn open_tab(client: &mut HerdrClient, project: &Project, wt: &Worktree, progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    usable(wt)?;
    let branch = wt.branch.as_deref().unwrap_or("detached");
    progress(&format!("opening {branch}…"));
    let result = call(client, "worktree.open", dict(&[("cwd", Value::str(&project.repo_root)), ("path", Value::str(&wt.path)), ("focus", Value::Bool(false))]), &["worktree", "open", "--cwd", &project.repo_root, "--path", &wt.path, "--no-focus"])?;
    let mut started = location(&result, &wt.path)?;
    if result.get("already_open").is_some_and(Value::truthy) {
        let tab = call(client, "tab.create", dict(&[("workspace_id", Value::str(&started.workspace_id)), ("cwd", Value::str(&wt.path)), ("focus", Value::Bool(false))]), &["tab", "create", "--workspace", &started.workspace_id, "--cwd", &wt.path, "--no-focus"])?;
        started.tab_id = id(&tab, "tab", "tab_id")?;
        // tab.create returns TabInfo, not a root pane; resolve that tab's only pane.
        let panes = call(client, "pane.list", dict(&[("workspace_id", Value::str(&started.workspace_id))]), &["pane", "list", "--workspace", &started.workspace_id])?;
        started.pane_id = panes.get("panes").map(Value::list_or_empty).unwrap_or(&[]).iter()
            .find(|p| p.get("tab_id").as_str() == Some(&started.tab_id))
            .and_then(|p| p.get("pane_id").as_str()).map(str::to_owned)
            .ok_or_else(|| "herdr returned no pane for the new tab".to_owned())?;
    }
    Ok(started)
}

/// `prompt` may still hold `{path}` until the new checkout has been created.
/// `label` names the new space; herdr names it after the branch without one.
#[allow(clippy::too_many_arguments)]
pub fn start_new(client: &mut HerdrClient, project: &Project, branch: &str, base: Option<&str>, label: Option<&str>, kind: &str, prompt: Option<&str>, taken_names: &[String], progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    progress(&format!("opening {branch}…"));
    let started = create_shell(client, project, branch, base, label)?;
    let prompt = prompt.map(|p| p.replace("{path}", &started.path));
    launch(client, started, branch, kind, prompt.as_deref(), taken_names, progress)
}

/// New worktree with only its shell: no agent. `focus` afterwards shows it.
pub fn create_shell(client: &mut HerdrClient, project: &Project, branch: &str, base: Option<&str>, label: Option<&str>) -> Result<Started, String> {
    validate_branch(&project.repo_root, branch)?;
    let path = crate::project::new_worktree_path(project, branch);
    let path = path.to_str().ok_or("Worktree path is not valid UTF-8")?;
    crate::project::ignore_worktrees(&project.repo_root)?;
    let mut params = dict(&[("cwd", Value::str(&project.repo_root)), ("branch", Value::str(branch)), ("path", Value::str(path)), ("focus", Value::Bool(false))]);
    let mut argv = vec!["worktree", "create", "--cwd", &project.repo_root, "--branch", branch, "--path", path, "--no-focus"];
    if let Some(base) = base { if let Value::Dict(map) = &mut params { map.insert("base", Value::str(base)); } argv.extend(["--base", base]); }
    if let Some(label) = label.filter(|label| !label.trim().is_empty()) { if let Value::Dict(map) = &mut params { map.insert("label", Value::str(label)); } argv.extend(["--label", label]); }
    let result = call(client, "worktree.create", params, &argv)?;
    location(&result, &id(&result, "worktree", "path")?)
}

pub fn rename_space(client: &mut HerdrClient, workspace: &str, label: &str) -> Result<(), String> {
    call(client, "workspace.rename", dict(&[("workspace_id", Value::str(workspace)), ("label", Value::str(label))]), &["workspace", "rename", workspace, label]).map(|_| ())
}

pub fn rename_tab(client: &mut HerdrClient, tab: &str, label: &str) -> Result<(), String> {
    call(client, "tab.rename", dict(&[("tab_id", Value::str(tab)), ("label", Value::str(label))]), &["tab", "rename", tab, label]).map(|_| ())
}

/// Close a space and everything running in it; the checkout stays. Herdr
/// groups linked worktrees' spaces under the main checkout's and will not
/// close that one alone.
pub fn close_space(client: &mut HerdrClient, workspace: &str) -> Result<(), String> {
    call(client, "workspace.close", dict(&[("workspace_id", Value::str(workspace))]), &["workspace", "close", workspace]).map(|_| ())
        .map_err(|error| if error.contains("linked worktree") { "other worktrees' spaces are grouped under this one: close those first".into() } else { error })
}

pub fn close_tab(client: &mut HerdrClient, tab: &str) -> Result<(), String> {
    call(client, "tab.close", dict(&[("tab_id", Value::str(tab))]), &["tab", "close", tab]).map(|_| ())
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
/// with `delete_branch` the local branch too. Never forced: herdr and git both
/// refuse a checkout with changes.
pub fn remove_worktree(client: &mut HerdrClient, project: &Project, wt: &Worktree, delete_branch: bool) -> Result<Removed, String> {
    if wt.is_main || wt.bare {
        return Err("the main checkout is never removed".into());
    }
    match &wt.open_workspace_id {
        // Herdr closes the space and removes the checkout in one step.
        Some(workspace) => {
            call(client, "worktree.remove", dict(&[("workspace_id", Value::str(workspace)), ("force", Value::Bool(false))]), &["worktree", "remove", "--workspace", workspace])?;
        }
        None => git(&project.repo_root, &["worktree", "remove", "--", &wt.path])?,
    }
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

fn git(repo_root: &str, args: &[&str]) -> Result<(), String> {
    let mut argv = vec!["git".to_owned(), "-C".to_owned(), repo_root.to_owned()];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    match crate::proc::run(&argv, 30.0) {
        Ok(output) if output.success => Ok(()),
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("git failed").trim_start_matches("fatal: ").to_owned()),
        Err(crate::proc::RunError::Spawn(e)) => Err(format!("cannot run git: {e}")),
        Err(crate::proc::RunError::Timeout) => Err("git timed out".into()),
    }
}

pub fn focus(client: &mut HerdrClient, started: &Started) -> Result<(), String> {
    call(client, "workspace.focus", dict(&[("workspace_id", Value::str(&started.workspace_id))]), &["workspace", "focus", &started.workspace_id])?;
    call(client, "tab.focus", dict(&[("tab_id", Value::str(&started.tab_id))]), &["tab", "focus", &started.tab_id])?;
    if !started.agent.is_empty() {
        call(client, "agent.focus", dict(&[("target", Value::str(&started.pane_id))]), &["agent", "focus", &started.pane_id])?;
    }
    Ok(())
}
pub fn jump(client: &mut HerdrClient, project: &Project, wt: &Worktree) -> Result<(), String> {
    usable(wt)?;
    if let Some(workspace) = &wt.open_workspace_id {
        call(client, "workspace.focus", dict(&[("workspace_id", Value::str(workspace))]), &["workspace", "focus", workspace])?;
    } else {
        call(client, "worktree.open", dict(&[("cwd", Value::str(&project.repo_root)), ("path", Value::str(&wt.path)), ("focus", Value::Bool(true))]), &["worktree", "open", "--cwd", &project.repo_root, "--path", &wt.path, "--focus"])?;
    }
    Ok(())
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
        let mut client = HerdrClient::new(Some("/nonexistent/hertrees-test".into()), 0.001, true);

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
