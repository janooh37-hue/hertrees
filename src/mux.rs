//! The terminal multiplexer hertrees drives: Herdr (`herdr.rs`) or tmux
//! (`tmux.rs`). Both show a checkout in a *space* (a Herdr workspace, a tmux
//! window) holding *tabs* (Herdr tabs, tmux panes), and run agents in panes.
//! Git discovery, prompts and cleanup rules stay shared in `project.rs` and
//! `session.rs`.

use crate::model::{AgentHere, Project, Space, Worktree};

/// The checkouts of one repository, as the backend lists them. `project::load`
/// adds branches, heads, upstreams and the default branch from git.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    /// Main checkout: new worktrees go under it, even from a linked one.
    pub repo_root: String,
    pub repo_name: Option<String>,
    /// The checkout the backend resolved for the caller's directory; the
    /// deepest worktree enclosing that directory takes precedence.
    pub source_checkout: Option<String>,
    /// `path`, `branch`, `head`, `detached`, `bare`, `prunable` and
    /// `open_workspace_id` as known; the first row is the main checkout.
    pub worktrees: Vec<Worktree>,
}

/// Where a session landed. `agent` is empty for a shell.
#[derive(Clone, Debug)]
pub struct Started { pub workspace_id: String, pub tab_id: String, pub pane_id: String, pub agent: String, pub path: String, pub prompt_deferred: bool }

pub trait Mux: Send {
    fn clone_box(&self) -> Box<dyn Mux>;
    /// Every checkout of the repository containing `cwd`.
    fn worktrees(&mut self, cwd: &str) -> Result<Listing, String>;
    /// Running agents; best-effort, so an unreachable server is an empty list.
    fn agents(&mut self) -> Vec<AgentHere>;
    /// Open spaces that show a checkout; None when the server could not be asked.
    fn spaces(&mut self) -> Option<Vec<Space>>;
    /// A fresh shell tab in the worktree's space, opening the space when it is closed.
    fn open_tab(&mut self, project: &Project, wt: &Worktree) -> Result<Started, String>;
    /// Create the checkout at `path` on a new `branch` and open its space with a shell.
    fn create_worktree(&mut self, project: &Project, branch: &str, base: Option<&str>, label: Option<&str>, path: &str) -> Result<Started, String>;
    /// Whether `kind` can be given a first prompt here; checked before anything is created.
    fn can_prompt(&self, kind: &str) -> Result<(), String>;
    /// Replace the shell in `started`'s pane with agent `kind`, named `started.agent`.
    fn launch(&mut self, started: Started, kind: &str, prompt: Option<&str>, progress: &mut dyn FnMut(&str)) -> Result<Started, String>;
    /// Prompt a running agent (name or pane id); returns whether delivery was deferred.
    fn send_to(&mut self, agent: &str, prompt: &str) -> Result<bool, String>;
    fn rename_space(&mut self, space: &str, label: &str) -> Result<(), String>;
    fn rename_tab(&mut self, tab: &str, label: &str) -> Result<(), String>;
    /// Close a space and everything running in it; the checkout stays.
    fn close_space(&mut self, space: &str) -> Result<(), String>;
    fn close_tab(&mut self, tab: &str) -> Result<(), String>;
    /// Close the worktree's space and remove its checkout, never forced: a
    /// checkout with changes stays, and so does its space.
    fn remove_checkout(&mut self, project: &Project, wt: &Worktree) -> Result<(), String>;
    /// Show a started session: its space, tab and agent pane.
    fn focus(&mut self, started: &Started) -> Result<(), String>;
    /// Show the worktree's space, opening it when it is closed.
    fn jump(&mut self, project: &Project, wt: &Worktree) -> Result<(), String>;
}

impl Clone for Box<dyn Mux> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Herdr,
    Tmux,
}

/// Which multiplexer this process runs under. `HERTREES_MUX=herdr|tmux` decides
/// when both are visible (one nested in the other); otherwise a Herdr plugin
/// pane or socket wins, then `$TMUX`, then Herdr through its CLI.
pub fn kind_from_env() -> Kind {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    match std::env::var("HERTREES_MUX").ok().as_deref().map(str::trim) {
        Some("tmux") => return Kind::Tmux,
        Some("herdr") => return Kind::Herdr,
        _ => {}
    }
    if set("HERDR_PLUGIN_ID") || set("HERDR_SOCKET_PATH") || !set("TMUX") { Kind::Herdr } else { Kind::Tmux }
}

pub fn from_env() -> Box<dyn Mux> {
    match kind_from_env() {
        Kind::Herdr => Box::new(crate::client::HerdrClient::from_env()),
        Kind::Tmux => Box::new(crate::tmux::Tmux),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_wins_unless_only_tmux_is_present_or_the_user_says_so() {
        let _guard = crate::testutil::env_lock();
        let mut env = crate::testutil::EnvVars::new();
        env.remove("HERTREES_MUX").remove("HERDR_PLUGIN_ID").remove("HERDR_SOCKET_PATH").remove("TMUX");
        assert_eq!(kind_from_env(), Kind::Herdr, "neither: Herdr through its CLI, as before");
        env.set("TMUX", "/tmp/tmux-1000/default,1,0");
        assert_eq!(kind_from_env(), Kind::Tmux);
        env.set("HERDR_SOCKET_PATH", "/run/herdr.sock");
        assert_eq!(kind_from_env(), Kind::Herdr, "Herdr nested with tmux keeps Herdr");
        env.set("HERTREES_MUX", "tmux");
        assert_eq!(kind_from_env(), Kind::Tmux);
        env.remove("HERDR_SOCKET_PATH").set("HERTREES_MUX", "herdr");
        assert_eq!(kind_from_env(), Kind::Herdr);
    }
}
