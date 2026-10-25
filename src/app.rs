//! The picker: state, key handling, background loads, and the draw loop.
//!
//! Portions derived from herdr-bar (MIT, see LICENSE.herdr-bar). The rows are a project's worktrees instead of
//! Herdr's tabs, and Enter can start an agent session as well as jump.

use crate::client::HerdrClient;
use crate::config::{Config, SCOPES};
use crate::fuzzy::{match_prepared, split_query, Term};
use crate::keys::{Event, KeyDecoder, MouseKind};
use crate::model::{AgentHere, GitFacts, PrInfo, PrState, Project, Worktree};
use crate::paint::Painter;
use crate::pr::{self, GhRepo, PrSnapshot};
use crate::presets::{self, Preset};
use crate::project;
use crate::pyjson;
use crate::pystr;
use crate::render::{self, compute_layout, scrollbar_column, Item, Layout, Meta, Row, Segment};
use crate::session::{self, PromptVars, Started};
use crate::term::Terminal;
use crate::textutil::{display_width, pad, sanitize, truncate};
use crate::theme::Theme;
use crate::wordmark::{self, Style};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

pub const ESC_TIMEOUT: f64 = 0.04;
pub const STATUS_TTL: f64 = 2.5;
pub const ERROR_TTL: f64 = 8.0;
const FACT_WORKERS: usize = 4;
/// Pseudo agent kind: a new worktree with only a shell.
const SHELL: &str = "shell";

pub const PLACEHOLDER: &str = "search worktrees, branches, PRs…    @ agents   ⇥ PR state";
pub const PLACEHOLDER_SHORT: &str = "search worktrees, branches, PRs…";

pub fn monotonic() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64() + 1_000_000.0
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn scope_chip(scope: &str) -> &'static str {
    match scope {
        "all" => "everything",
        "nopr" => "no PR",
        "open" => "◉ open",
        "draft" => "◐ draft",
        "merged" => "✓ merged",
        "closed" => "● closed",
        "agents" => "@ agents",
        _ => "",
    }
}

/// `42s`, `7m`, `2h04m`, `3d4h`: compact running-time format.
pub fn format_age(seconds: i64) -> String {
    if seconds < 0 {
        return String::new();
    }
    if seconds < 60 {
        return format!("{}s", seconds);
    }
    if seconds < 3600 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60);
    }
    format!("{}d{}h", seconds / 86_400, seconds % 86_400 / 3600)
}

fn status_rank(status: &str) -> u8 {
    match status {
        "blocked" => 0,
        "working" => 1,
        "done" => 2,
        "idle" => 3,
        _ => 4,
    }
}

pub fn pr_role(state: PrState) -> &'static str {
    match state {
        PrState::Merged => "pr_merged",
        PrState::Open => "pr_open",
        PrState::Draft => "pr_draft",
        PrState::Closed => "pr_closed",
        PrState::None => "muted",
        PrState::Unknown => "unknown",
    }
}

fn pr_word(state: PrState) -> &'static str {
    match state {
        PrState::Merged => "merged",
        PrState::Open => "open",
        PrState::Draft => "draft",
        PrState::Closed => "closed",
        PrState::None => "no PR",
        PrState::Unknown => "PR ?",
    }
}

fn pr_chip(info: &PrInfo) -> String {
    let glyph = match info.state {
        PrState::Merged => "✓ ",
        PrState::Open => "◉ ",
        PrState::Draft => "◐ ",
        PrState::Closed => "● ",
        // "· no PR" would stutter after the " · " that separates chunks.
        PrState::None => "",
        PrState::Unknown => "○ ",
    };
    match &info.pr {
        Some(pr) => format!("{}{} #{}", glyph, pr_word(info.state), pr.number),
        None => format!("{}{}", glyph, pr_word(info.state)),
    }
}

/// A single-line text editor: the search query and every inline prompt.
#[derive(Clone, Debug, Default)]
pub struct LineEdit {
    pub text: String,
    pub cursor: usize,
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn char_slice(text: &str, start: usize, stop: usize) -> String {
    text.chars().skip(start).take(stop.saturating_sub(start)).collect()
}

impl LineEdit {
    fn set(&mut self, text: String, cursor: usize) {
        self.cursor = cursor.min(char_len(&text));
        self.text = text;
    }

    /// Insert typed or pasted text; false when there was nothing to insert.
    fn insert(&mut self, text: &str, pasted: bool) -> bool {
        let mut text = sanitize(text, " ");
        if pasted {
            text = pystr::split_whitespace(&text).join(" ");
        }
        if text.is_empty() || (self.text.is_empty() && pystr::strip(&text).is_empty()) {
            return false;
        }
        let joined = format!("{}{}{}", char_slice(&self.text, 0, self.cursor), text, char_slice(&self.text, self.cursor, usize::MAX));
        let cursor = self.cursor + char_len(&text);
        self.set(joined, cursor);
        true
    }

    /// Apply an editing key; true when the key was an editing key.
    fn key(&mut self, name: &str) -> bool {
        match name {
            "home" | "ctrl+home" | "ctrl+a" => self.cursor = 0,
            "end" | "ctrl+end" | "ctrl+e" => self.cursor = char_len(&self.text),
            "left" => self.cursor = self.cursor.saturating_sub(1),
            "right" => self.cursor = (self.cursor + 1).min(char_len(&self.text)),
            "backspace" => {
                if self.cursor > 0 {
                    let text = format!("{}{}", char_slice(&self.text, 0, self.cursor - 1), char_slice(&self.text, self.cursor, usize::MAX));
                    self.set(text, self.cursor - 1);
                }
            }
            "ctrl+d" => {
                if self.cursor < char_len(&self.text) {
                    let text = format!("{}{}", char_slice(&self.text, 0, self.cursor), char_slice(&self.text, self.cursor + 1, usize::MAX));
                    self.set(text, self.cursor);
                }
            }
            "ctrl+u" => self.set(String::new(), 0),
            "ctrl+w" => {
                let head = char_slice(&self.text, 0, self.cursor);
                let head = pystr::rstrip(&head);
                let head = match head.rfind(' ') {
                    Some(space) => head[..space + 1].to_string(),
                    None => String::new(),
                };
                let cursor = char_len(&head);
                let text = format!("{}{}", head, char_slice(&self.text, self.cursor, usize::MAX));
                self.set(text, cursor);
            }
            _ => return false,
        }
        true
    }
}

/// The prompt editor: like `LineEdit`, but pasted line breaks stay and ^j or
/// alt+⏎ starts a new line. `↑`/`↓` move between lines and report false at the
/// first/last line, where the caller recalls earlier prompts instead.
#[derive(Clone, Debug, Default)]
pub struct TextEdit {
    pub text: String,
    pub cursor: usize,
}

impl TextEdit {
    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    fn set(&mut self, chars: &[char], cursor: usize) {
        self.text = chars.iter().collect();
        self.cursor = cursor.min(chars.len());
    }

    /// Start of the cursor's line and its column, in chars.
    fn line_start(chars: &[char], cursor: usize) -> usize {
        chars[..cursor].iter().rposition(|&c| c == '\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(chars: &[char], cursor: usize) -> usize {
        chars[cursor..].iter().position(|&c| c == '\n').map(|i| cursor + i).unwrap_or(chars.len())
    }

    fn insert(&mut self, text: &str, pasted: bool) -> bool {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<String> = text.split('\n').map(|line| sanitize(line, " ")).collect();
        let text = if pasted { lines.join("\n") } else { lines.join(" ") };
        if text.is_empty() {
            return false;
        }
        let mut chars = self.chars();
        let added: Vec<char> = text.chars().collect();
        let cursor = self.cursor + added.len();
        chars.splice(self.cursor..self.cursor, added);
        self.set(&chars, cursor);
        true
    }

    fn key(&mut self, name: &str) -> bool {
        let mut chars = self.chars();
        let cursor = self.cursor.min(chars.len());
        let start = Self::line_start(&chars, cursor);
        match name {
            "ctrl+j" | "alt+enter" | "shift+enter" => {
                chars.insert(cursor, '\n');
                self.set(&chars, cursor + 1);
            }
            "home" | "ctrl+a" => self.cursor = start,
            "end" | "ctrl+e" => self.cursor = Self::line_end(&chars, cursor),
            "ctrl+home" => self.cursor = 0,
            "ctrl+end" => self.cursor = chars.len(),
            "left" => self.cursor = cursor.saturating_sub(1),
            "right" => self.cursor = (cursor + 1).min(chars.len()),
            "up" => {
                if start == 0 {
                    return false;
                }
                let above = Self::line_start(&chars, start - 1);
                self.cursor = (above + cursor - start).min(start - 1);
            }
            "down" => {
                let end = Self::line_end(&chars, cursor);
                if end == chars.len() {
                    return false;
                }
                self.cursor = (end + 1 + cursor - start).min(Self::line_end(&chars, end + 1));
            }
            "backspace" if cursor > 0 => {
                chars.remove(cursor - 1);
                self.set(&chars, cursor - 1);
            }
            "ctrl+d" | "delete" if cursor < chars.len() => {
                chars.remove(cursor);
                self.set(&chars, cursor);
            }
            "backspace" | "ctrl+d" | "delete" => {}
            "ctrl+u" => self.set(&[], 0),
            "ctrl+w" => {
                let mut from = cursor;
                while from > start && chars[from - 1] == ' ' {
                    from -= 1;
                }
                while from > start && chars[from - 1] != ' ' {
                    from -= 1;
                }
                chars.drain(from..cursor);
                self.set(&chars, from);
            }
            _ => return false,
        }
        true
    }
}

/// Where a new session goes.
#[derive(Clone, Debug)]
pub enum Target {
    Existing(usize),
    New(String),
    /// Prompt an agent already running in this worktree (^s); `kind` is its herdr target.
    Running(usize),
}

enum Mode {
    List,
    Branch(LineEdit),
    Agent { target: Target, edit: LineEdit, kinds: Vec<String>, rows: Vec<usize>, selected: usize, preset: usize },
    /// The prompt editor: the user's text under `preset` (or alone for custom).
    Custom { target: Target, kind: String, edit: TextEdit, preset: usize, recall: Option<(usize, String)>, completion: Option<Completion> },
    /// Values for the prompt's own placeholders (`{ticket}`), one at a time.
    Fields { target: Target, kind: String, prompt: String, addition: String, names: Vec<String>, values: Vec<String>, edit: LineEdit, back: Box<Mode> },
    Presets { previous: Box<Mode>, edit: LineEdit, rows: Vec<Row>, selected: usize, confirm: Option<String> },
    PresetName { previous: Box<Mode>, action: PresetAction, edit: LineEdit },
    Busy(String),
}

/// A placeholder being completed with ⇥: where its `{` is, what was typed
/// after it, and which candidate is shown.
#[derive(Clone, Debug)]
struct Completion {
    brace: usize,
    prefix: String,
    choice: usize,
}

enum PresetAction {
    New,
    Rename(String),
    Save(String),
}

pub enum Outcome {
    Cancel,
    Jump(usize),
    Started(Started),
    Edit { path: PathBuf, created: bool },
}

enum Msg {
    Facts(usize, GitFacts),
    Prs(Result<PrSnapshot, String>),
    Progress(String),
    Done(Result<Started, String>),
    /// A prompt sent to a running agent: its name and whether delivery waits for it.
    Sent(Result<(String, bool), String>),
}

/// Everything one load of a project produces; built off the UI thread.
pub struct Loaded {
    pub result: Result<(Project, Vec<Worktree>), String>,
    pub agents: Vec<AgentHere>,
    pub gh: Result<GhRepo, String>,
    pub cached: Option<PrSnapshot>,
    pub branch_times: BTreeMap<String, i64>,
}

impl Loaded {
    pub fn load(client: &mut HerdrClient, cwd: Option<&str>) -> Loaded {
        let result = match cwd {
            Some(cwd) => project::load(client, cwd),
            None => Err("no working directory: open the picker from a pane".into()),
        };
        let agents = project::agents(client);
        let (gh, cached, branch_times) = match &result {
            Ok((project, _)) => {
                let gh = pr::github_repo(&project.repo_root);
                let cached = gh.as_ref().ok().and_then(pr::cached);
                (gh, cached, branch_times(&project.repo_root))
            }
            Err(error) => (Err(error.clone()), None, BTreeMap::new()),
        };
        Loaded { result, agents, gh, cached, branch_times }
    }
}

/// Branch name -> committer time of its head, in one git call.
fn branch_times(repo_root: &str) -> BTreeMap<String, i64> {
    let argv: Vec<String> = ["git", "-C", repo_root, "for-each-ref", "--format=%(refname:lstrip=2)%09%(committerdate:unix)", "refs/heads"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let Ok(output) = crate::proc::run(&argv, 3.0) else { return BTreeMap::new() };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (name, time) = line.rsplit_once('\t')?;
            Some((name.to_string(), time.trim().parse().ok()?))
        })
        .collect()
}

pub struct Picker {
    pub client: HerdrClient,
    pub config: Config,
    presets: Vec<Preset>,
    presets_dir: Option<PathBuf>,
    pub theme: Theme,
    pub project: Option<Project>,
    load_error: Option<String>,
    pub worktrees: Vec<Worktree>,
    agents: Vec<Vec<AgentHere>>,
    agent_names: Vec<String>,
    gh: Result<GhRepo, String>,
    snapshot: Option<PrSnapshot>,
    fetching: bool,
    fetch_error: Option<String>,
    prs: Vec<PrInfo>,
    facts: Vec<Option<GitFacts>>,
    branch_times: BTreeMap<String, i64>,
    /// Worktree indexes in at-rest order.
    order: Vec<usize>,
    items: Vec<Item>,
    rows: Vec<Row>,
    query: LineEdit,
    scope: &'static str,
    selected: usize,
    offset: usize,
    preview_enabled: bool,
    preview_chosen: bool,
    preview_showing: bool,
    mode: Mode,
    status: Option<(String, &'static str, f64)>,
    /// Screen row (0-based) -> (first column, end column, url) of its PR link.
    links: BTreeMap<isize, (isize, isize, String)>,
    list_top: isize,
    list_height: isize,
    /// The last drawn popup size, so ^l can tell whether a style fits.
    size: (isize, isize),
    last_refresh: f64,
    dirty: bool,
    painter: Painter,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pending_facts: usize,
    /// Earlier prompt-editor texts, oldest first; read when the editor opens.
    history: Vec<String>,
}

impl Picker {
    pub fn new(client: HerdrClient, config: Config, theme: Theme, loaded: Loaded) -> Picker {
        let (tx, rx) = channel();
        let preview_enabled = config.preview_starts_on();
        let scope = config.scope;
        let presets_dir = presets::dir();
        let presets = presets::load(presets_dir.as_deref(), None);
        let mut picker = Picker {
            client,
            config,
            presets,
            presets_dir,
            theme,
            project: None,
            load_error: None,
            worktrees: Vec::new(),
            agents: Vec::new(),
            agent_names: Vec::new(),
            gh: Err(String::new()),
            snapshot: None,
            fetching: false,
            fetch_error: None,
            prs: Vec::new(),
            facts: Vec::new(),
            branch_times: BTreeMap::new(),
            order: Vec::new(),
            items: Vec::new(),
            rows: Vec::new(),
            query: LineEdit::default(),
            scope,
            selected: 0,
            offset: 0,
            preview_enabled,
            preview_chosen: false,
            preview_showing: false,
            mode: Mode::List,
            status: None,
            links: BTreeMap::new(),
            list_top: 2,
            list_height: 1,
            last_refresh: monotonic(),
            dirty: true,
            painter: Painter::new(),
            tx,
            rx,
            pending_facts: 0,
            size: (0, 0),
            history: Vec::new(),
        };
        picker.take(loaded);
        picker.presets = picker.load_presets();
        picker
    }

