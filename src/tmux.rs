//! The tmux backend: one window per worktree (its space), whose panes are its
//! tabs. tmux reports no agent states, so an agent gets its first prompt on its
//! command line, is listed as `running` while it runs, and a prompt for a
//! running agent is pasted into its pane without waiting for it to be idle.
//!
//! hertrees marks what it opened with user options: a window's checkout, and
//! a pane's agent name, kind and tab label. Windows without the mark are left
//! alone. Free text reaches tmux escaped (`#` formats, a trailing `;`), and
//! prompts go through private files instead of tmux's command line.

use crate::model::{AgentHere, Project, Space, Tab, Worktree};
use crate::mux::{Listing, Mux, Started};
use crate::proc::RunError;

/// Window option: the checkout the window shows.
const CHECKOUT: &str = "@hertrees_checkout";
/// Pane options: the agent's name and kind while it runs, a tab's own label.
const AGENT: &str = "@hertrees_agent";
const KIND: &str = "@hertrees_kind";
const LABEL: &str = "@hertrees_label";

/// The pane command around an agent: run it (its prompt read from a private
/// file, which is deleted first), drop the agent marks when it exits, then
/// leave a shell in the pane. The trap keeps `sh` alive through the agent's
/// own ^C; the agent itself gets default signal handling.
const RUNNER: &str = r#"trap : INT QUIT
file=$1 flag=$2
shift 2
if [ -n "$file" ]; then
  prompt=$(cat "$file"; printf x)
  rm -f "$file"
  "$@" "$flag${prompt%x}"
else
  "$@"
fi
tmux set-option -pu -t "$TMUX_PANE" @hertrees_agent
tmux set-option -pu -t "$TMUX_PANE" @hertrees_kind
exec "${SHELL:-/bin/sh}"
"#;

#[derive(Clone, Debug)]
pub struct Tmux;

fn tmux(args: &[&str]) -> Result<String, String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("tmux".to_owned());
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    match crate::proc::run(&argv, 5.0) {
        Ok(output) if output.success => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("tmux failed").to_owned()),
        Err(RunError::Spawn(error)) if error.kind() == std::io::ErrorKind::NotFound => Err("tmux not found on PATH".into()),
        Err(RunError::Spawn(error)) => Err(format!("cannot run tmux: {error}")),
        Err(RunError::Timeout) => Err(format!("tmux {} timed out", args.first().unwrap_or(&""))),
    }
}

/// tmux ends a command at an argument ending in `;` unless a `\` precedes it,
/// and drops that one `\`.
fn literal(text: &str) -> String {
    match text.strip_suffix(';') {
        Some(head) => format!("{head}\\;"),
        None => text.to_owned(),
    }
}

/// An argument tmux also expands as a format (`-c`, `-n`, `rename-window`).
fn format_literal(text: &str) -> String {
    literal(&text.replace('#', "##"))
}

/// One row of `list-panes -a`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Pane {
    session: String,
    window: String,
    pane: String,
    focused: bool,
    checkout: String,
    agent: String,
    kind: String,
    label: String,
    command: String,
    window_name: String,
    cwd: String,
}

const PANE_FORMAT: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{window_active}#{pane_active}\t#{@hertrees_checkout}\t#{@hertrees_agent}\t#{@hertrees_kind}\t#{@hertrees_label}\t#{pane_current_command}\t#{window_name}\t#{pane_current_path}";

fn parse_panes(output: &str) -> Vec<Pane> {
    output.lines().filter_map(|line| {
        let mut fields = line.splitn(11, '\t');
        let mut next = || fields.next().map(str::to_owned);
        Some(Pane {
            session: next()?,
            window: next()?,
            pane: next()?,
            focused: next()? == "11",
            checkout: next()?,
            agent: next()?,
            kind: next()?,
            label: next()?,
            command: next()?,
            window_name: next()?,
            cwd: next()?,
        })
    }).collect()
}

fn panes() -> Result<Vec<Pane>, String> {
    tmux(&["list-panes", "-a", "-F", PANE_FORMAT]).map(|output| parse_panes(&output))
}

/// The window hertrees opened on `path`, if one is still open.
fn window_for(path: &str) -> Result<Option<String>, String> {
    Ok(panes()?.into_iter().find(|pane| pane.checkout == path).map(|pane| pane.window))
}

