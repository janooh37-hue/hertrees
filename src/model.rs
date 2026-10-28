//! The data one popup works on: the project, its worktrees, and what is known
//! about each one (PR, git facts, agents). Every module trades in these types.

/// The repository the popup was opened from.
#[derive(Clone, Debug, Default)]
pub struct Project {
    /// Main checkout; `worktree create/open` must be given this, never a linked checkout.
    pub repo_root: String,
    pub repo_name: String,
    /// Checkout the user was in when the popup opened (may be a linked worktree).
    pub source_checkout: String,
    /// Default branch name (e.g. "main") when it can be found locally, else None.
    pub default_branch: Option<String>,
}

/// One `git worktree` checkout. Identity is `path`, never `branch`.
#[derive(Clone, Debug, Default)]
pub struct Worktree {
    pub path: String,
    /// Local branch name without `refs/heads/`; None when detached or bare.
    pub branch: Option<String>,
    /// Full HEAD object id; empty when unknown.
    pub head: String,
    pub detached: bool,
    pub bare: bool,
    pub prunable: bool,
    /// The repository's main checkout (first row of `git worktree list`).
    pub is_main: bool,
    /// Space (Herdr workspace, tmux window) already showing this checkout.
    pub open_workspace_id: Option<String>,
    /// Upstream of `branch`: remote name and branch on that remote
    /// (`origin`, `feat/ledger-book-polish`). None without tracking config.
    pub upstream_remote: Option<String>,
    pub upstream_branch: Option<String>,
}

impl Worktree {
    pub fn title(&self) -> String {
        match &self.branch {
            Some(branch) => branch.clone(),
            None if self.bare => "(bare)".into(),
            None => format!("detached@{}", &self.head[..self.head.len().min(7)]),
        }
    }
}

/// A running agent whose working directory is inside a worktree.
#[derive(Clone, Debug, Default)]
pub struct AgentHere {
    /// herdr agent name (`herdr agent start <NAME>`), if it was given one.
    pub name: Option<String>,
    /// Agent kind as herdr reports it (`claude`, `omp`, ...).
    pub kind: String,
    /// herdr status: blocked | working | done | idle | unknown; tmux: running.
    pub status: String,
    pub cwd: String,
    pub pane_id: String,
    /// The tab holding the agent's pane; empty when herdr did not say.
    pub tab_id: String,
}

/// The space showing a worktree (a Herdr workspace, a tmux window): one per
/// checkout, named by the user or after the branch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Space {
    pub id: String,
    pub label: String,
    /// Checkout the workspace was opened on; matches `Worktree::path`.
    pub checkout: String,
    pub tabs: Vec<Tab>,
}

/// One tab of a space, in herdr's order. Labels are usually set by the bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tab {
    pub id: String,
    pub label: String,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrState {
    Merged,
    Open,
    Draft,
    Closed,
    /// Confirmed: no PR has this branch as its head.
    None,
    /// Not known: loading, gh missing/failed, unsupported remote, detached HEAD,
    /// or the fetched history was capped before a match was found.
    Unknown,
}

#[derive(Clone, Debug)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// Merged, Open, Draft or Closed; never None/Unknown.
    pub state: PrState,
    pub head_ref: String,
    /// Commit the PR's head pointed at last; empty when not known.
    pub head_oid: String,
    /// Login of the head repository owner; empty when GitHub no longer knows it.
    pub head_owner: String,
    pub base_ref: String,
    /// RFC 3339 timestamps as GitHub sends them.
    pub created_at: String,
    pub merged_at: Option<String>,
}

/// What the PR column shows for one worktree.
#[derive(Clone, Debug)]
pub struct PrInfo {
    pub state: PrState,
    pub pr: Option<PullRequest>,
    /// Matched through the branch's upstream name rather than its local name.
    pub via_upstream: bool,
    /// Why the state is Unknown (`gh not installed`, `offline`, ...), shown in the preview.
    pub reason: Option<String>,
}

impl PrInfo {
    pub fn unknown(reason: impl Into<String>) -> PrInfo {
        PrInfo { state: PrState::Unknown, pr: None, via_upstream: false, reason: Some(reason.into()) }
    }
}

/// Cheap local facts about a checkout. None means not computed / unavailable, never zero.
#[derive(Clone, Debug, Default)]
pub struct GitFacts {
    /// Changed + untracked entries from `git status`.
    pub dirty: Option<u32>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    /// Unix seconds of the HEAD commit.
    pub last_commit_epoch: Option<i64>,
    /// Up to five most recent commits: "abc1234 subject".
    pub recent_commits: Vec<String>,
    /// What the branch changed against its base, when it has one.
    pub base: Option<BaseDiff>,
}

/// A branch against the default branch it will merge into: what the preview's
/// diff shows. Unlike the upstream counts, this is the work itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BaseDiff {
    /// The ref compared with: `origin/main` when fetched, else `main`.
    pub base: String,
    /// Commits on the branch the base lacks, and the reverse.
    pub ahead: u32,
    pub behind: u32,
    /// Changed files since the merge base: (path, added, deleted); binary
    /// files have no line counts.
    pub files: Vec<(String, Option<u32>, Option<u32>)>,
}