    /// Adopt a fresh load: rows, cached PRs, then background fetches.
    fn take(&mut self, loaded: Loaded) {
        let Loaded { result, agents, gh, cached, branch_times } = loaded;
        match result {
            Ok((project, worktrees)) => {
                self.project = Some(project);
                self.worktrees = worktrees;
                self.load_error = None;
            }
            Err(error) => {
                self.project = None;
                self.worktrees = Vec::new();
                self.load_error = Some(error);
            }
        }
        self.branch_times = branch_times;
        self.gh = gh;
        if cached.is_some() {
            self.snapshot = cached;
        }
        self.facts = vec![None; self.worktrees.len()];
        self.set_agents(agents);
        self.order_rows();
        self.classify();
        self.rebuild_items();
        if self.project.is_some() {
            let stale = self.snapshot.as_ref().map(|s| !pr::is_fresh(s, self.config.pr_cache_ttl_s)).unwrap_or(true);
            if stale {
                self.fetch_prs();
            }
            self.spawn_facts();
        }
    }

    fn set_agents(&mut self, agents: Vec<AgentHere>) {
        self.agent_names = agents.iter().filter_map(|a| a.name.clone()).collect();
        let mut by = project::agents_by_worktree(&self.worktrees, &agents);
        for list in &mut by {
            list.sort_by_key(|a| status_rank(&a.status));
        }
        self.agents = by;
    }

    /// At rest: the main checkout, then branches by their last commit, newest
    /// first, then detached and broken checkouts.
    fn order_rows(&mut self) {
        let mut order: Vec<usize> = (0..self.worktrees.len()).collect();
        let key = |i: &usize| {
            let wt = &self.worktrees[*i];
            let time = wt.branch.as_ref().and_then(|b| self.branch_times.get(b)).copied();
            (!wt.is_main, wt.branch.is_none() || wt.prunable || wt.bare, std::cmp::Reverse(time.unwrap_or(0)))
        };
        order.sort_by_key(key);
        self.order = order;
    }

    fn classify(&mut self) {
        let unknown = |reason: &str| PrInfo::unknown(reason);
        self.prs = self
            .worktrees
            .iter()
            .map(|wt| match (&self.gh, &self.snapshot) {
                (Err(reason), _) => unknown(reason),
                (Ok(repo), Some(snapshot)) => {
                    pr::classify(wt, repo, self.project.as_ref().and_then(|p| p.default_branch.as_deref()), snapshot)
                }
                (Ok(_), None) => match &self.fetch_error {
                    Some(reason) => unknown(reason),
                    None => unknown("loading pull requests…"),
                },
            })
            .collect();
    }