/// `git worktree list --porcelain -z`: the first record is the main checkout.
fn parse_worktrees(output: &[u8]) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in String::from_utf8_lossy(output).split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            worktrees.extend(current.take());
            current = Some(Worktree { path: path.to_owned(), ..Worktree::default() });
            continue;
        }
        let Some(worktree) = current.as_mut() else { continue };
        if let Some(head) = field.strip_prefix("HEAD ") {
            worktree.head = head.to_owned();
        } else if let Some(branch) = field.strip_prefix("branch ") {
            worktree.branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(branch).to_owned());
        } else if field == "bare" {
            worktree.bare = true;
        } else if field == "detached" {
            worktree.detached = true;
        } else if field == "prunable" || field.starts_with("prunable ") {
            worktree.prunable = true;
        }
    }
    worktrees.extend(current);
    worktrees
}

/// A private file holding `text`, for the pane runner or `load-buffer`.
fn private_file(text: &str) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = crate::config::state_dir().filter(|dir| dir.is_absolute()).unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot save the prompt: {e}"))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let path = dir.join(format!("hertrees-tmux-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    crate::host::owner_only(&mut options);
    let written = options.open(&path).and_then(|mut file| file.write_all(text.as_bytes()));
    match written {
        Ok(()) => Ok(path),
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(format!("Cannot save the prompt: {error}"))
        }
    }
}

/// How an agent takes its first prompt on the command line.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PromptArg {
    /// The last argument, after `--` so a leading `-` stays text.
    AfterDashes,
    /// The last argument, as the agent documents it (no `--` support known).
    Bare,
    /// One argument, this option glued in front: `--prompt=TEXT`.
    Glued(&'static str),
}

/// The agent's executable, and how it takes a prompt; None: it cannot.
fn agent_command(kind: &str) -> (&str, Option<PromptArg>) {
    match kind {
        "claude" | "codex" | "pi" => (kind, Some(PromptArg::AfterDashes)),
        "cursor" => ("cursor-agent", Some(PromptArg::AfterDashes)),
        "omp" => (kind, Some(PromptArg::Bare)),
        "opencode" => (kind, Some(PromptArg::Glued("--prompt="))),
        "gemini" => (kind, Some(PromptArg::Glued("--prompt-interactive="))),
        "copilot" => (kind, Some(PromptArg::Glued("--interactive="))),
        other => (other, None),
    }
}

impl Tmux {
    /// A new detached window on `path`, marked as its space.
    fn new_window(&self, path: &str, name: &str) -> Result<Started, String> {
        let (dir, name) = (format_literal(path), format_literal(name));
        let output = tmux(&["new-window", "-d", "-P", "-F", "#{window_id}\t#{pane_id}", "-c", &dir, "-n", &name])?;
        let (window, pane) = output.trim_end().split_once('\t').ok_or("tmux did not report the new window")?;
        tmux(&["set-option", "-w", "-t", window, CHECKOUT, &literal(path)])?;
        Ok(Started { workspace_id: window.to_owned(), tab_id: pane.to_owned(), pane_id: pane.to_owned(), agent: String::new(), path: path.to_owned(), prompt_deferred: false })
    }

    /// Bring `window` (and `pane`) up in this client, switching session when it is elsewhere.
    fn show(&self, window: &str, pane: Option<&str>) -> Result<(), String> {
        tmux(&["select-window", "-t", window])?;
        if let Some(pane) = pane { tmux(&["select-pane", "-t", pane])?; }
        let target = tmux(&["display-message", "-p", "-t", window, "#{session_id}"])?;
        let current = tmux(&["display-message", "-p", "#{session_id}"])?;
        if target.trim() != current.trim() {
            tmux(&["switch-client", "-t", target.trim()])?;
        }
        Ok(())
    }
}

impl Mux for Tmux {
    fn clone_box(&self) -> Box<dyn Mux> {
        Box::new(self.clone())
    }

    fn worktrees(&mut self, cwd: &str) -> Result<Listing, String> {
        let argv: Vec<String> = ["git", "--no-optional-locks", "-C", cwd, "worktree", "list", "--porcelain", "-z"].map(str::to_owned).to_vec();
        let output = crate::proc::run(&argv, 3.0).ok().filter(|output| output.success).ok_or("not a git project")?;
        let mut worktrees = parse_worktrees(&output.stdout);
        let repo_root = worktrees.first().map(|worktree| worktree.path.clone()).ok_or("not a git project")?;
        // Without a server there is nothing open; the checkouts still list.
        let panes = panes().unwrap_or_default();
        for worktree in &mut worktrees {
            worktree.open_workspace_id = panes.iter().find(|pane| pane.checkout == worktree.path).map(|pane| pane.window.clone());
        }
        Ok(Listing { repo_root, repo_name: None, source_checkout: None, worktrees })
    }

