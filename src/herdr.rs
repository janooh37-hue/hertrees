//! The Herdr backend: workspaces, tabs and agents through Herdr's API, with
//! its agent states gating prompt delivery.

use crate::client::{dict, HerdrClient};
use crate::model::{AgentHere, Project, Space, Tab, Worktree};
use crate::mux::{Listing, Mux, Started};
use crate::pyjson::{Map, Value};
use std::time::{Duration, Instant};

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

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

impl Mux for HerdrClient {
    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(self.clone())
    }

    /// Discover through Herdr so linked checkouts resolve to their main repository.
    fn worktrees(&mut self, cwd: &str) -> Result<Listing, String> {
        let result = self
            .call("worktree.list", dict(&[("cwd", Value::str(cwd))]), &["worktree", "list", "--cwd", cwd])
            .map_err(|error| {
                let message = error.message.trim();
                if message.is_empty() { "herdr unreachable".to_owned() } else { message.to_owned() }
            })?;
        let source = result.get("source").ok_or("not a git project")?;
        let repo_root = text(source.get("repo_root")).ok_or("not a git project")?;
        let worktrees = result.get("worktrees").map(Value::list_or_empty).unwrap_or_default().iter().filter_map(|row| {
            let path = text(row.get("path"))?;
            let bare = row.get("is_bare").truthy();
            let detached = row.get("is_detached").truthy();
            Some(Worktree {
                path: path.to_owned(),
                branch: if bare || detached { None } else {
                    text(row.get("branch")).map(|branch| branch.strip_prefix("refs/heads/").unwrap_or(branch).to_owned())
                },
                detached,
                bare,
                prunable: row.get("is_prunable").truthy(),
                open_workspace_id: text(row.get("open_workspace_id")).map(str::to_owned),
                ..Worktree::default()
            })
        }).collect();
        Ok(Listing {
            repo_root: repo_root.to_owned(),
            repo_name: text(source.get("repo_name")).map(str::to_owned),
            source_checkout: text(source.get("source_checkout_path")).map(str::to_owned),
            worktrees,
        })
    }

    fn agents(&mut self) -> Vec<AgentHere> {
        let Ok(result) = self.call("agent.list", dict(&[]), &["agent", "list"]) else { return Vec::new() };
        result.get("agents").map(Value::list_or_empty).unwrap_or_default().iter().map(|row| {
            let status = match row.get("agent_status").as_str().unwrap_or("") {
                "blocked" => "blocked",
                "working" => "working",
                "done" => "done",
                "idle" => "idle",
                _ => "unknown",
            };
            AgentHere {
                name: text(row.get("name")).map(str::to_owned),
                kind: text(row.get("agent")).unwrap_or("unknown").to_owned(),
                status: status.to_owned(),
                cwd: text(row.get("foreground_cwd")).or_else(|| text(row.get("cwd"))).unwrap_or("").to_owned(),
                pane_id: text(row.get("pane_id")).unwrap_or("").to_owned(),
                tab_id: text(row.get("tab_id")).unwrap_or("").to_owned(),
            }
        }).collect()
    }

    fn spaces(&mut self) -> Option<Vec<Space>> {
        let workspaces = self.call("workspace.list", dict(&[]), &["workspace", "list"]).ok()?;
        let tabs = self.call("tab.list", dict(&[]), &["tab", "list"]).ok();
        let tabs = tabs.as_ref().and_then(|result| result.get("tabs")).map(Value::list_or_empty).unwrap_or_default();
        Some(workspaces.get("workspaces").map(Value::list_or_empty).unwrap_or_default().iter().filter_map(|row| {
            let id = text(row.get("workspace_id"))?;
            let checkout = text(row.get("worktree").get("checkout_path"))?;
            Some(Space {
                id: id.to_owned(),
                label: text(row.get("label")).unwrap_or(id).to_owned(),
                checkout: checkout.to_owned(),
                tabs: tabs.iter().filter(|tab| tab.get("workspace_id").as_str() == Some(id)).filter_map(|tab| {
                    let tab_id = text(tab.get("tab_id"))?;
                    Some(Tab {
                        id: tab_id.to_owned(),
                        label: text(tab.get("label")).unwrap_or(tab_id).to_owned(),
                        focused: tab.get("focused").truthy(),
                    })
                }).collect(),
            })
        }).collect())
    }

    fn open_tab(&mut self, project: &Project, wt: &Worktree) -> Result<Started, String> {
        let result = call(self, "worktree.open", dict(&[("cwd", Value::str(&project.repo_root)), ("path", Value::str(&wt.path)), ("focus", Value::Bool(false))]), &["worktree", "open", "--cwd", &project.repo_root, "--path", &wt.path, "--no-focus"])?;
        let mut started = location(&result, &wt.path)?;
        if result.get("already_open").is_some_and(Value::truthy) {
            let tab = call(self, "tab.create", dict(&[("workspace_id", Value::str(&started.workspace_id)), ("cwd", Value::str(&wt.path)), ("focus", Value::Bool(false))]), &["tab", "create", "--workspace", &started.workspace_id, "--cwd", &wt.path, "--no-focus"])?;
            started.tab_id = id(&tab, "tab", "tab_id")?;
            // tab.create returns TabInfo, not a root pane; resolve that tab's only pane.
            let panes = call(self, "pane.list", dict(&[("workspace_id", Value::str(&started.workspace_id))]), &["pane", "list", "--workspace", &started.workspace_id])?;
            started.pane_id = panes.get("panes").map(Value::list_or_empty).unwrap_or(&[]).iter()
                .find(|p| p.get("tab_id").as_str() == Some(&started.tab_id))
                .and_then(|p| p.get("pane_id").as_str()).map(str::to_owned)
                .ok_or_else(|| "herdr returned no pane for the new tab".to_owned())?;
        }
        Ok(started)
    }

    /// `label` names the new space; herdr names it after the branch without one.
    fn create_worktree(&mut self, project: &Project, branch: &str, base: Option<&str>, label: Option<&str>, path: &str) -> Result<Started, String> {
        let mut params = dict(&[("cwd", Value::str(&project.repo_root)), ("branch", Value::str(branch)), ("path", Value::str(path)), ("focus", Value::Bool(false))]);
        let mut argv = vec!["worktree", "create", "--cwd", &project.repo_root, "--branch", branch, "--path", path, "--no-focus"];
        if let Some(base) = base { if let Value::Dict(map) = &mut params { map.insert("base", Value::str(base)); } argv.extend(["--base", base]); }
        if let Some(label) = label.filter(|label| !label.trim().is_empty()) { if let Value::Dict(map) = &mut params { map.insert("label", Value::str(label)); } argv.extend(["--label", label]); }
        let result = call(self, "worktree.create", params, &argv)?;
        location(&result, &id(&result, "worktree", "path")?)
    }

    fn can_prompt(&self, _kind: &str) -> Result<(), String> {
        Ok(())
    }

    fn launch(&mut self, mut started: Started, kind: &str, prompt: Option<&str>, progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
        let old_timeout = self.timeout;
        self.timeout = 60.0;
        let result = call(self, "agent.start", dict(&[("name", Value::str(&started.agent)), ("kind", Value::str(kind)), ("pane_id", Value::str(&started.pane_id)), ("timeout_ms", Value::Int(45000))]), &["agent", "start", &started.agent, "--kind", kind, "--pane", &started.pane_id, "--timeout", "45000"]);
        self.timeout = old_timeout;
        match result {
            Ok(_) => {}
            // The agent runs but stopped at a startup question (folder trust, login,
            // update notice): it keeps its name, and the prompt waits for it below.
            Err(error) if error.contains("blocked during startup") || error.contains("not ready") => {}
            Err(error) => return Err(error),
        }
        if let Some(prompt) = prompt.filter(|p| !p.is_empty()) {
            match wait_ready(self, &started.agent, Duration::from_secs(8)) {
                Readiness::Ready => {
                    progress("sending prompt…");
                    send_prompt(self, &started.agent, prompt)?;
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

    /// An idle agent gets it now; a busy one, or one waiting on a question,
    /// gets it from the detached sender once it is idle again.
    fn send_to(&mut self, agent: &str, prompt: &str) -> Result<bool, String> {
        match wait_ready(self, agent, Duration::from_secs(1)) {
            Readiness::Ready => send_prompt(self, agent, prompt).map(|()| false),
            Readiness::Gone => Err("that agent is no longer running".into()),
            Readiness::Blocked | Readiness::NotYet => defer_prompt(agent, prompt).map(|()| true),
        }
    }

    fn rename_space(&mut self, space: &str, label: &str) -> Result<(), String> {
        call(self, "workspace.rename", dict(&[("workspace_id", Value::str(space)), ("label", Value::str(label))]), &["workspace", "rename", space, label]).map(|_| ())
    }

    fn rename_tab(&mut self, tab: &str, label: &str) -> Result<(), String> {
        call(self, "tab.rename", dict(&[("tab_id", Value::str(tab)), ("label", Value::str(label))]), &["tab", "rename", tab, label]).map(|_| ())
    }

    /// Herdr groups linked worktrees' spaces under the main checkout's and will
    /// not close that one alone.
    fn close_space(&mut self, space: &str) -> Result<(), String> {
        call(self, "workspace.close", dict(&[("workspace_id", Value::str(space))]), &["workspace", "close", space]).map(|_| ())
            .map_err(|error| if error.contains("linked worktree") { "other worktrees' spaces are grouped under this one: close those first".into() } else { error })
    }

    fn close_tab(&mut self, tab: &str) -> Result<(), String> {
        call(self, "tab.close", dict(&[("tab_id", Value::str(tab))]), &["tab", "close", tab]).map(|_| ())
    }

    fn remove_checkout(&mut self, project: &Project, wt: &Worktree) -> Result<(), String> {
        match &wt.open_workspace_id {
            // Herdr closes the space and removes the checkout in one step.
            Some(workspace) => {
                call(self, "worktree.remove", dict(&[("workspace_id", Value::str(workspace)), ("force", Value::Bool(false))]), &["worktree", "remove", "--workspace", workspace]).map(|_| ())
            }
            None => crate::session::git(&project.repo_root, &["worktree", "remove", "--", &wt.path]),
        }
    }

    fn focus(&mut self, started: &Started) -> Result<(), String> {
        call(self, "workspace.focus", dict(&[("workspace_id", Value::str(&started.workspace_id))]), &["workspace", "focus", &started.workspace_id])?;
        call(self, "tab.focus", dict(&[("tab_id", Value::str(&started.tab_id))]), &["tab", "focus", &started.tab_id])?;
        if !started.agent.is_empty() {
            call(self, "agent.focus", dict(&[("target", Value::str(&started.pane_id))]), &["agent", "focus", &started.pane_id])?;
        }
        Ok(())
    }

    fn jump(&mut self, project: &Project, wt: &Worktree) -> Result<(), String> {
        if let Some(workspace) = &wt.open_workspace_id {
            call(self, "workspace.focus", dict(&[("workspace_id", Value::str(workspace))]), &["workspace", "focus", workspace])?;
        } else {
            call(self, "worktree.open", dict(&[("cwd", Value::str(&project.repo_root)), ("path", Value::str(&wt.path)), ("focus", Value::Bool(true))]), &["worktree", "open", "--cwd", &project.repo_root, "--path", &wt.path, "--focus"])?;
        }
        Ok(())
    }
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