    fn fetch_prs(&mut self) {
        let Ok(repo) = self.gh.clone() else { return };
        if self.fetching {
            return;
        }
        self.fetching = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Prs(pr::fetch(&repo)));
        });
    }

    /// Git facts on a few worker threads, rows in display order first.
    fn spawn_facts(&mut self) {
        let queue: Vec<(usize, String)> =
            self.order.iter().rev().map(|&i| (i, self.worktrees[i].path.clone())).collect();
        self.pending_facts = queue.len();
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..FACT_WORKERS.min(self.pending_facts) {
            let queue = queue.clone();
            let tx = self.tx.clone();
            std::thread::spawn(move || loop {
                let Some((index, path)) = queue.lock().ok().and_then(|mut q| q.pop()) else { return };
                if tx.send(Msg::Facts(index, project::git_facts(&path))).is_err() {
                    return;
                }
            });
        }
    }

    fn rebuild_items(&mut self) {
        let now = now_unix();
        self.items = (0..self.worktrees.len()).map(|i| self.build_item(i, now)).collect();
        self.refilter();
    }

    fn display_path(&self, path: &str) -> String {
        let Some(project) = &self.project else { return path.to_string() };
        let root = project.repo_root.trim_end_matches('/');
        if path == root {
            return crate::pyos::basename(root).to_string();
        }
        if let Some(rest) = path.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
            return rest.to_string();
        }
        let parent = std::path::Path::new(root).parent().and_then(|p| p.to_str()).unwrap_or("");
        if let Some(rest) = path.strip_prefix(parent).filter(|_| !parent.is_empty()).and_then(|r| r.strip_prefix('/')) {
            return format!("../{}", rest);
        }
        if let Some(home) = crate::pyos::home() {
            if let Some(rest) = path.strip_prefix(home.trim_end_matches('/')).and_then(|r| r.strip_prefix('/')) {
                return format!("~/{}", rest);
            }
        }
        path.to_string()
    }


    fn build_item(&self, index: usize, now: i64) -> Item {
        let wt = &self.worktrees[index];
        let agents = &self.agents[index];
        let info = &self.prs[index];
        let detail = self.display_path(&wt.path);
        let (status, agent) = match agents.first() {
            Some(first) => (first.status.clone(), Some(first.kind.clone())),
            None => ("none".to_string(), None),
        };
        let mut meta = Vec::new();
        if let Some(first) = agents.first() {
            let role = render::agent_role(Some(&first.kind));
            if agents.len() > 1 {
                meta.push(Meta::new(role, format!("{} agents", agents.len())));
            } else if let Some(name) = &first.name {
                meta.push(Meta::new(role, format!("@{}", name)));
            } else if !self.theme.agent_icons || crate::icons::logo_for(Some(&first.kind)).is_none() {
                meta.push(Meta::new(role, first.kind.clone()));
            }
        }
        if wt.prunable {
            meta.push(Meta::new("blocked", "missing"));
        }
        if let Some(facts) = &self.facts[index] {
            if let Some(dirty) = facts.dirty.filter(|&d| d > 0) {
                meta.push(Meta::new("working", format!("±{}", dirty)));
            }
            let mut sync = String::new();
            if let Some(ahead) = facts.ahead.filter(|&a| a > 0) {
                sync.push_str(&format!("↑{}", ahead));
            }
            if let Some(behind) = facts.behind.filter(|&b| b > 0) {
                sync.push_str(&format!("↓{}", behind));
            }
            if !sync.is_empty() {
                meta.push(Meta::new("muted", sync));
            }
            if let Some(epoch) = facts.last_commit_epoch {
                meta.push(Meta::new("muted", format_age(now - epoch)));
            }
        }
        if self.project.as_ref().map(|p| p.source_checkout == wt.path).unwrap_or(false) {
            meta.push(Meta::new("accent", "current"));
        }
        let word = render::status_word(&status);
        if !word.is_empty() {
            meta.push(Meta::new(render::status_role(&status), word));
        }
        let mut chip = Meta::new(pr_role(info.state), pr_chip(info));
        chip.link = info.pr.as_ref().map(|pr| pr.url.clone());
        meta.push(chip);

        let mut fields = vec![detail.clone()];
        if let Some(pr) = &info.pr {
            fields.push(format!("#{}", pr.number));
            fields.push(pr.title.clone());
        }
        fields.push(pr_word(info.state).to_string());
        for agent in agents {
            fields.push(agent.kind.clone());
            if let Some(name) = &agent.name {
                fields.push(name.clone());
            }
        }
        Item::new(wt.title(), detail, status, agent, meta, fields)
    }

    fn in_scope(&self, index: usize) -> bool {
        let state = self.prs[index].state;
        match self.scope {
            "nopr" => state == PrState::None,
            "open" => state == PrState::Open,
            "draft" => state == PrState::Draft,
            "merged" => state == PrState::Merged,
            "closed" => state == PrState::Closed,
            "agents" => !self.agents[index].is_empty(),
            _ => true,
        }
    }

    /// Filter and rank, keeping the selected worktree selected when it survives.
    fn refilter(&mut self) {
        let keep = self.rows.get(self.selected).map(|r| r.item);
        let terms: Vec<Term> = split_query(&self.query.text).iter().map(|t| Term::new(t)).collect();
        let mut scored: Vec<(i64, usize, Row)> = Vec::new();
        for (rank, &index) in self.order.iter().enumerate() {
            if !self.in_scope(index) {
                continue;
            }
            if terms.is_empty() {
                scored.push((0, rank, Row { item: index, positions: Vec::new(), field_positions: BTreeMap::new() }));
                continue;
            }
            if let Some((score, field_positions)) = score_item(&terms, &self.items[index]) {
                let positions = field_positions.get(&0).cloned().unwrap_or_default();
                scored.push((score, rank, Row { item: index, positions, field_positions }));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.rows = scored.into_iter().map(|(_, _, row)| row).collect();
        self.selected = keep.and_then(|k| self.rows.iter().position(|r| r.item == k)).unwrap_or(0);
        if !terms.is_empty() {
            self.selected = 0;
        }
        self.dirty = true;
    }

    fn selected_index(&self) -> Option<usize> {
        self.rows.get(self.selected).map(|r| r.item)
    }

    fn flash(&mut self, text: impl Into<String>, role: &'static str) {
        let ttl = if role == "blocked" { ERROR_TTL } else { STATUS_TTL };
        self.status = Some((text.into(), role, monotonic() + ttl));
        self.dirty = true;
    }

    // -- background results ---------------------------------------------------

    fn drain(&mut self) -> Option<Outcome> {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Facts(index, facts) => {
                    if index < self.facts.len() {
                        self.facts[index] = Some(facts);
                    }
                    self.pending_facts = self.pending_facts.saturating_sub(1);
                    changed = true;
                }
                Msg::Prs(result) => {
                    self.fetching = false;
                    match result {
                        Ok(snapshot) => {
                            self.snapshot = Some(snapshot);
                            self.fetch_error = None;
                        }
                        Err(reason) => {
                            if self.snapshot.is_some() {
                                self.flash(format!("PRs not refreshed: {}", reason), "muted");
                            }
                            self.fetch_error = Some(reason);
                        }
                    }
                    self.classify();
                    changed = true;
                }
                Msg::Progress(text) => {
                    self.mode = Mode::Busy(text);
                    self.dirty = true;
                }
                Msg::Done(Ok(started)) => return Some(Outcome::Started(started)),
                Msg::Sent(result) => {
                    self.mode = Mode::List;
                    match result {
                        Ok((agent, false)) => self.flash(format!("sent to {}", agent), "accent"),
                        Ok((agent, true)) => self.flash(format!("queued for {}: sent once it is idle", agent), "accent"),
                        Err(error) => self.flash(error, "blocked"),
                    }
                    self.refresh_agents();
                }
                Msg::Done(Err(error)) => {
                    self.mode = Mode::List;
                    // A failure after `worktree.create` (agent start, prompt) still
                    // left a new checkout: list it, so its agents are not shown under main.
                    self.reload();
                    self.flash(error, "blocked");
                }
            }
        }
        if changed {
            self.rebuild_items();
        }
        None
    }

    fn refresh_agents(&mut self) {
        self.last_refresh = monotonic();
        if self.project.is_none() {
            return;
        }
        let agents = project::agents(&mut self.client);
        let before: Vec<Vec<(String, String)>> =
            self.agents.iter().map(|l| l.iter().map(|a| (a.pane_id.clone(), a.status.clone())).collect()).collect();
        self.set_agents(agents);
        let after: Vec<Vec<(String, String)>> =
            self.agents.iter().map(|l| l.iter().map(|a| (a.pane_id.clone(), a.status.clone())).collect()).collect();
        if before != after {
            self.rebuild_items();
        }
    }

    /// ^r: reload worktrees, agents and git facts, and re-fetch PRs.
    fn reload(&mut self) {
        let Some(project) = &self.project else { return };
        let cwd = project.source_checkout.clone();
        let keep = self.selected_index().map(|i| self.worktrees[i].path.clone());
        let loaded = Loaded::load(&mut self.client, Some(&cwd));
        let snapshot = self.snapshot.take();
        self.take(Loaded { cached: loaded.cached.or(snapshot), ..loaded });
        self.fetch_prs();
        if let Some(path) = keep {
            if let Some(pos) = self.rows.iter().position(|r| self.worktrees[r.item].path == path) {
                self.selected = pos;
            }
        }
        self.flash("refreshing…", "accent");
    }

    // -- input ----------------------------------------------------------------

    fn move_by(&mut self, delta: isize, wrap: bool) {
        let count = match &self.mode {
            Mode::Agent { rows, .. } => rows.len(),
            Mode::Presets { rows, .. } => rows.len(),
            _ => self.rows.len(),
        } as isize;
        if count == 0 {
            return;
        }
        let current = match &self.mode {
            Mode::Agent { selected, .. } | Mode::Presets { selected, .. } => *selected,
            _ => self.selected,
        } as isize;
        let mut target = current + delta;
        if wrap && delta.abs() == 1 {
            target = target.rem_euclid(count);
        }
        let target = target.clamp(0, count - 1) as usize;
        match &mut self.mode {
            Mode::Agent { selected, .. } | Mode::Presets { selected, .. } => *selected = target,
            _ => self.selected = target,
        }
        self.dirty = true;
    }

    fn page(&self) -> isize {
        (self.list_height - 1).max(1)
    }

    fn cycle_scope(&mut self, step: isize) {
        let index = SCOPES.iter().position(|s| *s == self.scope).unwrap_or(0) as isize;
        self.scope = SCOPES[(index + step).rem_euclid(SCOPES.len() as isize) as usize];
        self.selected = 0;
        self.refilter();
    }

    fn handle_list_key(&mut self, name: &str) -> Option<Outcome> {
        match name {
            "esc" | "ctrl+c" | "ctrl+g" | "ctrl+q" => return Some(Outcome::Cancel),
            "enter" => {
                let index = self.selected_index()?;
                let wt = &self.worktrees[index];
                if wt.bare || wt.prunable {
                    self.flash("this checkout cannot be opened", "blocked");
                    return None;
                }
                return Some(Outcome::Jump(index));
            }
            "up" | "ctrl+p" | "ctrl+k" => self.move_by(-1, true),
            "down" | "ctrl+n" | "ctrl+j" => self.move_by(1, true),
            "tab" => self.cycle_scope(1),
            "shift+tab" => self.cycle_scope(-1),
            "pgup" | "alt+v" => self.move_by(-self.page(), false),
            "pgdn" | "ctrl+v" => self.move_by(self.page(), false),
            "ctrl+o" => {
                self.preview_enabled = !self.preview_showing;
                self.preview_chosen = true;
                self.dirty = true;
            }
            "ctrl+b" => self.open_selected_pr(),
            "ctrl+r" => self.reload(),
            "ctrl+l" => self.cycle_wordmark(),
            "ctrl+s" => {
                if let Some(index) = self.selected_index() {
                    if self.agents.get(index).is_some_and(|agents| !agents.is_empty()) {
                        self.enter_agent_mode(Target::Running(index));
                    } else {
                        self.flash("no agent is running in this worktree: ^t starts one", "muted");
                    }
                }
            }
            "ctrl+t" => {
                if let Some(index) = self.selected_index() {
                    let wt = &self.worktrees[index];
                    if wt.bare || wt.prunable {
                        self.flash("this checkout cannot host a session", "blocked");
                    } else {
                        self.enter_agent_mode(Target::Existing(index));
                    }
                }
            }
            "ctrl+x" => {
                if self.project.is_some() {
                    self.mode = Mode::Branch(LineEdit::default());
                    self.dirty = true;
                }
            }
            "backspace" if self.query.text.is_empty() && self.scope != "all" => {
                self.scope = "all";
                self.refilter();
            }
            "ctrl+u" => {
                self.scope = self.config.scope;
                self.query = LineEdit::default();
                self.refilter();
            }
            other => {
                let before = self.query.text.clone();
                if self.query.key(other) {
                    if self.query.text != before {
                        self.refilter();
                    }
                    self.dirty = true;
                }
            }
        }
        None
    }

    /// ^l: the next wordmark style, drawn at once and saved to config.json.
    fn cycle_wordmark(&mut self) {
        let style = self.config.wordmark.next();
        self.config.wordmark = style;
        self.dirty = true;
        let (width, height) = self.size;
        let bare = compute_layout(width, height, self.preview_showing, 0);
        let shown = match style.resolve(bare.inner_width, bare.list_height) {
            Style::Search if style != Style::Search => " (needs a larger popup)",
            _ => "",
        };
        match Config::store("wordmark", pyjson::Value::str(style.name())) {
            Ok(_) => self.flash(format!("wordmark: {}{}", style.name(), shown), "accent"),
            Err(error) => self.flash(format!("wordmark: {} — not saved: {}", style.name(), error), "blocked"),
        }
    }

    fn open_selected_pr(&mut self) {
        let Some(index) = self.selected_index() else { return };
        match &self.prs[index].pr {
            Some(found) => {
                let (url, number) = (found.url.clone(), found.number);
                match pr::open_url(&url) {
                    Ok(()) => self.flash(format!("opened #{} in the browser", number), "accent"),
                    Err(error) => self.flash(format!("{} — {}", error, url), "blocked"),
                }
            }
            None => self.flash("no pull request for this worktree", "muted"),
        }
    }

    fn enter_agent_mode(&mut self, target: Target) {
        if let Target::Running(index) = target {
            let kinds: Vec<String> = self.agents[index].iter().map(agent_target).collect();
            let rows = (0..kinds.len()).collect();
            self.mode = Mode::Agent { target, edit: LineEdit::default(), kinds, rows, selected: 0, preset: 0 };
            self.dirty = true;
            return;
        }
        let mut kinds = session::installed_agents(&self.config.agents);
        if let Some(default) = &self.config.default_agent {
            if let Some(pos) = kinds.iter().position(|k| k == default) {
                let kind = kinds.remove(pos);
                kinds.insert(0, kind);
            }
        }
        if matches!(target, Target::New(_)) {
            kinds.push(SHELL.to_string());
        }
        if kinds.is_empty() {
            self.flash("no supported agent found on PATH", "blocked");
            return;
        }
        let rows = (0..kinds.len()).collect();
        self.mode = Mode::Agent { target, edit: LineEdit::default(), kinds, rows, selected: 0, preset: 0 };
        self.dirty = true;
    }

    fn preset_item(preset: &Preset) -> Item {
        let first_line = || preset.prompt.as_deref().unwrap_or("").lines().find(|line| !line.trim().is_empty()).unwrap_or("").to_string();
        let detail = preset.description.clone().unwrap_or_else(first_line);
        let mut meta = Vec::new();
        if preset.ask {
            meta.push(Meta::new("muted", "asks"));
        }
        if let Some(agent) = &preset.agent {
            meta.push(Meta::new("muted", agent.clone()));
        }
        if preset.repo {
            meta.push(Meta::new("accent", "repo"));
        }
        Item::new(preset.name.clone(), detail.clone(), String::new(), None, meta, vec![detail])
    }

    /// The user's presets plus the open repository's `.hertrees/presets/`.
    fn load_presets(&self) -> Vec<Preset> {
        presets::load(self.presets_dir.as_deref(), self.project.as_ref().map(|project| project.repo_root.as_str()))
    }

    /// Keep the agent's chosen template by name while nested in a preset editor.
    fn reload_presets(&mut self) {
        fn remap(mode: &mut Mode, old: &[Preset], new: &[Preset]) {
            match mode {
                Mode::Agent { preset, .. } => {
                    *preset = old.get(*preset).and_then(|chosen| new.iter().position(|p| p.name == chosen.name)).unwrap_or(0);
                }
                Mode::Presets { previous, .. } | Mode::PresetName { previous, .. } => remap(previous, old, new),
                _ => {}
            }
        }
        let loaded = self.load_presets();
        remap(&mut self.mode, &self.presets, &loaded);
        self.presets = loaded;
        self.presets_refilter();
    }

    fn presets_refilter(&mut self) {
        let Mode::Presets { edit, rows, selected, .. } = &mut self.mode else { return };
        let terms: Vec<Term> = split_query(&edit.text).iter().map(|term| Term::new(term)).collect();
        let mut scored: Vec<_> = self.presets.iter().enumerate().filter(|(_, preset)| preset.path.is_some())
            .filter_map(|(index, preset)| {
                let (score, fields) = score_item(&terms, &Self::preset_item(preset))?;
                Some((score, Row { item: index, positions: fields.get(&0).cloned().unwrap_or_default(), field_positions: fields }))
            }).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        *rows = scored.into_iter().map(|(_, row)| row).collect();
        *selected = 0;
        self.offset = 0;
        self.dirty = true;
    }

    fn select_preset_file(&mut self, name: &str) {
        if let Mode::Presets { edit, .. } = &mut self.mode {
            *edit = LineEdit::default();
        }
        self.presets_refilter();
        if let Mode::Presets { rows, selected, .. } = &mut self.mode {
            *selected = rows.iter().position(|row| self.presets[row.item].name == name).unwrap_or(0);
        }
    }

    fn enter_presets(&mut self) {
        self.reload_presets();
        let previous = Box::new(std::mem::replace(&mut self.mode, Mode::List));
        self.mode = Mode::Presets { previous, edit: LineEdit::default(), rows: Vec::new(), selected: 0, confirm: None };
        self.status = None;
        self.presets_refilter();
    }

    fn enter_preset_name(&mut self, action: PresetAction) {
        if self.presets_dir.is_none() {
            self.flash("no configuration directory available", "blocked");
            return;
        }
        let previous = Box::new(std::mem::replace(&mut self.mode, Mode::List));
        self.mode = Mode::PresetName { previous, action, edit: LineEdit::default() };
        self.status = None;
        self.dirty = true;
    }

    fn restore_preset_previous(&mut self) {
        match std::mem::replace(&mut self.mode, Mode::List) {
            Mode::Presets { previous, .. } | Mode::PresetName { previous, .. } => self.mode = *previous,
            other => self.mode = other,
        }
        self.offset = 0;
        self.dirty = true;
    }

    fn handle_presets_key(&mut self, name: &str) -> Option<Outcome> {
        if matches!(name, "ctrl+c" | "ctrl+q") {
            return Some(Outcome::Cancel);
        }
        let Mode::Presets { rows, selected, confirm, .. } = &mut self.mode else { return None };
        if let Some(armed) = confirm.take() {
            self.dirty = true;
            if name == "enter" {
                let result = self.presets_dir.as_deref().ok_or_else(|| "no configuration directory available".to_string())
                    .and_then(|dir| presets::delete(dir, &armed));
                match result {
                    Ok(()) => {
                        self.reload_presets();
                        self.flash(format!("deleted “{}”", armed), "accent");
                    }
                    Err(error) => self.flash(error, "blocked"),
                }
            }
            return None;
        }
        let chosen = rows.get(*selected).map(|row| self.presets[row.item].clone());
        match name {
            "esc" | "ctrl+g" => self.restore_preset_previous(),
            "enter" => return Some(Outcome::Edit { path: chosen?.path?, created: false }),
            "ctrl+x" => self.enter_preset_name(PresetAction::New),
            "ctrl+r" | "delete" | "ctrl+d" if chosen.as_ref().is_some_and(|chosen| chosen.repo) => {
                self.flash("a repo preset: rename or delete it in the repository's .hertrees/presets/", "muted");
            }
            "ctrl+r" => {
                if let Some(chosen) = chosen {
                    self.enter_preset_name(PresetAction::Rename(chosen.name));
                }
            }
            "delete" | "ctrl+d" => {
                if let Some(chosen) = chosen {
                    if let Mode::Presets { confirm, .. } = &mut self.mode {
                        *confirm = Some(chosen.name);
                    }
                }
            }
            "up" | "ctrl+p" | "ctrl+k" => self.move_by(-1, true),
            "down" | "ctrl+n" | "ctrl+j" => self.move_by(1, true),
            "pgup" | "alt+v" => self.move_by(-self.page(), false),
            "pgdn" | "ctrl+v" => self.move_by(self.page(), false),
            "ctrl+o" => {
                self.preview_enabled = !self.preview_showing;
                self.preview_chosen = true;
            }
            other => {
                if let Mode::Presets { edit, .. } = &mut self.mode {
                    let before = edit.text.clone();
                    if edit.key(other) && edit.text != before {
                        self.presets_refilter();
                    }
                }
            }
        }
        self.dirty = true;
        None
    }

    fn handle_preset_name_key(&mut self, name: &str) -> Option<Outcome> {
        match name {
            "ctrl+c" | "ctrl+q" => return Some(Outcome::Cancel),
            "esc" | "ctrl+g" => self.restore_preset_previous(),
            "enter" => {
                let Mode::PresetName { edit, action, .. } = &self.mode else { return None };
                let Some(dir) = self.presets_dir.as_deref() else { return None };
                let result = presets::check_name(&edit.text).and_then(|name| {
                    let path = match action {
                        PresetAction::New => presets::create(dir, &name, "")?,
                        PresetAction::Rename(old) => presets::rename(dir, old, &name)?,
                        PresetAction::Save(prompt) => presets::create(dir, &name, prompt)?,
                    };
                    Ok((name, path))
                });
                let (name, path) = match result {
                    Ok(saved) => saved,
                    Err(error) => {
                        self.flash(error, "blocked");
                        return None;
                    }
                };
                let Mode::PresetName { previous, action, .. } = std::mem::replace(&mut self.mode, Mode::List) else { unreachable!() };
                self.mode = *previous;
                self.reload_presets();
                match action {
                    PresetAction::Save(_) => {
                        let Mode::Custom { target, kind, .. } = std::mem::replace(&mut self.mode, Mode::List) else { unreachable!() };
                        self.enter_agent_mode(target);
                        if let Mode::Agent { kinds, rows, selected, preset, .. } = &mut self.mode {
                            *selected = rows.iter().position(|&row| kinds[row] == kind).unwrap_or(0);
                            *preset = self.presets.iter().position(|preset| preset.name == name).unwrap_or(0);
                        }
                        self.flash(format!("saved preset “{}”", name), "accent");
                    }
                    PresetAction::New => {
                        self.select_preset_file(&name);
                        return Some(Outcome::Edit { path, created: true });
                    }
                    PresetAction::Rename(_) => {
                        self.select_preset_file(&name);
                        self.flash(format!("renamed to “{}”", name), "accent");
                    }
                }
            }
            other => {
                if let Mode::PresetName { edit, .. } = &mut self.mode {
                    edit.key(other);
                }
            }
        }
        self.dirty = true;
        None
    }

    pub fn edited(&mut self, path: &Path, created: bool, result: Result<(), String>) {
        let name = path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let empty = created && std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).trim().is_empty()).unwrap_or(false);
        let discarded = if empty { std::fs::remove_file(path).map_err(|error| error.to_string()) } else { Ok(()) };
        self.reload_presets();
        self.select_preset_file(&name);
        match discarded.and(result) {
            Err(error) => self.flash(error, "blocked"),
            Ok(()) if empty => self.flash("empty preset discarded", "muted"),
            Ok(()) if !path.exists() => self.flash(format!("preset “{}” no longer exists", name), "blocked"),
            Ok(()) => self.flash(format!("saved “{}”", name), "accent"),
        }
        self.invalidate();
    }

    fn base_branch(&self) -> Option<String> {
        self.project.as_ref().and_then(|p| p.default_branch.clone())
    }

    fn handle_branch_key(&mut self, name: &str) -> Option<Outcome> {
        let Mode::Branch(edit) = &mut self.mode else { return None };
        match name {
            "ctrl+c" | "ctrl+q" => return Some(Outcome::Cancel),
            "esc" | "ctrl+g" => self.mode = Mode::List,
            "enter" => {
                let branch = pystr::strip(&edit.text).to_string();
                if branch.is_empty() {
                    return None;
                }
                if let Some(existing) = self.worktrees.iter().find(|w| w.branch.as_deref() == Some(branch.as_str())) {
                    let path = self.display_path(&existing.path);
                    self.flash(format!("{} is already checked out in {}", branch, path), "blocked");
                    return None;
                }
                let root = self.project.as_ref().map(|p| p.repo_root.clone()).unwrap_or_default();
                if let Err(error) = session::validate_branch(&root, &branch) {
                    self.flash(error, "blocked");
                    return None;
                }
                self.enter_agent_mode(Target::New(branch));
            }
            other => {
                edit.key(other);
            }
        }
        self.dirty = true;
        None
    }

    fn agent_refilter(&mut self) {
        let Mode::Agent { edit, kinds, rows, selected, .. } = &mut self.mode else { return };
        let query = pystr::lower(pystr::strip(&edit.text));
        *rows = (0..kinds.len()).filter(|&i| query.is_empty() || kinds[i].contains(&query)).collect();
        *selected = 0;
        self.dirty = true;
    }

    fn handle_agent_key(&mut self, name: &str) -> Option<Outcome> {
        let Mode::Agent { target, edit, kinds, rows, selected, preset } = &mut self.mode else { return None };
        match name {
            "ctrl+c" | "ctrl+q" => return Some(Outcome::Cancel),
            "esc" | "ctrl+g" => {
                self.mode = match target {
                    Target::New(branch) => {
                        let len = char_len(branch);
                        Mode::Branch(LineEdit { text: branch.clone(), cursor: len })
                    }
                    Target::Existing(_) | Target::Running(_) => Mode::List,
                };
            }
            "ctrl+e" => self.enter_presets(),
            "tab" | "shift+tab" => {
                let count = self.presets.len();
                *preset = if name == "tab" { (*preset + 1) % count } else { (*preset + count - 1) % count };
                // A preset's `agent:` setting picks that agent while it is chosen.
                if let Some(wanted) = self.presets[*preset].agent.as_deref() {
                    if !matches!(target, Target::Running(_)) {
                        if let Some(position) = rows.iter().position(|&row| kinds[row] == wanted) {
                            *selected = position;
                        }
                    }
                }
            }
            "up" | "ctrl+p" | "ctrl+k" => self.move_by(-1, true),
            "down" | "ctrl+n" | "ctrl+j" => self.move_by(1, true),
            "enter" | "ctrl+o" => {
                let Some(&row) = rows.get(*selected) else { return None };
                let kind = kinds[row].clone();
                let target = target.clone();
                let chosen = *preset;
                let template = self.presets[chosen].prompt.clone();
                if kind == SHELL {
                    self.launch(target, kind, String::new(), String::new(), Vec::new());
                    return None;
                }
                // ^o always adds text under the preset; ⏎ does when the preset
                // asks for it (custom, `ask: true`, or an `{input}` slot).
                let opens = name == "ctrl+o"
                    || self.presets[chosen].ask
                    || template.as_deref().map(|t| t.contains("{input}")).unwrap_or(true);
                if opens {
                    self.open_editor(target, kind, chosen);
                } else {
                    self.submit(target, kind, template.unwrap_or_default(), String::new());
                }
            }
            other => {
                let before = edit.text.clone();
                if edit.key(other) && edit.text != before {
                    self.agent_refilter();
                }
            }
        }
        self.dirty = true;
        None
    }

    fn open_editor(&mut self, target: Target, kind: String, preset: usize) {
        self.history = presets::history();
        self.mode = Mode::Custom { target, kind, edit: TextEdit::default(), preset, recall: None, completion: None };
        self.status = None;
        self.dirty = true;
    }

    /// The preset text the editor shows above the user's own.
    fn editor_template(&self, preset: usize) -> String {
        self.presets.get(preset).and_then(|preset| preset.prompt.clone()).unwrap_or_default()
    }

    /// Back from the editor to the agent picker, on the same agent and preset.
    fn leave_editor(&mut self) {
        let Mode::Custom { target, kind, preset: chosen, .. } = std::mem::replace(&mut self.mode, Mode::List) else { return };
        self.enter_agent_mode(target);
        if let Mode::Agent { kinds, rows, selected, preset, .. } = &mut self.mode {
            *selected = rows.iter().position(|&row| kinds[row] == kind).unwrap_or(0);
            *preset = chosen.min(self.presets.len() - 1);
        }
    }

    fn handle_custom_key(&mut self, name: &str) -> Option<Outcome> {
        let template = match &self.mode {
            Mode::Custom { preset, .. } => self.editor_template(*preset),
            _ => return None,
        };
        let Mode::Custom { target, kind, edit, recall, completion, .. } = &mut self.mode else { return None };
        if !matches!(name, "tab" | "shift+tab") {
            *completion = None;
        }
        match name {
            "ctrl+c" | "ctrl+q" => return Some(Outcome::Cancel),
            "esc" | "ctrl+g" => self.leave_editor(),
            "ctrl+s" => {
                let prompt = session::compose(&template, &edit.text);
                if prompt.trim().is_empty() {
                    self.flash("type a prompt before saving a preset", "blocked");
                } else {
                    self.enter_preset_name(PresetAction::Save(prompt));
                }
            }
            "enter" => {
                let addition = pystr::strip(&edit.text).to_string();
                if template.is_empty() && addition.is_empty() {
                    return None;
                }
                let (target, kind) = (target.clone(), kind.clone());
                let prompt = session::compose(&template, &addition);
                self.submit(target, kind, prompt, addition);
            }
            "tab" | "shift+tab" => {
                let extra = session::fields(&template);
                let step = if name == "tab" { 1 } else { -1 };
                match complete(edit, completion, step, &extra) {
                    Ok(name) => {
                        let about = session::PLACEHOLDERS.iter().find(|(known, _)| *known == name)
                            .map(|(_, about)| *about).unwrap_or("you type its value before sending");
                        self.flash(format!("{{{}}} — {}", name, about), "accent");
                    }
                    Err(error) => self.flash(error, "muted"),
                }
            }
            "up" | "down" => {
                if !edit.key(name) {
                    recall_history(&self.history, edit, recall, name == "up");
                }
            }
            other => {
                edit.key(other);
            }
        }
        self.dirty = true;
        None
    }

    /// Ask for the prompt's own placeholders first, if it has any, then start.
    fn submit(&mut self, target: Target, kind: String, prompt: String, addition: String) {
        let names = session::fields(&prompt);
        if names.is_empty() {
            self.launch(target, kind, prompt, addition, Vec::new());
            return;
        }
        let back = Box::new(std::mem::replace(&mut self.mode, Mode::List));
        self.mode = Mode::Fields { target, kind, prompt, addition, names, values: Vec::new(), edit: LineEdit::default(), back };
        self.status = None;
        self.dirty = true;
    }

    fn handle_fields_key(&mut self, name: &str) -> Option<Outcome> {
        let Mode::Fields { names, values, edit, .. } = &mut self.mode else { return None };
        match name {
            "ctrl+c" | "ctrl+q" => return Some(Outcome::Cancel),
            "esc" | "ctrl+g" => {
                if let Mode::Fields { back, .. } = std::mem::replace(&mut self.mode, Mode::List) {
                    self.mode = *back;
                }
            }
            "enter" => {
                let value = pystr::strip(&edit.text).to_string();
                if value.is_empty() {
                    let field = names[values.len()].clone();
                    self.flash(format!("type a value for {{{}}}", field), "blocked");
                    return None;
                }
                values.push(value);
                *edit = LineEdit::default();
                if values.len() == names.len() {
                    let Mode::Fields { target, kind, prompt, addition, names, values, .. } = std::mem::replace(&mut self.mode, Mode::List) else { unreachable!() };
                    self.launch(target, kind, prompt, addition, names.into_iter().zip(values).collect());
                }
            }
            other => {
                edit.key(other);
            }
        }
        self.dirty = true;
        None
    }

    fn prompt_vars(&self, target: &Target) -> PromptVars {
        let project = self.project.clone().unwrap_or_default();
        match target {
            Target::Existing(index) | Target::Running(index) => {
                session::prompt_vars(&project, &self.worktrees[*index], None, Some(&self.prs[*index]))
            }
            Target::New(branch) => session::prompt_vars(
                &project,
                &Worktree { branch: Some(branch.clone()), path: "{path}".into(), ..Worktree::default() },
                None,
                None,
            ),
        }
    }

    /// Start the session (or prompt the running agent) on a thread; the UI
    /// shows its progress meanwhile. `fields` are the values the user typed.
    fn launch(&mut self, target: Target, kind: String, template: String, addition: String, fields: Vec<(String, String)>) {
        let Some(project) = self.project.clone() else { return };
        presets::remember(&addition);
        let mut vars = self.prompt_vars(&target);
        for (name, value) in &fields {
            vars.set(name, value.clone());
        }
        let (checkout, pr_url) = match &target {
            Target::Existing(index) | Target::Running(index) => {
                (Some(self.worktrees[*index].path.clone()), self.prs[*index].pr.as_ref().map(|pr| pr.url.clone()))
            }
            Target::New(_) => (None, None),
        };
        let base = self.base_branch();
        let worktree = match &target {
            Target::Existing(index) | Target::Running(index) => Some(self.worktrees[*index].clone()),
            Target::New(_) => None,
        };
        let taken = self.agent_names.clone();
        let mut client = self.client.clone();
        let tx = self.tx.clone();
        self.mode = Mode::Busy(match target {
            Target::Running(_) => format!("sending to {}…", kind),
            _ => format!("starting {}…", kind),
        });
        self.dirty = true;
        std::thread::spawn(move || {
            session::fill_lazy(&mut vars, &template, checkout.as_deref(), pr_url.as_deref());
            let prompt = if template.is_empty() { String::new() } else { session::expand_prompt(&template, &vars) };
            if let Target::Running(_) = target {
                let result = session::send_to(&mut client, &kind, &prompt).map(|deferred| (kind, deferred));
                let _ = tx.send(Msg::Sent(result));
                return;
            }
            let progress_tx = tx.clone();
            let mut progress = move |text: &str| {
                let _ = progress_tx.send(Msg::Progress(text.to_string()));
            };
            let prompt = if prompt.is_empty() { None } else { Some(prompt.as_str()) };
            let result = match (&target, worktree) {
                (Target::New(branch), _) if kind == SHELL => session::create_shell(&mut client, &project, branch, base.as_deref()),
                (Target::New(branch), _) => {
                    session::start_new(&mut client, &project, branch, base.as_deref(), &kind, prompt, &taken, &mut progress)
                }
                (Target::Existing(_), Some(wt)) => {
                    session::start_existing(&mut client, &project, &wt, &kind, prompt, &taken, &mut progress)
                }
                (_, None) | (Target::Running(_), _) => Err("worktree disappeared".into()),
            };
            let _ = tx.send(Msg::Done(result));
        });
    }

    fn insert(&mut self, text: &str, pasted: bool) {
        match &mut self.mode {
            Mode::List => {
                if self.query.text.is_empty() && text == "@" {
                    self.scope = "agents";
                    self.selected = 0;
                    self.refilter();
                    return;
                }
                if self.query.insert(text, pasted) {
                    self.refilter();
                }
            }
            Mode::Branch(edit) => {
                // Branch names never hold spaces: type them as dashes.
                let text = text.replace(' ', "-");
                edit.insert(&text, pasted);
            }
            Mode::Agent { edit, .. } => {
                if edit.insert(text, pasted) {
                    self.agent_refilter();
                }
            }
            Mode::Custom { edit, completion, .. } => {
                *completion = None;
                edit.insert(text, pasted);
            }
            Mode::Fields { edit, .. } => {
                edit.insert(text, pasted);
            }
            Mode::Presets { edit, confirm, .. } => {
                if confirm.take().is_some() {
                    self.dirty = true;
                    return;
                }
                if edit.insert(text, pasted) {
                    self.presets_refilter();
                }
            }
            Mode::PresetName { edit, .. } => {
                edit.insert(text, pasted);
            }
            Mode::Busy(_) => return,
        }
        self.dirty = true;
    }

    fn handle_mouse(&mut self, kind: MouseKind, x: i128, y: i128) -> Option<Outcome> {
        let row = y as isize - 1;
        let col = x as isize - 1;
        match kind {
            MouseKind::WheelUp => self.move_by(-3, false),
            MouseKind::WheelDown => self.move_by(3, false),
            MouseKind::Press => {
                if let Some((start, end, url)) = self.links.get(&row) {
                    if *start <= col && col < *end {
                        let url = url.clone();
                        if let Err(error) = pr::open_url(&url) {
                            self.flash(format!("{} — {}", error, url), "blocked");
                        } else {
                            self.flash("opened in the browser", "accent");
                        }
                        return None;
                    }
                }
                let index = row - self.list_top;
                if index < 0 || index >= self.list_height {
                    return None;
                }
                let position = self.offset + index as usize;
                match &mut self.mode {
                    Mode::List => {
                        if position < self.rows.len() {
                            if position == self.selected {
                                return self.handle_list_key("enter");
                            }
                            self.selected = position;
                        }
                    }
                    Mode::Agent { rows, selected, .. } => {
                        if position < rows.len() {
                            if position == *selected {
                                return self.handle_agent_key("enter");
                            }
                            *selected = position;
                        }
                    }
                    Mode::Presets { rows, selected, confirm, .. } => {
                        if confirm.take().is_some() {
                            self.dirty = true;
                            return None;
                        }
                        if position < rows.len() {
                            if position == *selected {
                                return self.handle_presets_key("enter");
                            }
                            *selected = position;
                        }
                    }
                    _ => {}
                }
                self.dirty = true;
            }
            MouseKind::Release => {}
        }
        None
    }

    fn consume(&mut self, events: &[Event]) -> Option<Outcome> {
        for event in events {
            let found = match event {
                Event::Key(name) => match &self.mode {
                    Mode::List => self.handle_list_key(name),
                    Mode::Branch(_) => self.handle_branch_key(name),
                    Mode::Agent { .. } => self.handle_agent_key(name),
                    Mode::Custom { .. } => self.handle_custom_key(name),
                    Mode::Fields { .. } => self.handle_fields_key(name),
                    Mode::Presets { .. } => self.handle_presets_key(name),
                    Mode::PresetName { .. } => self.handle_preset_name_key(name),
                    Mode::Busy(_) => None,
                },
                Event::Text(text) => {
                    self.insert(text, false);
                    None
                }
                Event::Paste(text) => {
                    self.insert(text, true);
                    None
                }
                Event::Mouse { kind, x, y } => self.handle_mouse(*kind, *x, *y),
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }

    // -- drawing --------------------------------------------------------------

    pub fn draw(&mut self, terminal: &mut Terminal) {
        let (width, height) = terminal.size();
        let rows = self.compose(width as isize, height as isize);
        let payload = self.painter.frame(&rows, width.max(0) as usize, height.max(0) as usize);
        if !payload.is_empty() {
            terminal.write(&payload);
            terminal.flush();
        }
        self.dirty = false;
    }

    pub fn invalidate(&mut self) {
        self.painter.reset();
        self.dirty = true;
    }

    fn compose(&mut self, width: isize, height: isize) -> Vec<String> {
        self.size = (width, height);
        let preview_on = self.preview_enabled && self.config.fits_preview((width - 2).max(0), self.preview_chosen);
        self.preview_showing = preview_on;
        // A pixel wordmark takes rows above the input line when the list keeps
        // enough of its own; otherwise the name falls back to the search line.
        let bare = compute_layout(width, height, preview_on, 0);
        let wordmark = self.config.wordmark.resolve(bare.inner_width, bare.list_height);
        let layout = compute_layout(width, height, preview_on, wordmark.rows());
        self.list_top = layout.list_top;
        self.list_height = layout.list_height;
        self.scroll_into_view(layout.list_height);
        self.links.clear();

        let agents = self.agents.iter().map(Vec::len).sum();
        let mut rows: Vec<String> =
            wordmark::render(&self.theme, wordmark, layout.inner_width, crate::cli::VERSION, self.worktrees.len(), agents)
                .into_iter()
                .map(|row| format!(" {}", row))
                .collect();
        rows.push(format!(" {}", self.input_line(&layout, wordmark)));
        if layout.rules {
            rows.push(format!(" {}", render::render_rule(&self.theme, layout.inner_width)));
        }
        let preview_body =
            if layout.preview_width != 0 { self.preview(layout.preview_width, layout.list_height) } else { Vec::new() };
        let list_body = self.list_rows(&layout);
        for index in 0..layout.list_height.max(0) as usize {
            let left = list_body.get(index).map(|s| s.as_str()).unwrap_or("");
            if layout.preview_width != 0 {
                let right = preview_body.get(index).map(|s| s.as_str()).unwrap_or("");
                let separator = self.theme.paint("unknown", render::SEPARATOR);
                rows.push(format!(" {}{}{}", pad(left, layout.list_width), separator, right));
            } else {
                rows.push(format!(" {}", left));
            }
        }
        if layout.footer {
            if layout.rules {
                rows.push(format!(" {}", render::render_rule(&self.theme, layout.inner_width)));
            }
            rows.push(format!(" {}", self.footer(layout.inner_width)));
        }
        rows
    }

    fn input_line(&self, layout: &Layout, wordmark: Style) -> String {
        let width = layout.inner_width;
        match &self.mode {
            Mode::List | Mode::Busy(_) => render::render_search(
                &self.theme,
                &self.query.text,
                self.query.cursor,
                width,
                PLACEHOLDER,
                PLACEHOLDER_SHORT,
                scope_chip(self.scope),
                wordmark == Style::Search,
            ),
            Mode::Branch(edit) => {
                let base = self.base_branch().unwrap_or_else(|| "HEAD".into());
                let placeholder = format!("branch name for the new worktree — from {}", base);
                render::render_input(&self.theme, &edit.text, edit.cursor, width, &placeholder, "⏎ next · esc back", "new worktree ❯")
            }
            Mode::Agent { target, edit, preset, .. } => {
                let (verb, prompt) = if matches!(target, Target::Running(_)) { ("send to an agent", "send ❯") } else { ("start an agent", "agent ❯") };
                let placeholder = format!("{} in {}…    ⇥ preset · ^o add text", verb, self.target_name(target));
                let chosen = &self.presets[*preset];
                let chip = format!("preset: {}{}", chosen.name, if chosen.repo { " (repo)" } else { "" });
                render::render_input(&self.theme, &edit.text, edit.cursor, width, &placeholder, &chip, prompt)
            }
            Mode::Custom { target, kind, .. } => {
                let title = match target {
                    Target::Running(_) => format!("send ❯ prompt for {}", kind),
                    _ => format!("prompt ❯ {} in {}", kind, self.target_name(target)),
                };
                render::render_status(&self.theme, width, "accent", &title, "⏎ send · ^j new line · esc back")
            }
            Mode::Fields { names, values, edit, .. } => {
                let name = &names[values.len().min(names.len() - 1)];
                let placeholder = format!("value for {{{}}}", name);
                let chip = format!("{}/{} · ⏎ next · esc back", values.len() + 1, names.len());
                render::render_input(&self.theme, &edit.text, edit.cursor, width, &placeholder, &chip, &format!("{} ❯", name))
            }
            Mode::Presets { edit, .. } => {
                render::render_input(&self.theme, &edit.text, edit.cursor, width, "search presets…", "", "presets ❯")
            }
            Mode::PresetName { edit, action, .. } => {
                let (prompt, placeholder, chip) = match action {
                    PresetAction::Rename(name) => ("rename ❯", format!("new name for “{}”", name), "⏎ save · esc keep"),
                    PresetAction::New => ("new preset ❯", "preset name".into(), "⏎ edit · esc keep"),
                    PresetAction::Save(_) => ("save as ❯", "preset name".into(), "⏎ save · esc keep"),
                };
                render::render_input(&self.theme, &edit.text, edit.cursor, width, &placeholder, chip, prompt)
            }
        }
    }

    fn target_name(&self, target: &Target) -> String {
        match target {
            Target::Existing(index) | Target::Running(index) => self.worktrees[*index].title(),
            Target::New(branch) => branch.clone(),
        }
    }

    fn scroll_into_view(&mut self, height: isize) {
        let (count, selected) = match &self.mode {
            Mode::Agent { rows, selected, .. } => (rows.len(), *selected),
            Mode::Presets { rows, selected, .. } => (rows.len(), *selected),
            _ => (self.rows.len(), self.selected),
        };
        if count == 0 {
            self.offset = 0;
            return;
        }
        let (selected, mut offset) = (selected as isize, self.offset as isize);
        if selected < offset {
            offset = selected;
        } else if selected >= offset + height {
            offset = selected - height + 1;
        }
        self.offset = offset.min((count as isize - height).max(0)).max(0) as usize;
    }

    fn empty_state(&self, layout: &Layout) -> Vec<String> {
        let filter = if self.scope != "all" { scope_chip(self.scope) } else { "" };
        let quoted;
        let headline;
        let lines: Vec<(&str, &'static str, bool)> = if let Some(error) = &self.load_error {
            vec![("not a git project", "text", true), (error.as_str(), "muted", false), ("open the picker from a pane inside a repo", "unknown", false)]
        } else if self.items.is_empty() {
            vec![("no worktrees", "text", true), ("^x makes one", "unknown", false)]
        } else if !self.query.text.is_empty() {
            quoted = format!("for “{}”", self.query.text);
            vec![("no matches", "text", true), (quoted.as_str(), "muted", false), ("^u clears the query", "unknown", false)]
        } else if !filter.is_empty() {
            headline = format!("nothing in {} filter", filter);
            vec![(headline.as_str(), "text", true), ("⇥ next filter · ⌫ back to everything", "unknown", false)]
        } else {
            vec![("no matches", "text", true)]
        };
        let mut empty = render::render_empty_state(&self.theme, layout.list_width, &lines, filter);
        // Nothing to list at all: show the mark above the message when it fits.
        if self.load_error.is_some() || self.items.is_empty() {
            let mut mark = render::render_mark(&self.theme, layout.list_width);
            if !mark.is_empty() && (mark.len() + 1 + empty.len()) as isize <= layout.list_height {
                mark.push(String::new());
                mark.append(&mut empty);
                empty = mark;
            }
        }
        let top = ((layout.list_height - empty.len() as isize).max(0) / 2) as usize;
        let mut out = vec![String::new(); top];
        out.extend(empty);
        out
    }

    fn list_rows(&mut self, layout: &Layout) -> Vec<String> {
        let manager = match &self.mode {
            Mode::Presets { rows, selected, .. } => Some((rows, *selected)),
            Mode::PresetName { previous, .. } => match previous.as_ref() {
                Mode::Presets { rows, selected, .. } => Some((rows, *selected)),
                _ => None,
            },
            _ => None,
        };
        if let Some((rows, selected)) = manager {
            if rows.is_empty() {
                return render::render_empty_state(&self.theme, layout.list_width,
                    &[("no presets", "text", true), ("^x makes one", "unknown", false)], "");
            }
            return (0..layout.list_height.max(0) as usize).map(|index| {
                let position = self.offset + index;
                rows.get(position).map(|row| render::render_row(&self.theme, &Self::preset_item(&self.presets[row.item]),
                    row, layout.list_width, position == selected).text).unwrap_or_default()
            }).collect();
        }
        if let Mode::Custom { .. } | Mode::Fields { .. } = &self.mode {
            return self.editor_rows(layout);
        }
        let agent_rows: Option<(Vec<Item>, usize)> = match &self.mode {
            Mode::Agent { target, kinds, rows, selected, .. } => {
                let items = rows.iter().map(|&i| self.agent_item(target, &kinds[i])).collect();
                Some((items, *selected))
            }
            _ => None,
        };
        if let Some((items, selected)) = agent_rows {
            let blank = Row { item: 0, positions: Vec::new(), field_positions: BTreeMap::new() };
            let mut out = Vec::new();
            for index in 0..layout.list_height.max(0) as usize {
                let position = self.offset + index;
                out.push(match items.get(position) {
                    Some(item) => render::render_row(&self.theme, item, &blank, layout.list_width, position == selected).text,
                    None => String::new(),
                });
            }
            return out;
        }

        if self.rows.is_empty() {
            return self.empty_state(layout);
        }
        let marks = scrollbar_column(self.rows.len() as isize, layout.list_height, self.offset as isize, layout.list_height);
        let content_width = layout.list_width - if marks.is_empty() { 0 } else { 1 };
        let mut out = Vec::new();
        for index in 0..layout.list_height.max(0) {
            let position = self.offset + index as usize;
            let mut body = if position >= self.rows.len() {
                String::new()
            } else {
                let row = &self.rows[position];
                let rendered = render::render_row(&self.theme, &self.items[row.item], row, content_width, position == self.selected);
                if let Some((start, end, url)) = rendered.link {
                    // Rows start after the one-cell gutter.
                    self.links.insert(layout.list_top + index, (start + 1, end + 1, url));
                }
                rendered.text
            };
            if !marks.is_empty() {
                let mark = marks.get(&index).copied().unwrap_or(" ");
                body = format!("{}{}", pad(&body, content_width), self.theme.paint("unknown", mark));
            }
            out.push(body);
        }
        out
    }

    fn agent_item(&self, target: &Target, kind: &str) -> Item {
        if let Target::Running(index) = target {
            if let Some(agent) = self.agents[*index].iter().find(|agent| agent_target(agent) == kind) {
                let meta = vec![Meta::new(render::status_role(&agent.status), render::status_word(&agent.status))];
                return Item::new(kind.to_string(), agent.kind.clone(), agent.status.clone(), Some(agent.kind.clone()), meta, Vec::new());
            }
        }
        let mut meta = Vec::new();
        if self.config.default_agent.as_deref() == Some(kind) {
            meta.push(Meta::new("accent", "default"));
        }
        let (agent, detail) = if kind == SHELL { (None, "no agent, just the worktree") } else { (Some(kind.to_string()), "") };
        Item::new(kind.to_string(), detail.to_string(), String::new(), agent, meta, Vec::new())
    }

    // -- preview ----------------------------------------------------------------

    fn preview(&self, width: isize, height: isize) -> Vec<String> {
        match &self.mode {
            Mode::Presets { rows, selected, .. } => {
                let Some(preset) = rows.get(*selected).map(|row| &self.presets[row.item]) else {
                    return Vec::new();
                };
                let body: Vec<_> = wrap_template(preset.prompt.as_deref().unwrap_or(""), width).into_iter()
                    .map(|line| vec![Segment::new("text", line)]).collect();
                let path = preset.path.as_ref().map(|path| path.to_string_lossy()).unwrap_or_default();
                render::render_preview(&self.theme, &preset.name, "", &path, &body, width, height)
            }
            Mode::PresetName { previous, action: PresetAction::Save(prompt), .. } => {
                if let Mode::Custom { target, kind, preset, .. } = previous.as_ref() {
                    return self.session_preview(target, kind, &self.presets[*preset].name, Some(prompt.clone()), width, height);
                }
                Vec::new()
            }
            Mode::PresetName { .. } => Vec::new(),
            Mode::Agent { target, kinds, rows, selected, preset, .. } => {
                let kind = rows.get(*selected).map(|&i| kinds[i].as_str()).unwrap_or("");
                let template = self.presets[*preset].prompt.clone();
                self.session_preview(target, kind, &self.presets[*preset].name, template, width, height)
            }
            Mode::Custom { target, kind, edit, preset, .. } => {
                let prompt = session::compose(&self.editor_template(*preset), &edit.text);
                self.session_preview(target, kind, &self.presets[*preset].name, Some(prompt), width, height)
            }
            Mode::Fields { target, kind, prompt, names, values, back, .. } => {
                let mut typed = PromptVars::default();
                for (name, value) in names.iter().zip(values) {
                    typed.set(name, value.clone());
                }
                let preset = match back.as_ref() {
                    Mode::Custom { preset, .. } | Mode::Agent { preset, .. } => self.presets[*preset].name.as_str(),
                    _ => "",
                };
                self.session_preview(target, kind, preset, Some(session::expand_prompt(prompt, &typed)), width, height)
            }
            Mode::Branch(edit) => {
                let base = self.base_branch().unwrap_or_else(|| "HEAD".into());
                let branch = pystr::strip(&edit.text);
                let body = vec![
                    vec![Segment::new("muted", format!("git worktree add -b {} … {}", if branch.is_empty() { "<branch>" } else { branch }, base))],
                    Vec::new(),
                    vec![Segment::new("muted", "next: pick an agent and a preset,")],
                    vec![Segment::new("muted", "or shell for just the checkout")],
                ];
                render::render_preview(&self.theme, "new worktree", branch, &format!("from {}", base), &body, width, height)
            }
            _ => match self.selected_index() {
                Some(index) => self.worktree_preview(index, width, height),
                None => vec![String::new(); height.max(0) as usize],
            },
        }
    }

    fn session_preview(&self, target: &Target, kind: &str, preset: &str, template: Option<String>, width: isize, height: isize) -> Vec<String> {
        let (subtitle, mut body) = match target {
            Target::Existing(index) => {
                let wt = &self.worktrees[*index];
                let how = if wt.open_workspace_id.is_some() { "new tab in its open workspace" } else { "opens its workspace" };
                (wt.path.clone(), vec![vec![Segment::new("muted", how)]])
            }
            Target::New(branch) => {
                let base = self.base_branch().unwrap_or_else(|| "HEAD".into());
                (
                    format!("new worktree from {}", base),
                    vec![vec![Segment::new("muted", format!("git worktree add -b {} … {}", branch, base))]],
                )
            }
            Target::Running(index) => {
                let how = if kind.is_empty() { "types into a running agent".into() } else { format!("types into {}; queued while it is busy", kind) };
                (self.worktrees[*index].path.clone(), vec![vec![Segment::new("muted", how)]])
            }
        };
        body.push(Vec::new());
        if kind == SHELL {
            body.push(vec![Segment::new("muted", "creates the worktree and focuses its shell")]);
        } else {
            body.push(vec![Segment::new("muted", "preset  "), Segment::bold("accent", preset, true)]);
            let text = match template {
                Some(t) if !t.is_empty() => {
                    session::expand_prompt(&t, &self.prompt_vars(target)).replace("{path}", "‹new worktree path›")
                }
                Some(_) if preset == "custom" => "type the prompt in the editor".into(),
                _ => "no first prompt: the agent starts idle".into(),
            };
            for line in wrap_template(&text, width) {
                body.push(vec![Segment::new("text", line)]);
            }
        }
        let title = match (target, kind.is_empty()) {
            (Target::Running(_), _) => "prompt".to_string(),
            (_, true) => "new session".to_string(),
            (_, false) => format!("{} session", kind),
        };
        render::render_preview(&self.theme, &title, &self.target_name(target), &subtitle, &body, width, height)
    }

    /// The editor's body: the agent, the preset's text dimmed (shortened when
    /// room is short), then the user's own text with its cursor. Placeholder
    /// fields list the values typed so far instead.
    fn editor_rows(&self, layout: &Layout) -> Vec<String> {
        let width = layout.list_width;
        let height = layout.list_height.max(0) as usize;
        let paint = |segments: &[Segment]| render::segments_to_text(&self.theme, segments, "");
        let (target, kind) = match &self.mode {
            Mode::Custom { target, kind, .. } | Mode::Fields { target, kind, .. } => (target, kind),
            _ => return Vec::new(),
        };
        let blank = Row { item: 0, positions: Vec::new(), field_positions: BTreeMap::new() };
        let mut top = vec![render::render_row(&self.theme, &self.agent_item(target, kind), &blank, width, true).text, String::new()];
        let mut bottom: Vec<String> = Vec::new();
        let mut header: Vec<String> = Vec::new();
        match &self.mode {
            Mode::Fields { names, values, .. } => {
                for (index, name) in names.iter().enumerate() {
                    let (role, value) = match values.get(index) {
                        Some(value) => ("text", value.clone()),
                        None if index == values.len() => ("accent", "typing above…".into()),
                        None => ("unknown", "next".into()),
                    };
                    let line = [Segment::new("muted", format!("{{{}}}  ", name)), Segment::new(role, value)];
                    header.push(truncate(&paint(&line), width));
                }
            }
            Mode::Custom { edit, preset, .. } => {
                let template = self.editor_template(*preset);
                if !template.is_empty() {
                    let chosen = &self.presets[*preset];
                    let mut title = vec![Segment::new("muted", "preset "), Segment::bold("accent", chosen.name.clone(), true)];
                    if chosen.repo {
                        title.push(Segment::new("muted", " · repo"));
                    }
                    header.push(paint(&title));
                    for line in wrap_template(&template, width - 2) {
                        header.push(paint(&[Segment::new("unknown", format!("│ {}", line))]));
                    }
                    header.push(String::new());
                    let place = if template.contains("{input}") { "your text goes at {input}" } else { "your text goes below the preset" };
                    header.push(paint(&[Segment::new("muted", place)]));
                }
                bottom = self.editor_text(edit, width);
                if height > top.len() + bottom.len() + 2 {
                    bottom.push(String::new());
                    bottom.push(paint(&[Segment::new("unknown", "⇥ placeholder · ↑ earlier prompts · ^s save as preset")]));
                }
            }
            _ => {}
        }
        // The user's text keeps its rows; the preset's text gives way first.
        let room = height.saturating_sub(top.len() + bottom.len());
        if header.len() > room {
            let hidden = header.len() + 1 - room.max(1);
            header.truncate(room.saturating_sub(1));
            if room > 0 {
                header.push(paint(&[Segment::new("unknown", format!("│ … {} more lines in the preview", hidden))]));
            }
        }
        top.extend(header);
        top.extend(bottom);
        if top.len() > height {
            let skip = top.len() - height;
            top.drain(2..(2 + skip).min(top.len()));
        }
        top
    }

    /// The editor's text wrapped to `width`, with a block cursor.
    fn editor_text(&self, edit: &TextEdit, width: isize) -> Vec<String> {
        let width = (width - 2).max(4) as usize;
        let chars = edit.chars();
        if chars.is_empty() {
            let hint = "type here: ⏎ sends, ^j new line";
            return vec![format!("  \x1b[7m \x1b[27m{}", self.theme.paint("unknown", hint))];
        }
        let mut rows: Vec<String> = Vec::new();
        let (mut line, mut used) = (String::new(), 0usize);
        let flush = |line: &mut String, rows: &mut Vec<String>| rows.push(format!("  {}", std::mem::take(line)));
        for (index, &c) in chars.iter().chain(std::iter::once(&'\n')).enumerate() {
            let at_cursor = index == edit.cursor;
            let cell = if c == '\n' { 1 } else { display_width(&c.to_string()) };
            if used + cell > width {
                flush(&mut line, &mut rows);
                used = 0;
            }
            let shown = if c == '\n' { " ".to_string() } else { c.to_string() };
            if at_cursor {
                line.push_str(&format!("\x1b[7m{}\x1b[27m", self.theme.paint("text", &shown)));
            } else if c != '\n' {
                line.push_str(&self.theme.paint("text", &shown));
            }
            used += if c == '\n' && !at_cursor { 0 } else { cell };
            if c == '\n' {
                flush(&mut line, &mut rows);
                used = 0;
            }
        }
        rows
    }

    fn worktree_preview(&self, index: usize, width: isize, height: isize) -> Vec<String> {
        let wt = &self.worktrees[index];
        let info = &self.prs[index];
        let mut body: Vec<Vec<Segment>> = Vec::new();
        let mut head = vec![Segment::bold(pr_role(info.state), pr_chip(info), true)];
        if info.via_upstream {
            if let Some(upstream) = &wt.upstream_branch {
                head.push(Segment::new("muted", format!(" · via {}", upstream)));
            }
        }
        body.push(head);
        match &info.pr {
            Some(pr) => {
                body.push(vec![Segment::new("text", pr.title.clone())]);
                let mut link = Segment::new("accent", pr.url.clone());
                link.link = Some(pr.url.clone());
                body.push(vec![link]);
                body.push(vec![Segment::new("muted", format!("{} ← {}  ^b opens it", pr.base_ref, pr.head_ref))]);
            }
            None => {
                if let Some(reason) = &info.reason {
                    body.push(vec![Segment::new("muted", reason.clone())]);
                } else if wt.is_main && wt.branch.is_some() && wt.branch == self.base_branch() {
                    body.push(vec![Segment::new("muted", "default branch: pull requests merge into it")]);
                } else if info.state == PrState::None {
                    body.push(vec![Segment::new("muted", "no pull request has this branch as its head")]);
                }
            }
        }
        body.push(Vec::new());
        if let (Some(remote), Some(branch)) = (&wt.upstream_remote, &wt.upstream_branch) {
            body.push(vec![Segment::new("muted", "tracks "), Segment::new("text", format!("{}/{}", remote, branch))]);
        }
        match &self.facts[index] {
            Some(facts) => {
                let mut parts = Vec::new();
                if let (Some(ahead), Some(behind)) = (facts.ahead, facts.behind) {
                    parts.push(format!("↑{} ↓{}", ahead, behind));
                }
                if let Some(dirty) = facts.dirty {
                    parts.push(if dirty == 0 { "clean".to_string() } else { format!("±{} changed", dirty) });
                }
                if let Some(epoch) = facts.last_commit_epoch {
                    parts.push(format!("{} ago", format_age(now_unix() - epoch)));
                }
                if !parts.is_empty() {
                    body.push(vec![Segment::new("muted", parts.join(" · "))]);
                }
                if !self.agents[index].is_empty() {
                    body.push(Vec::new());
                }
                self.agent_lines(index, &mut body);
                if !facts.recent_commits.is_empty() {
                    body.push(Vec::new());
                    for commit in &facts.recent_commits {
                        body.push(vec![Segment::new("muted", commit.clone())]);
                    }
                }
            }
            None => {
                self.agent_lines(index, &mut body);
                body.push(vec![Segment::new("unknown", "…")]);
            }
        }
        render::render_preview(&self.theme, &wt.title(), "", &wt.path, &body, width, height)
    }

    fn agent_lines(&self, index: usize, body: &mut Vec<Vec<Segment>>) {
        for agent in &self.agents[index] {
            let mut line = vec![
                Segment::new(render::status_role(&agent.status), format!("{} ", crate::theme::status_glyph(&agent.status))),
                Segment::new(render::agent_role(Some(&agent.kind)), agent.kind.clone()),
            ];
            if let Some(name) = &agent.name {
                line.push(Segment::new("muted", format!(" @{}", name)));
            }
            let word = render::status_word(&agent.status);
            if !word.is_empty() {
                line.push(Segment::new("unknown", " · "));
                line.push(Segment::new(render::status_role(&agent.status), word));
            }
            body.push(line);
        }
    }

    // -- footer -----------------------------------------------------------------

    fn footer(&self, width: isize) -> String {
        if let Mode::Busy(text) = &self.mode {
            return render::render_status(&self.theme, width, "accent", text, "");
        }
        if let Mode::Presets { confirm: Some(name), .. } = &self.mode {
            return render::render_confirm(&self.theme, width, "delete", name, "⏎ delete · esc keep");
        }
        if let Some((text, role, until)) = &self.status {
            if monotonic() < *until {
                return render::render_status(&self.theme, width, role, text, "");
            }
        }
        let (hints, counter): (Vec<(&str, &str)>, String) = match &self.mode {
            Mode::Branch(_) => (vec![("⏎", "next"), ("esc", "back"), ("^u", "clear")], "new worktree".into()),
            Mode::Agent { rows, .. } => {
                (vec![("↑↓", "agent"), ("⇥", "preset"), ("⏎", "start"), ("^o", "add text"), ("^e", "presets"), ("esc", "back")], count(rows.len(), "agent"))
            }
            Mode::Custom { target, .. } => (
                vec![("⏎", "send"), ("^j", "new line"), ("⇥", "{placeholder}"), ("↑", "history"), ("^s", "save"), ("esc", "back")],
                if matches!(target, Target::Running(_)) { "prompt".into() } else { "first prompt".into() },
            ),
            Mode::Fields { names, values, .. } => (vec![("⏎", "next"), ("esc", "back"), ("^u", "clear")], format!("field {}/{}", values.len() + 1, names.len())),
            Mode::Presets { rows, .. } => (vec![("⏎", "edit"), ("^x", "new"), ("^r", "rename"), ("⌦", "delete"), ("esc", "back")], count(rows.len(), "preset")),
            Mode::PresetName { action, .. } => (vec![("⏎", "save"), ("esc", "keep"), ("^u", "clear")],
                match action { PresetAction::Rename(_) => "renaming", PresetAction::New => "new preset", PresetAction::Save(_) => "saving preset" }.into()),
            _ => {
                let hints = if width < 52 {
                    vec![("↑↓", ""), ("⏎", ""), ("esc", "")]
                } else {
                    vec![
                        ("↑↓", "move"),
                        ("⏎", "open"),
                        ("^t", "agent"),
                        ("^s", "send"),
                        ("^x", "new"),
                        ("^b", "PR"),
                        ("⇥", "scope"),
                        ("^r", "refresh"),
                        ("^o", "preview"),
                        ("^l", "wordmark"),
                        ("esc", "quit"),
                    ]
                };
                let total = self.items.len();
                let shown = self.rows.len();
                let mut counter = if shown != total { format!("{}/{}", shown, total) } else { total.to_string() };
                if self.fetching {
                    counter = format!("PRs… · {}", counter);
                }
                (hints, counter)
            }
        };
        render::render_footer(&self.theme, width, &hints, &counter)
    }

    // -- main loop --------------------------------------------------------------

    pub fn run(&mut self, terminal: &mut Terminal) -> Outcome {
        let mut decoder = KeyDecoder::new();
        self.draw(terminal);
        let mut escape_deadline: Option<f64> = None;
        loop {
            let timeout = self.next_timeout(escape_deadline);
            if terminal.wait(timeout) {
                let data = terminal.read();
                if data.is_empty() {
                    return Outcome::Cancel;
                }
                let events = decoder.feed(&data);
                let outcome = self.consume(&events);
                escape_deadline = if decoder.pending_escape() { Some(monotonic() + ESC_TIMEOUT) } else { None };
                if let Some(outcome) = outcome {
                    return outcome;
                }
            } else if let Some(deadline) = escape_deadline {
                if monotonic() >= deadline {
                    escape_deadline = None;
                    let events = decoder.flush();
                    if let Some(outcome) = self.consume(&events) {
                        return outcome;
                    }
                }
            }
            if let Some(outcome) = self.drain() {
                return outcome;
            }
            if terminal.take_resize() {
                self.invalidate();
            }
            let now = monotonic();
            if !matches!(self.mode, Mode::Busy(_)) && now - self.last_refresh >= self.config.refresh_ms as f64 / 1000.0 {
                self.refresh_agents();
            }
            if self.status.as_ref().map(|(_, _, until)| now >= *until).unwrap_or(false) {
                self.status = None;
                self.dirty = true;
            }
            if self.dirty {
                self.draw(terminal);
            }
        }
    }

    fn next_timeout(&self, escape_deadline: Option<f64>) -> f64 {
        let now = monotonic();
        if self.pending_facts > 0 || self.fetching || matches!(self.mode, Mode::Busy(_)) {
            return 0.05;
        }
        let mut deadline = self.last_refresh + self.config.refresh_ms as f64 / 1000.0;
        if let Some(escape) = escape_deadline {
            deadline = deadline.min(escape);
        }
        if let Some((_, _, until)) = &self.status {
            deadline = deadline.min(*until);
        }
        (deadline - now).max(0.01)
    }
}

/// Score one row: every term must match one field, best field wins (title +40).
pub fn score_item(terms: &[Term], item: &Item) -> Option<(i64, BTreeMap<usize, Vec<usize>>)> {
    let mut total = 0;
    let mut found_positions: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for term in terms {
        let mut best: Option<(i64, usize, Vec<usize>)> = None;
        for (index, prepared) in item.prepared.iter().enumerate() {
            let Some(found) = match_prepared(term, prepared) else { continue };
            let value = found.score + if index == 0 { 40 } else { 0 };
            if best.as_ref().map(|(b, _, _)| value > *b).unwrap_or(true) {
                best = Some((value, index, found.positions));
            }
        }
        let (value, index, positions) = best?;
        total += value;
        found_positions.entry(index).or_default().extend(positions);
    }
    for marks in found_positions.values_mut() {
        marks.sort_unstable();
        marks.dedup();
    }
    Some((total, found_positions))
}

/// `1 preset`, `3 presets`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// Word-wrap plain text to a display width.
fn wrap(text: &str, width: isize) -> Vec<String> {
    let width = width.max(8) as usize;
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let needed = display_width(&line) + usize::from(!line.is_empty()) + display_width(word);
        if !line.is_empty() && needed > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Preserve explicit line breaks (including blank lines) in prompt templates.
fn wrap_template(text: &str, width: isize) -> Vec<String> {
    text.split('\n').flat_map(|line| {
        if line.trim().is_empty() { vec![String::new()] } else { wrap(line, width) }
    }).collect()
}

/// How herdr addresses a running agent: its name, else its pane.
fn agent_target(agent: &AgentHere) -> String {
    agent.name.clone().unwrap_or_else(|| agent.pane_id.clone())
}

/// ⇥ in the prompt editor: complete the `{name` before the cursor to a
/// placeholder, or start one (inserting `{`) when there is none. Pressing it
/// again cycles through the candidates. Returns the placeholder shown.
fn complete(edit: &mut TextEdit, completion: &mut Option<Completion>, step: isize, extra: &[String]) -> Result<String, String> {
    let mut chars = edit.chars();
    let state = match completion.take() {
        Some(state) => {
            // Remove the candidate shown last, back to the typed prefix.
            chars.drain(state.brace + 1 + char_len(&state.prefix)..edit.cursor.min(chars.len()));
            edit.cursor = state.brace + 1 + char_len(&state.prefix);
            Completion { choice: state.choice, ..state }
        }
        None => {
            let cursor = edit.cursor.min(chars.len());
            let mut start = cursor;
            while start > 0 && (chars[start - 1].is_ascii_lowercase() || chars[start - 1].is_ascii_digit() || chars[start - 1] == '_') {
                start -= 1;
            }
            let brace = if start > 0 && chars[start - 1] == '{' {
                start - 1
            } else {
                chars.insert(cursor, '{');
                edit.cursor = cursor + 1;
                cursor
            };
            let prefix: String = chars[brace + 1..edit.cursor].iter().collect();
            Completion { brace, prefix, choice: usize::MAX }
        }
    };
    // `{input}` belongs in preset files: in the user's own text it has no value.
    let candidates: Vec<String> = session::PLACEHOLDERS.iter().map(|(name, _)| name.to_string())
        .filter(|name| name != "input")
        .chain(extra.iter().cloned())
        .filter(|name| name.starts_with(&state.prefix))
        .collect();
    if candidates.is_empty() {
        edit.set(&chars, edit.cursor);
        return Err(format!("no placeholder starts with {{{}", state.prefix));
    }
    let count = candidates.len() as isize;
    let choice = if state.choice == usize::MAX {
        if step > 0 { 0 } else { candidates.len() - 1 }
    } else {
        (state.choice as isize + step).rem_euclid(count) as usize
    };
    let name = candidates[choice].clone();
    let rest: Vec<char> = format!("{}}}", &name[state.prefix.len()..]).chars().collect();
    let cursor = edit.cursor + rest.len();
    chars.splice(edit.cursor..edit.cursor, rest);
    edit.set(&chars, cursor);
    *completion = Some(Completion { choice, ..state });
    Ok(name)
}

/// ↑ on the editor's first line / ↓ on its last: step through earlier
/// prompts, keeping what was being typed to come back to.
fn recall_history(history: &[String], edit: &mut TextEdit, recall: &mut Option<(usize, String)>, older: bool) {
    let position = match (recall.as_ref(), older) {
        (None, true) if !history.is_empty() => history.len() - 1,
        (None, _) => return,
        (Some((index, _)), true) => index.saturating_sub(1),
        (Some((index, _)), false) if index + 1 < history.len() => index + 1,
        (Some(_), false) => {
            let (_, draft) = recall.take().unwrap();
            let length = char_len(&draft);
            *edit = TextEdit { text: draft, cursor: length };
            return;
        }
    };
    let draft = recall.take().map(|(_, draft)| draft).unwrap_or_else(|| edit.text.clone());
    let text = history[position].clone();
    let length = char_len(&text);
    *edit = TextEdit { text, cursor: length };
    *recall = Some((position, draft));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, EnvVars, TempDir};

    fn preset_picker(config_dir: &Path, env: &mut EnvVars) -> Picker {
        env.set("HERDR_PLUGIN_CONFIG_DIR", config_dir);
        let loaded = Loaded {
            result: Err("test without a project".into()), agents: Vec::new(), gh: Err("no remote".into()),
            cached: None, branch_times: BTreeMap::new(),
        };
        let mut picker = Picker::new(HerdrClient::new(Some("/nonexistent/hertrees-test".into()), 0.001, true),
            Config::default(), Theme::new(None, None, None), loaded);
        picker.mode = Mode::Agent {
            target: Target::New("test".into()), edit: LineEdit::default(),
            kinds: vec!["claude".into(), "codex".into()], rows: vec![0, 1], selected: 1, preset: 2,
        };
        picker
    }

    fn key(picker: &mut Picker, name: &str) -> Option<Outcome> {
        picker.consume(&[Event::Key(name.into())])
    }

    #[test]
    fn the_not_a_project_state_shows_the_mark_only_when_it_fits() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        picker.mode = Mode::List;
        let mark = render::render_mark(&picker.theme, 60);
        let tall = crate::textutil::strip_ansi(&picker.compose(80, 30).join("\n"));
        assert!(tall.contains("not a git project"));
        assert!(tall.contains(crate::textutil::strip_ansi(&mark[0]).trim()));
        let short = crate::textutil::strip_ansi(&picker.compose(80, 12).join("\n"));
        assert!(short.contains("not a git project"));
        assert!(!short.contains('█'));
    }

    fn plain(picker: &mut Picker, width: isize, height: isize) -> Vec<String> {
        picker.compose(width, height).iter().map(|row| crate::textutil::strip_ansi(row)).collect()
    }

    #[test]
    fn pixel_wordmarks_sit_above_a_plain_search_line_or_fall_back_to_it() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        picker.mode = Mode::List;
        for style in [Style::Pixel, Style::PixelSmall, Style::Braille, Style::Sextant, Style::Icon] {
            picker.config.wordmark = style;
            let rows = plain(&mut picker, 100, 30);
            let header = style.rows() as usize;
            assert_eq!(rows.len(), 30, "{style:?}");
            assert!(!rows[0].trim().is_empty(), "{style:?}: the art starts on the first row");
            assert!(rows[header].starts_with(" ❯  search worktrees"), "{style:?}: {:?}", rows[header]);
            assert!(rows[header + 1].trim_start().starts_with('─'), "{style:?}: the rule follows");
            assert_eq!(picker.list_top, header as isize + 2);
            assert_eq!(picker.list_height, 30 - 4 - header as isize);
            assert!(rows.join("\n").contains("not a git project"));
            // Too short for the art and five list rows: the search-line wordmark.
            let short = plain(&mut picker, 100, header as isize + 4 + wordmark::MIN_LIST_ROWS - 1);
            assert!(short[0].starts_with(" ❯ hertrees │ "), "{style:?}: {:?}", short[0]);
            assert_eq!(picker.list_top, 2);
            // Too narrow for the art: the same fallback, plain below 60 columns.
            let narrow = plain(&mut picker, style.min_width() + 1, 30);
            assert!(narrow[0].starts_with(" ❯ "), "{style:?}: {:?}", narrow[0]);
            assert!(!narrow[0].contains("hertrees") && !narrow[0].contains('▀'));
        }
        picker.config.wordmark = Style::Off;
        assert!(plain(&mut picker, 100, 30)[0].starts_with(" ❯  search worktrees"));
        picker.config.wordmark = Style::Search;
        assert!(plain(&mut picker, 100, 30)[0].starts_with(" ❯ hertrees │ "));
    }

    #[test]
    fn ctrl_l_cycles_the_wordmark_live_and_saves_it() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        picker.mode = Mode::List;
        plain(&mut picker, 100, 30);
        key(&mut picker, "ctrl+l");
        assert_eq!(picker.config.wordmark, Style::Pixel);
        assert_eq!(picker.status.as_ref().unwrap().0, "wordmark: pixel");
        assert_eq!(Config::load().wordmark, Style::Pixel);
        assert!(plain(&mut picker, 100, 30)[0].contains('█'), "drawn without reopening the popup");
        // A popup too small for the art says so, and still saves the choice.
        plain(&mut picker, 100, 8);
        key(&mut picker, "ctrl+l");
        assert_eq!(picker.status.as_ref().unwrap().0, "wordmark: pixel-small (needs a larger popup)");
        for _ in 2..wordmark::NAMES.len() {
            key(&mut picker, "ctrl+l");
        }
        assert_eq!((picker.config.wordmark, Config::load().wordmark), (Style::Search, Style::Search));
        std::fs::write(tmp.path().join("config.json"), "{broken").unwrap();
        key(&mut picker, "ctrl+l");
        assert_eq!(picker.config.wordmark, Style::Pixel);
        assert_eq!(picker.status.as_ref().unwrap().1, "blocked");
        assert_eq!(std::fs::read_to_string(tmp.path().join("config.json")).unwrap(), "{broken");
    }

    #[test]
    fn clicks_land_on_the_row_drawn_below_a_multi_row_wordmark() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        picker.config.wordmark = Style::Icon;
        let rows = plain(&mut picker, 100, 30);
        let top = picker.list_top as usize;
        assert_eq!(top, Style::Icon.rows() as usize + 2);
        assert!(rows[top].contains("claude") && rows[top + 1].contains("codex"), "{rows:#?}");
        // Mouse rows are 1-based: the art and input rows select nothing.
        for y in 1..=top as i128 {
            picker.consume(&[Event::Mouse { kind: MouseKind::Press, x: 5, y }]);
            let Mode::Agent { selected, .. } = &picker.mode else { panic!() };
            assert_eq!(*selected, 1, "row {y}");
        }
        picker.consume(&[Event::Mouse { kind: MouseKind::Press, x: 5, y: top as i128 + 1 }]);
        let Mode::Agent { selected, .. } = &picker.mode else { panic!() };
        assert_eq!(*selected, 0, "the click on the claude row selects it");
    }

    #[test]
    fn manager_filters_renames_and_confirms_deletion_without_losing_agent_choice() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        key(&mut picker, "ctrl+e");
        picker.insert("rsch", false);
        let Mode::Presets { rows, .. } = &picker.mode else { panic!("manager not opened") };
        assert_eq!(rows.len(), 1);
        assert_eq!(picker.presets[rows[0].item].name, "research");
        assert!(!rows[0].positions.is_empty());
        key(&mut picker, "ctrl+r");
        picker.insert("plan", false);
        key(&mut picker, "enter");
        assert!(matches!(picker.mode, Mode::PresetName { .. }));
        assert!(picker.status.as_ref().unwrap().0.contains("already exists"));
        key(&mut picker, "ctrl+u");
        picker.insert("study", false);
        key(&mut picker, "enter");
        let dir = tmp.path().join("presets");
        assert!(!presets::path(&dir, "research").exists());
        assert!(presets::path(&dir, "study").exists());
        key(&mut picker, "ctrl+d");
        assert!(crate::textutil::strip_ansi(&picker.footer(100)).contains("delete “study”?"));
        key(&mut picker, "esc");
        assert!(presets::path(&dir, "study").exists());
        key(&mut picker, "delete");
        key(&mut picker, "enter");
        assert!(!presets::path(&dir, "study").exists());
        key(&mut picker, "esc");
        let Mode::Agent { kinds, rows, selected, preset, .. } = &picker.mode else { panic!("not back at agents") };
        assert_eq!(kinds[rows[*selected]], "codex");
        assert_eq!(picker.presets[*preset].name, presets::BLANK);
        key(&mut picker, "ctrl+e");
        assert_eq!(presets::files(&dir).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["plan"]);
    }

    #[test]
    fn editor_reloads_multiline_templates_and_discards_empty_new_files() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        key(&mut picker, "ctrl+e");
        key(&mut picker, "ctrl+x");
        picker.insert("notes", false);
        let Some(Outcome::Edit { path, created: true }) = key(&mut picker, "enter") else { panic!("no editor outcome") };
        std::fs::write(&path, "first line\n\nsecond line").unwrap();
        picker.edited(&path, true, Ok(()));
        let preview = picker.preview(50, 15).join("\n");
        assert!(preview.contains("first line"));
        assert!(preview.contains("second line"));
        let Some(Outcome::Edit { path: again, created: false }) = key(&mut picker, "enter") else { panic!("not selected after editing") };
        assert_eq!(again, path);
        key(&mut picker, "ctrl+x");
        picker.insert("discard", false);
        let Some(Outcome::Edit { path: empty, created: true }) = key(&mut picker, "enter") else { panic!("no editor outcome") };
        picker.edited(&empty, true, Ok(()));
        assert!(!empty.exists());
        assert_eq!(picker.status.as_ref().unwrap().0, "empty preset discarded");
        picker.edited(&path, false, Err("editor failed".into()));
        assert_eq!(picker.status.as_ref().unwrap().1, "blocked");
        assert!(path.exists());
        assert_eq!(wrap_template("first\n\nsecond line", 8), ["first", "", "second", "line"]);
    }

    #[test]
    fn custom_save_preserves_prompt_and_selected_agent() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("codex");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        env.set("PATH", &bin);
        let custom = picker.presets.len() - 1;
        picker.mode = Mode::Custom { target: Target::New("test".into()), kind: "codex".into(), edit: TextEdit::default(), preset: custom, recall: None, completion: None };
        key(&mut picker, "ctrl+s");
        assert!(matches!(picker.mode, Mode::Custom { .. }));
        picker.insert("Review {branch} carefully", false);
        key(&mut picker, "ctrl+s");
        picker.insert("review", false);
        key(&mut picker, "enter");
        assert_eq!(std::fs::read_to_string(tmp.path().join("presets/review.md")).unwrap(), "Review {branch} carefully");
        let Mode::Agent { kinds, rows, selected, preset, .. } = &picker.mode else { panic!("not back at agents") };
        assert_eq!(kinds[rows[*selected]], "codex");
        assert_eq!(picker.presets[*preset].name, "review");
    }

    fn editor(text: &str, cursor: usize) -> TextEdit {
        TextEdit { text: text.into(), cursor }
    }

    #[test]
    fn text_edit_keeps_pasted_lines_and_moves_between_them() {
        let mut edit = TextEdit::default();
        edit.insert("one\r\ntwo\tx", true);
        assert_eq!(edit.text, "one\ntwo    x");
        edit.insert("a\nb", false);
        assert_eq!(edit.text, "one\ntwo    xa b", "typed text never breaks lines");
        let mut edit = editor("first\nsecond line", 13);
        assert!(edit.key("up"));
        assert_eq!(edit.cursor, 5, "clamped to the shorter line");
        assert!(!edit.key("up"), "first line: the caller recalls history");
        assert!(edit.key("down"));
        assert_eq!(edit.cursor, 11);
        assert!(!edit.key("down"));
        edit.key("ctrl+a");
        assert_eq!(edit.cursor, 6);
        edit.key("ctrl+e");
        assert_eq!(edit.cursor, 17);
        edit.key("ctrl+w");
        assert_eq!(edit.text, "first\nsecond ");
        edit.key("ctrl+j");
        edit.key("alt+enter");
        assert_eq!(edit.text, "first\nsecond \n\n");
        edit.key("backspace");
        edit.key("ctrl+home");
        edit.key("delete");
        assert_eq!((edit.text.as_str(), edit.cursor), ("irst\nsecond \n", 0));
    }

    #[test]
    fn tab_completes_and_cycles_placeholders() {
        let mut edit = editor("on {co", 6);
        let mut state = None;
        assert_eq!(complete(&mut edit, &mut state, 1, &[]).unwrap(), "commits");
        assert_eq!((edit.text.as_str(), edit.cursor), ("on {commits}", 12));
        // Nothing else starts with "co": cycling stays put.
        assert_eq!(complete(&mut edit, &mut state, 1, &[]).unwrap(), "commits");
        let mut edit = editor("see ", 4);
        let mut state = None;
        assert_eq!(complete(&mut edit, &mut state, 1, &["ticket".into()]).unwrap(), "branch");
        assert_eq!(edit.text, "see {branch}");
        assert_eq!(complete(&mut edit, &mut state, 1, &["ticket".into()]).unwrap(), "base");
        assert_eq!(complete(&mut edit, &mut state, -1, &["ticket".into()]).unwrap(), "branch");
        assert_eq!(complete(&mut edit, &mut state, -1, &["ticket".into()]).unwrap(), "ticket");
        assert_eq!(edit.text, "see {ticket}");
        let mut edit = editor("{zz", 3);
        assert!(complete(&mut edit, &mut None, 1, &[]).unwrap_err().contains("{zz"));
        assert_eq!(edit.text, "{zz");
    }

    #[test]
    fn up_and_down_recall_earlier_prompts_and_the_draft() {
        let history = vec!["old".to_string(), "newer".to_string()];
        let mut edit = editor("draft", 5);
        let mut recall = None;
        recall_history(&history, &mut edit, &mut recall, true);
        assert_eq!(edit.text, "newer");
        recall_history(&history, &mut edit, &mut recall, true);
        recall_history(&history, &mut edit, &mut recall, true);
        assert_eq!(edit.text, "old");
        recall_history(&history, &mut edit, &mut recall, false);
        assert_eq!(edit.text, "newer");
        recall_history(&history, &mut edit, &mut recall, false);
        assert_eq!((edit.text.as_str(), edit.cursor, recall.is_none()), ("draft", 5, true));
        recall_history(&[], &mut edit, &mut recall, true);
        assert_eq!(edit.text, "draft");
    }

    /// Stand-in `claude` and `codex` on an isolated PATH: leaving the editor
    /// rebuilds the agent list from what is installed.
    fn fake_agents(tmp: &Path, env: &mut EnvVars) {
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["claude", "codex"] {
            let executable = bin.join(name);
            std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        env.set("PATH", &bin);
    }

    fn select_preset(picker: &mut Picker, name: &str) {
        let index = picker.presets.iter().position(|preset| preset.name == name).unwrap();
        if let Mode::Agent { preset, .. } = &mut picker.mode {
            *preset = index;
        }
    }

    #[test]
    fn ctrl_o_adds_text_under_the_preset() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        env.set("HERDR_PLUGIN_STATE_DIR", tmp.path().join("state"));
        fake_agents(tmp.path(), &mut env);
        let mut picker = preset_picker(tmp.path(), &mut env);
        select_preset(&mut picker, "plan");
        key(&mut picker, "ctrl+o");
        let Mode::Custom { kind, preset, .. } = &picker.mode else { panic!("editor not open") };
        assert_eq!((kind.as_str(), picker.presets[*preset].name.as_str()), ("codex", "plan"));
        picker.insert("focus on the parser", false);
        let rows = plain(&mut picker, 120, 40);
        let screen = rows.join("\n");
        assert!(screen.contains("preset plan") && screen.contains("│ You are in the git worktree"), "{screen}");
        assert!(screen.contains("your text goes below the preset") && screen.contains("focus on the parser"));
        let preview_order = screen.find("Do not change code yet").unwrap() < screen.rfind("focus on the parser").unwrap();
        assert!(preview_order, "the preview shows the preset, then the added text");
        // esc returns to the same agent and preset.
        key(&mut picker, "esc");
        let Mode::Agent { kinds, rows, selected, preset, .. } = &picker.mode else { panic!() };
        assert_eq!((kinds[rows[*selected]].as_str(), picker.presets[*preset].name.as_str()), ("codex", "plan"));
    }

    #[test]
    fn enter_opens_the_editor_for_ask_and_input_presets_and_asks_for_fields() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let dir = tmp.path().join("presets");
        presets::seed(&dir).unwrap();
        std::fs::write(dir.join("rules.md"), "---\nask: true\n---\nFollow the rules.").unwrap();
        std::fs::write(dir.join("wrap.md"), "Task: {input}").unwrap();
        std::fs::write(dir.join("ticket.md"), "Fix {ticket} for {owner}.").unwrap();
        fake_agents(tmp.path(), &mut env);
        let mut picker = preset_picker(tmp.path(), &mut env);
        for name in ["rules", "wrap"] {
            select_preset(&mut picker, name);
            key(&mut picker, "enter");
            assert!(matches!(picker.mode, Mode::Custom { .. }), "{name}");
            key(&mut picker, "esc");
        }
        select_preset(&mut picker, "ticket");
        key(&mut picker, "enter");
        let Mode::Fields { names, .. } = &picker.mode else { panic!("fields not asked") };
        assert_eq!(names, &["ticket", "owner"]);
        key(&mut picker, "enter");
        assert_eq!(picker.status.as_ref().unwrap().0, "type a value for {ticket}");
        picker.insert("ABC-1", false);
        key(&mut picker, "enter");
        let screen = plain(&mut picker, 120, 30).join("\n");
        assert!(screen.contains("owner ❯") && screen.contains("{ticket}  ABC-1") && screen.contains("Fix ABC-1 for {owner}."), "{screen}");
        key(&mut picker, "esc");
        assert!(matches!(picker.mode, Mode::Agent { .. }), "esc goes back to where the prompt came from");
    }

    #[test]
    fn ctrl_s_needs_a_running_agent() {
        let _lock = env_lock();
        let tmp = TempDir::new();
        let mut env = EnvVars::new();
        let mut picker = preset_picker(tmp.path(), &mut env);
        picker.worktrees = vec![Worktree { path: "/repo".into(), branch: Some("main".into()), ..Worktree::default() }];
        picker.prs = vec![PrInfo::unknown("test")];
        picker.agents = vec![Vec::new()];
        picker.facts = vec![None];
        picker.mode = Mode::List;
        picker.order = vec![0];
        picker.rebuild_items();
        key(&mut picker, "ctrl+s");
        assert!(picker.status.as_ref().unwrap().0.contains("no agent is running"));
        picker.agents = vec![vec![AgentHere { name: Some("claude-main".into()), kind: "claude".into(), status: "working".into(), cwd: "/repo".into(), pane_id: "p1".into() }]];
        key(&mut picker, "ctrl+s");
        let Mode::Agent { target: Target::Running(0), kinds, .. } = &picker.mode else { panic!("not sending") };
        assert_eq!(kinds, &["claude-main"]);
        let screen = plain(&mut picker, 120, 30).join("\n");
        assert!(screen.contains("send ❯") && screen.contains("claude-main"), "{screen}");
        key(&mut picker, "ctrl+o");
        assert!(matches!(&picker.mode, Mode::Custom { target: Target::Running(0), kind, .. } if kind == "claude-main"));
    }

    #[test]
    fn ages_use_the_bars_format() {
        assert_eq!(format_age(42), "42s");
        assert_eq!(format_age(7 * 60 + 5), "7m");
        assert_eq!(format_age(2 * 3600 + 4 * 60), "2h04m");
        assert_eq!(format_age(3 * 86_400 + 4 * 3600 + 59), "3d4h");
        assert_eq!(format_age(-1), "");
    }

    #[test]
    fn wrap_breaks_on_words_within_width() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("averyveryverylongword x", 8), ["averyveryverylongword", "x"]);
    }
}