    fn agents(&mut self) -> Vec<AgentHere> {
        panes().unwrap_or_default().into_iter().filter(|pane| !pane.agent.is_empty()).map(|pane| AgentHere {
            name: Some(pane.agent),
            kind: if pane.kind.is_empty() { "unknown".into() } else { pane.kind },
            status: "running".into(),
            cwd: pane.cwd,
            tab_id: pane.pane.clone(),
            pane_id: pane.pane,
        }).collect()
    }

    fn spaces(&mut self) -> Option<Vec<Space>> {
        let panes = panes().ok()?;
        let mut spaces: Vec<Space> = Vec::new();
        for pane in panes.into_iter().filter(|pane| !pane.checkout.is_empty()) {
            let tab = Tab {
                label: [&pane.label, &pane.agent, &pane.command].into_iter().find(|label| !label.is_empty()).unwrap_or(&pane.pane).clone(),
                id: pane.pane,
                focused: pane.focused,
            };
            match spaces.iter_mut().find(|space| space.id == pane.window) {
                Some(space) => space.tabs.push(tab),
                None => spaces.push(Space { id: pane.window, label: pane.window_name, checkout: pane.checkout, tabs: vec![tab] }),
            }
        }
        Some(spaces)
    }

    fn open_tab(&mut self, _project: &Project, wt: &Worktree) -> Result<Started, String> {
        let Some(window) = window_for(&wt.path)? else { return self.new_window(&wt.path, &wt.title()) };
        let dir = format_literal(&wt.path);
        let pane = tmux(&["split-window", "-d", "-t", &window, "-c", &dir, "-P", "-F", "#{pane_id}"])?;
        let pane = pane.trim_end();
        Ok(Started { workspace_id: window, tab_id: pane.to_owned(), pane_id: pane.to_owned(), agent: String::new(), path: wt.path.clone(), prompt_deferred: false })
    }

    /// `git worktree add -b`, as the picker's preview shows it.
    fn create_worktree(&mut self, project: &Project, branch: &str, base: Option<&str>, label: Option<&str>, path: &str) -> Result<Started, String> {
        let mut args = vec!["worktree", "add", "-b", branch, "--", path];
        args.extend(base);
        crate::session::git(&project.repo_root, &args)?;
        self.new_window(path, label.unwrap_or(branch))
    }

    fn can_prompt(&self, kind: &str) -> Result<(), String> {
        match agent_command(kind).1 {
            Some(_) => Ok(()),
            None => Err(format!("under tmux, {kind} cannot be started with a prompt: start it blank, then type it")),
        }
    }

    fn launch(&mut self, started: Started, kind: &str, prompt: Option<&str>, _progress: &mut dyn FnMut(&str)) -> Result<Started, String> {
        let (executable, form) = agent_command(kind);
        let mut command = vec![executable];
        let flag = match (prompt, form) {
            (None, _) => "",
            (Some(_), None) => return Err(self.can_prompt(kind).unwrap_err()),
            (Some(_), Some(PromptArg::AfterDashes)) => {
                command.push("--");
                ""
            }
            (Some(_), Some(PromptArg::Bare)) => "",
            (Some(_), Some(PromptArg::Glued(flag))) => flag,
        };
        let file = prompt.map(private_file).transpose()?;
        let file_arg = file.as_ref().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
        let result = (|| {
            // Marks first: they outlive the respawn, and the runner drops them on exit.
            tmux(&["set-option", "-p", "-t", &started.pane_id, AGENT, &literal(&started.agent)])?;
            tmux(&["set-option", "-p", "-t", &started.pane_id, KIND, &literal(kind)])?;
            let dir = format_literal(&started.path);
            let mut args = vec!["respawn-pane", "-k", "-t", &started.pane_id, "-c", &dir, "--", "sh", "-c", RUNNER, "hertrees", &file_arg, flag];
            args.extend(command.iter().copied());
            tmux(&args)
        })();
        if let Err(error) = result {
            if let Some(file) = &file { let _ = std::fs::remove_file(file); }
            return Err(error);
        }
        Ok(started)
    }

