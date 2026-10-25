//! Agent launches and checkout navigation through herdr.
use crate::client::{dict, HerdrClient};
use crate::model::{PrInfo, Project, Worktree};
use crate::pyjson::{Map, Value};
use std::time::{Duration, Instant};

// Herdr's supported kinds, the most used first: the agent picker's default order.
const KINDS: &[&str] = &["claude", "codex", "omp", "opencode", "pi", "gemini", "cursor", "copilot", "amp", "grok", "kimi", "qwen", "devin", "agy", "cline", "mastracode", "kiro", "droid", "hermes", "kilo", "qodercli", "letta", "maki", "muse"];

/// Supported, executable agents in configuration order (or the order of `KINDS`).
pub fn installed_agents(preferred: &[String]) -> Vec<String> {
    let Some(path) = std::env::var_os("PATH") else { return Vec::new() };
    let dirs: Vec<_> = std::env::split_paths(&path).collect();
    let candidates: Vec<&str> = if preferred.is_empty() { KINDS.to_vec() } else { preferred.iter().map(String::as_str).collect() };
    let mut result = Vec::new();
    for kind in candidates {
        if !KINDS.contains(&kind) || result.iter().any(|s| s == kind) { continue; }
        let executable = if kind == "cursor" { "cursor-agent" } else { kind };
        if dirs.iter().any(|dir| crate::opener::is_executable_file(&dir.join(executable))) {
            result.push(kind.to_owned());
        }
    }
    result
}

#[derive(Clone, Debug, Default)]
pub struct PromptVars { pub branch: String, pub base: String, pub path: String, pub repo: String, pub pr: String }

/// Shared picker/CLI variables; a new checkout keeps `{path}` until creation.
pub fn prompt_vars(project: &Project, wt: &Worktree, base: Option<&str>, info: Option<&PrInfo>) -> PromptVars {
    PromptVars {
        branch: wt.title(),
        base: base.or(project.default_branch.as_deref()).unwrap_or("HEAD").into(),
        path: wt.path.clone(),
        repo: project.repo_name.clone(),
        pr: info.and_then(|info| info.pr.as_ref()).map(|pr| format!("#{} {}", pr.number, pr.url)).unwrap_or_else(|| "none".into()),
    }
}

/// Expand known placeholders once, without interpreting substituted text.
pub fn expand_prompt(template: &str, vars: &PromptVars) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let replacements = [("{branch}", &vars.branch), ("{base}", &vars.base), ("{path}", &vars.path), ("{repo}", &vars.repo), ("{pr}", &vars.pr)];
        if let Some((token, value)) = replacements.iter().find(|(token, _)| rest.starts_with(token)) {
            out.push_str(value);
            rest = &rest[token.len()..];
        } else { out.push('{'); rest = &rest[1..]; }
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
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::var_os("HERDR_PLUGIN_STATE_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot save deferred prompt: {e}"))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let path = dir.join(format!("hertrees-prompt-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let result = (|| {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(|e| format!("Cannot save deferred prompt: {e}"))?;
        file.write_all(prompt.as_bytes()).map_err(|e| format!("Cannot save deferred prompt: {e}"))?;
        let executable = std::env::current_exe().map_err(|e| format!("Cannot defer prompt: {e}"))?;
        let mut command = std::process::Command::new(executable);
        command.arg("--send-prompt").arg(agent).arg(&path)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        unsafe { command.pre_exec(|| { if libc::setsid() < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) } }); }
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
    launch(client, started, branch, kind, prompt, taken_names, progress)
}

/// `prompt` may still hold `{path}` until the new checkout has been created.
pub fn start_new(client: &mut HerdrClient, project: &Project, branch: &str, base: Option<&str>, kind: &str, prompt: Option<&str>, taken_names: &[String], progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
    progress(&format!("opening {branch}…"));
    let started = create_shell(client, project, branch, base)?;
    let prompt = prompt.map(|p| p.replace("{path}", &started.path));
    launch(client, started, branch, kind, prompt.as_deref(), taken_names, progress)
}

/// New worktree with only its shell: no agent. `focus` afterwards shows it.
pub fn create_shell(client: &mut HerdrClient, project: &Project, branch: &str, base: Option<&str>) -> Result<Started, String> {
    validate_branch(&project.repo_root, branch)?;
    let path = crate::project::new_worktree_path(project, branch);
    let path = path.to_str().ok_or("Worktree path is not valid UTF-8")?;
    crate::project::ignore_worktrees(&project.repo_root)?;
    let mut params = dict(&[("cwd", Value::str(&project.repo_root)), ("branch", Value::str(branch)), ("path", Value::str(path)), ("focus", Value::Bool(false))]);
    let mut argv = vec!["worktree", "create", "--cwd", &project.repo_root, "--branch", branch, "--path", path, "--no-focus"];
    if let Some(base) = base { if let Value::Dict(map) = &mut params { map.insert("base", Value::str(base)); } argv.extend(["--base", base]); }
    let result = call(client, "worktree.create", params, &argv)?;
    location(&result, &id(&result, "worktree", "path")?)
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
    fn prompts_preserve_unknown_and_do_not_reexpand() {
        let vars = PromptVars { branch: "feature/{base}".into(), base: "main".into(), path: "/tmp/repo".into(), repo: "repo".into(), pr: "none".into() };
        assert_eq!(expand_prompt("{branch} {base} {path} {repo} {pr} {unknown} {", &vars), "feature/{base} main /tmp/repo repo none {unknown} {");
        assert_eq!(expand_prompt("", &vars), "");
    }
}