    /// Paste as one bracketed paste, then press Enter. Never deferred: tmux
    /// cannot tell whether the agent is ready, so this is typed now.
    fn send_to(&mut self, agent: &str, prompt: &str) -> Result<bool, String> {
        let pane = panes()?.into_iter().find(|pane| !pane.agent.is_empty() && (pane.agent == agent || pane.pane == agent))
            .ok_or("that agent is no longer running")?;
        let file = private_file(prompt)?;
        let buffer = format!("hertrees-{}", std::process::id());
        let loaded = tmux(&["load-buffer", "-b", &buffer, &file.to_string_lossy()]);
        let _ = std::fs::remove_file(&file);
        loaded?;
        tmux(&["paste-buffer", "-d", "-p", "-r", "-b", &buffer, "-t", &pane.pane])?;
        // A key in the same read as the paste can be taken as part of it.
        std::thread::sleep(std::time::Duration::from_millis(300));
        tmux(&["send-keys", "-t", &pane.pane, "Enter"])?;
        Ok(false)
    }

    fn rename_space(&mut self, space: &str, label: &str) -> Result<(), String> {
        tmux(&["rename-window", "-t", space, "--", &format_literal(label)]).map(|_| ())
    }

    fn rename_tab(&mut self, tab: &str, label: &str) -> Result<(), String> {
        tmux(&["set-option", "-p", "-t", tab, LABEL, &literal(label)]).map(|_| ())
    }

    fn close_space(&mut self, space: &str) -> Result<(), String> {
        tmux(&["kill-window", "-t", space]).map(|_| ())
    }

    fn close_tab(&mut self, tab: &str) -> Result<(), String> {
        tmux(&["kill-pane", "-t", tab]).map(|_| ())
    }

    /// git refuses a checkout with changes before the window is touched.
    fn remove_checkout(&mut self, project: &Project, wt: &Worktree) -> Result<(), String> {
        crate::session::git(&project.repo_root, &["worktree", "remove", "--", &wt.path])?;
        match &wt.open_workspace_id {
            Some(window) => match tmux(&["kill-window", "-t", window]) {
                Err(error) if !error.contains("can't find window") => Err(format!("removed the checkout, but its window stays: {error}")),
                _ => Ok(()),
            },
            None => Ok(()),
        }
    }

    fn focus(&mut self, started: &Started) -> Result<(), String> {
        self.show(&started.workspace_id, Some(&started.pane_id))
    }

    fn jump(&mut self, _project: &Project, wt: &Worktree) -> Result<(), String> {
        let window = match window_for(&wt.path)? {
            Some(window) => window,
            None => self.new_window(&wt.path, &wt.title())?.workspace_id,
        };
        self.show(&window, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_keeps_main_first_and_flags_detached_bare_and_prunable() {
        let output = b"worktree /r\0HEAD aaa\0branch refs/heads/main\0\0\
            worktree /r/.worktrees/feat/x\0HEAD bbb\0branch refs/heads/feat/x\0locked\0\0\
            worktree /r/.worktrees/d\0HEAD ccc\0detached\0prunable gitdir file points to non-existent location\0\0";
        let rows = parse_worktrees(output);
        assert_eq!(rows.iter().map(|w| (w.path.as_str(), w.branch.as_deref(), w.head.as_str())).collect::<Vec<_>>(),
            [("/r", Some("main"), "aaa"), ("/r/.worktrees/feat/x", Some("feat/x"), "bbb"), ("/r/.worktrees/d", None, "ccc")]);
        assert_eq!(rows.iter().map(|w| (w.detached, w.prunable)).collect::<Vec<_>>(), [(false, false), (false, false), (true, true)]);
        let bare = parse_worktrees(b"worktree /r.git\0bare\0\0");
        assert!(bare[0].bare && bare[0].branch.is_none());
    }

    #[test]
    fn free_text_survives_tmux_parsing() {
        assert_eq!(literal("plain"), "plain");
        assert_eq!(literal("ends;"), "ends\\;");
        assert_eq!(literal("ends\\;"), "ends\\\\;");
        assert_eq!(format_literal("#{x} 50%;"), "##{x} 50%\\;");
    }

    #[test]
    fn panes_split_on_tabs_and_keep_tabs_in_the_path() {
        let rows = parse_panes("$1\t@2\t%3\t11\t/r/.worktrees/x\tclaude-x\tclaude\t\tclaude\tx\t/r/.worktrees/x/a\tb\n$1\t@1\t%0\t01\t\t\t\t\tzsh\tmain\t/r\n");
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].focused, rows[0].agent.as_str(), rows[0].cwd.as_str()), (true, "claude-x", "/r/.worktrees/x/a\tb"));
        assert!(!rows[1].focused && rows[1].checkout.is_empty());
    }
}
