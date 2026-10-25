//! Command line entry point.

use crate::app::{Loaded, Outcome, Picker};
use crate::client::HerdrClient;
use crate::config::Config;
use crate::host;
use crate::icons;
use crate::model::{PrInfo, PrState, Project, Worktree};
use crate::opener::open_picker;
use crate::presets::{self, Preset};
use crate::project;
use crate::render;
use crate::pyjson::{self, Map, Value};
use crate::session;
use crate::term::{detect_appearance, isatty, Terminal};
use crate::theme::{default_selection_background, Theme};
use std::io::Write;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const USAGE: &str = "hertrees - every worktree of a project, its pull request, and agent sessions

usage: hertrees [options]
       hertrees presets
       hertrees start [--cwd PATH] (--path CHECKOUT | --branch NEW [--base REF] [--label NAME])
                      --agent KIND [--preset NAME] [--prompt TEXT] [--var NAME=VALUE]… [--focus]
       hertrees send [--cwd PATH] --agent NAME [--preset NAME] [--prompt TEXT] [--var NAME=VALUE]…
       hertrees clean [--cwd PATH] (--path CHECKOUT | --merged) [--dry-run]

options:
  --open           open the picker popup through the Herdr CLI
  --list           print the worktrees the picker would show, as JSON
  --cwd PATH       use the project at PATH instead of the focused pane's
  --version        print the plugin version
  --help           show this message

subcommands:
  presets          print the preset directories and templates (yours and the repo's) as JSON
  start            start an installed agent in an existing or new worktree
    --agent KIND   agent kind, or shell (a new tab, or a new worktree, with no agent)
    --label NAME   name the new worktree's space (default: the branch)
    --preset NAME  file preset or blank; custom requires --prompt
    --prompt TEXT  first prompt, or text added to --preset (at {input}, else below it)
    --var N=VALUE  value for a placeholder the prompt defines itself, e.g. {ticket}
    --focus        focus the started session (default: leave focus unchanged)
  send             prompt an agent already running in the project (--agent NAME is its
                   herdr name or pane id); a busy agent gets it once it is idle
  clean            remove a finished worktree: close its space, remove the checkout, and
                   delete the branch when GitHub merged it at its current commit
    --merged       every worktree whose PR is merged and that is safe to remove
    --dry-run      report what would be removed and why the rest stay; change nothing
  Placeholders: {branch} {base} {path} {repo} {pr} {pr_title} {pr_body} {commits}
  {diffstat} {input}. start prints workspace_id, tab_id, pane_id, agent (name, empty
  for shell), path, and prompt_deferred as JSON; send prints agent and deferred;
  clean prints dry_run, removed (path, branch, workspace, branch_deleted) and kept
  (path, branch, reason).
  Errors go to stderr with exit status 2.

Normally Herdr launches this itself:
  herdr plugin action invoke open --plugin hertrees
";

/// The truecolor mark; its background cells are the brand ink.
const MARK_ANSI: &str = include_str!("../branding/terminal/mark.ansi");
const INK_CELL: &str = "\x1b[38;2;20;23;31;48;2;20;23;31m";

/// `--version`: the mark above the version line on a terminal (plain with
/// NO_COLOR); just the version line otherwise, so scripts see no change.
fn version_text(tty: bool, no_color: bool) -> String {
    let line = format!("hertrees {}\n", VERSION);
    if !tty {
        return line;
    }
    let mark = if no_color {
        render::mark_rows(render::MARK, |row| row.trim().is_empty())
    } else {
        render::mark_rows(MARK_ANSI, |row| row.replace(INK_CELL, "").replace("\x1b[0m", "").chars().all(|c| c == '▀'))
    };
    format!("{}\n{}", mark.join("\n"), line)
}

fn out(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

fn fail(message: &str) -> i32 {
    eprintln!("hertrees: {}", message);
    2
}

/// The directory whose project to show: `--cwd`, else the pane the popup was
/// opened from. Outside Herdr, the shell's own directory.
fn source_cwd(args: &[String]) -> Option<String> {
    if let Some(pos) = args.iter().position(|a| a == "--cwd") {
        return args.get(pos + 1).cloned();
    }
    if let Some(cwd) = project::context_from_env().cwd {
        return Some(cwd);
    }
    if std::env::var_os("HERDR_PLUGIN_ID").is_some() {
        // A plugin pane runs in the plugin's own directory: never show that.
        return None;
    }
    std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned())
}

fn state_name(state: PrState) -> &'static str {
    match state {
        PrState::Merged => "merged",
        PrState::Open => "open",
        PrState::Draft => "draft",
        PrState::Closed => "closed",
        PrState::None => "none",
        PrState::Unknown => "unknown",
    }
}

fn opt(value: Option<&String>) -> Value {
    value.map(|v| Value::str(v.clone())).unwrap_or(Value::Null)
}

/// Fetch once, preserving the same last-good cache and classification for both commands.
fn classify_prs(project: &Project, worktrees: &[Worktree], config: &Config, gh: &Result<crate::pr::GhRepo, String>, cached: Option<&crate::pr::PrSnapshot>) -> Vec<PrInfo> {
    let snapshot = match gh {
        Ok(_) if cached.is_some_and(|snapshot| crate::pr::is_fresh(snapshot, config.pr_cache_ttl_s)) => Ok(cached.unwrap().clone()),
        Ok(repo) => crate::pr::fetch(repo).or_else(|error| cached.cloned().ok_or(error)),
        Err(error) => Err(error.clone()),
    };
    worktrees.iter().map(|wt| match (gh, &snapshot) {
        (Ok(repo), Ok(snapshot)) => crate::pr::classify(wt, repo, project.default_branch.as_deref(), snapshot),
        (_, Err(reason)) => PrInfo::unknown(reason.clone()),
        _ => PrInfo::unknown("no PR source"),
    }).collect()
}

#[derive(Debug, Default, PartialEq)]
struct StartArgs {
    cwd: Option<String>,
    path: Option<String>,
    branch: Option<String>,
    base: Option<String>,
    label: Option<String>,
    agent: String,
    preset: Option<String>,
    prompt: Option<String>,
    vars: Vec<(String, String)>,
    focus: bool,
}

fn parse_start(args: &[String]) -> Result<StartArgs, String> {
    let mut parsed = StartArgs::default();
    let mut agent = None;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        if flag == "--focus" {
            if parsed.focus { return Err("--focus specified more than once".into()); }
            parsed.focus = true;
            continue;
        }
        if flag == "--var" {
            let value = args.next().ok_or("--var needs NAME=VALUE")?;
            let (name, value) = value.split_once('=').ok_or("--var needs NAME=VALUE")?;
            if session::names(&format!("{{{name}}}")).is_empty() {
                return Err(format!("--var name must be lowercase letters, digits or _: {name}"));
            }
            parsed.vars.push((name.to_string(), value.to_string()));
            continue;
        }
        let slot = match flag.as_str() {
            "--cwd" => &mut parsed.cwd,
            "--path" => &mut parsed.path,
            "--branch" => &mut parsed.branch,
            "--base" => &mut parsed.base,
            "--label" => &mut parsed.label,
            "--agent" => &mut agent,
            "--preset" => &mut parsed.preset,
            "--prompt" => &mut parsed.prompt,
            _ => return Err(format!("unknown start option: {flag}")),
        };
        if slot.is_some() { return Err(format!("{flag} specified more than once")); }
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        // Prompt text is literal, even if it happens to be a global option.
        if flag != "--prompt" && (value.is_empty() || value.starts_with("--")) {
            return Err(format!("{flag} needs a value"));
        }
        *slot = Some(value.clone());
    }
    if parsed.path.is_some() == parsed.branch.is_some() {
        return Err("start requires exactly one of --path or --branch".into());
    }
    if parsed.base.is_some() && parsed.branch.is_none() {
        return Err("--base requires --branch".into());
    }
    if parsed.label.is_some() && parsed.branch.is_none() {
        return Err("--label requires --branch".into());
    }
    parsed.agent = agent.ok_or("start requires --agent KIND")?;
    Ok(parsed)
}

/// The preset's template with `--prompt` added the way the popup's editor adds it.
fn resolve_prompt(args: &StartArgs, presets: &[Preset]) -> Result<String, String> {
    let addition = args.prompt.as_deref().unwrap_or("");
    let name = args.preset.as_deref().unwrap_or(presets::BLANK);
    if name.eq_ignore_ascii_case(presets::BLANK) { return Ok(addition.to_string()); }
    if name.eq_ignore_ascii_case(presets::CUSTOM) {
        return match &args.prompt { Some(prompt) => Ok(prompt.clone()), None => Err("the custom preset needs --prompt TEXT".into()) };
    }
    let template = presets.iter().find(|preset| preset.path.is_some() && preset.name == name)
        .and_then(|preset| preset.prompt.clone())
        .ok_or_else(|| format!("unknown preset: {name}"))?;
    Ok(session::compose(&template, addition))
}

/// Fill a resolved prompt: `--var` values, the git/gh placeholders, then the rest.
fn finish_prompt(template: &str, mut vars: session::PromptVars, given: &[(String, String)], checkout: Option<&str>, pr_url: Option<&str>) -> Result<String, String> {
    let missing: Vec<String> = session::fields(template).into_iter().filter(|name| !given.iter().any(|(known, _)| known == name)).collect();
    if !missing.is_empty() {
        let flags: Vec<String> = missing.iter().map(|name| format!("--var {name}=VALUE")).collect();
        return Err(format!("the prompt needs {}", flags.join(" ")));
    }
    for (name, value) in given {
        vars.set(name, value.clone());
    }
    session::fill_lazy(&mut vars, template, checkout, pr_url);
    Ok(session::expand_prompt(template, &vars))
}

/// The main checkout of the repository containing `cwd`, from git alone (no herdr).
fn repo_root(cwd: &str) -> Option<String> {
    let argv: Vec<String> = ["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"].iter().map(|arg| arg.to_string()).collect();
    let output = crate::proc::run(&argv, 5.0).ok().filter(|output| output.success)?;
    let common = String::from_utf8_lossy(&output.stdout).trim().to_string();
    std::path::Path::new(&common).parent().map(|root| root.to_string_lossy().into_owned())
}

fn matching_worktree<'a>(worktrees: &'a [Worktree], path: &str) -> Result<&'a Worktree, String> {
    let canonical = std::fs::canonicalize(path).ok();
    worktrees.iter().find(|wt| {
        match (canonical.as_ref(), std::fs::canonicalize(&wt.path).ok()) {
            (Some(path), Some(candidate)) => path == &candidate,
            _ => path.trim_end_matches('/') == wt.path.trim_end_matches('/'),
        }
    }).ok_or_else(|| format!("--path is not a listed worktree: {path}"))
}

fn print_presets() -> i32 {
    let dir = presets::dir();
    let root = source_cwd(&[]).as_deref().and_then(repo_root);
    let entries = presets::load(dir.as_deref(), root.as_deref()).into_iter().filter(|preset| preset.path.is_some()).map(|preset| {
        let mut entry = Map::new();
        entry.insert("name", Value::str(preset.name));
        entry.insert("path", Value::str(preset.path.unwrap().to_string_lossy().into_owned()));
        entry.insert("prompt", Value::str(preset.prompt.unwrap_or_default()));
        entry.insert("repo", Value::Bool(preset.repo));
        entry.insert("ask", Value::Bool(preset.ask));
        entry.insert("agent", preset.agent.map(Value::str).unwrap_or(Value::Null));
        entry.insert("description", preset.description.map(Value::str).unwrap_or(Value::Null));
        Value::Dict(entry)
    }).collect();
    let mut top = Map::new();
    top.insert("dir", dir.map(|path| Value::str(path.to_string_lossy().into_owned())).unwrap_or(Value::Null));
    top.insert("repo_dir", root.map(|root| Value::str(presets::repo_dir(&root).to_string_lossy().into_owned())).unwrap_or(Value::Null));
    top.insert("presets", Value::List(entries));
    out(&(pyjson::dumps_indent(&Value::Dict(top), 2) + "\n"));
    0
}

fn start(args: &[String]) -> Result<(), String> {
    let args = parse_start(args)?;
    if args.agent != "shell" && session::installed_agents(std::slice::from_ref(&args.agent)).is_empty() {
        return Err(format!("unknown or uninstalled agent kind: {}", args.agent));
    }
    let mut client = HerdrClient::from_env();
    let cwd = args.cwd.clone().or_else(|| source_cwd(&[])).ok_or("no working directory: use --cwd PATH")?;
    let (project, worktrees) = project::load(&mut client, &cwd)?;
    let template = resolve_prompt(&args, &presets::load(presets::dir().as_deref(), Some(&project.repo_root)))?;
    let new_worktree;
    let wt = if let Some(path) = &args.path {
        matching_worktree(&worktrees, path)?
    } else {
        new_worktree = Worktree { branch: args.branch.clone(), path: "{path}".into(), ..Worktree::default() };
        &new_worktree
    };
    let info = pr_info(&project, wt, &template);
    let checkout = args.path.as_ref().map(|_| wt.path.as_str());
    let pr_url = info.as_ref().and_then(|info| info.pr.as_ref()).map(|pr| pr.url.as_str());
    let vars = session::prompt_vars(&project, wt, args.base.as_deref(), info.as_ref());
    let prompt = finish_prompt(&template, vars, &args.vars, checkout, pr_url)?;
    let prompt = if prompt.is_empty() { None } else { Some(prompt.as_str()) };
    let started = if args.agent == "shell" {
        match &args.branch {
            Some(branch) => session::create_shell(&mut client, &project, branch, args.base.as_deref(), args.label.as_deref())?,
            None => session::open_tab(&mut client, &project, wt, &mut |_| {})?,
        }
    } else {
        let taken = project::agents(&mut client).into_iter().filter_map(|agent| agent.name).collect::<Vec<_>>();
        if let Some(branch) = &args.branch {
            session::start_new(&mut client, &project, branch, args.base.as_deref(), args.label.as_deref(), &args.agent, prompt, &taken, &mut |_| {})?
        } else {
            session::start_existing(&mut client, &project, wt, &args.agent, prompt, &taken, &mut |_| {})?
        }
    };
    if args.focus { session::focus(&mut client, &started)?; }
    let mut result = Map::new();
    result.insert("workspace_id", Value::str(started.workspace_id));
    result.insert("tab_id", Value::str(started.tab_id));
    result.insert("pane_id", Value::str(started.pane_id));
    result.insert("agent", Value::str(started.agent));
    result.insert("path", Value::str(started.path));
    result.insert("prompt_deferred", Value::Bool(started.prompt_deferred));
    out(&(pyjson::dumps_indent(&Value::Dict(result), 2) + "\n"));
    Ok(())
}

/// The worktree's PR, looked up only when the prompt uses a PR placeholder.
fn pr_info(project: &Project, wt: &Worktree, template: &str) -> Option<PrInfo> {
    if !session::names(template).iter().any(|name| name.starts_with("pr")) {
        return None;
    }
    let gh = crate::pr::github_repo(&project.repo_root);
    let cached = gh.as_ref().ok().and_then(crate::pr::cached);
    classify_prs(project, std::slice::from_ref(wt), &Config::load(), &gh, cached.as_ref()).pop()
}

/// `send`: prompt a running agent of the project. Shares start's prompt options.
fn send(args: &[String]) -> Result<(), String> {
    let mut rest = Vec::new();
    let mut agent = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--agent" if agent.is_some() => return Err("--agent specified more than once".into()),
            "--agent" => agent = Some(iter.next().filter(|v| !v.is_empty() && !v.starts_with("--")).ok_or("--agent needs a value")?.clone()),
            "--path" | "--branch" | "--base" | "--focus" => return Err(format!("send does not accept {flag}")),
            _ => rest.push(flag.clone()),
        }
    }
    let target = agent.ok_or("send requires --agent NAME")?;
    // Reuse start's parser for --cwd/--preset/--prompt/--var.
    rest.extend(["--agent".into(), "send".into(), "--branch".into(), "unused".into()]);
    let parsed = parse_start(&rest)?;
    if parsed.preset.is_none() && parsed.prompt.is_none() {
        return Err("send needs --preset NAME or --prompt TEXT".into());
    }
    let mut client = HerdrClient::from_env();
    let cwd = parsed.cwd.clone().or_else(|| source_cwd(&[])).ok_or("no working directory: use --cwd PATH")?;
    let (project, worktrees) = project::load(&mut client, &cwd)?;
    let agents = project::agents(&mut client);
    let by_worktree = project::agents_by_worktree(&worktrees, &agents);
    let index = by_worktree.iter().position(|here| here.iter().any(|a| a.name.as_deref() == Some(&target) || a.pane_id == target))
        .ok_or_else(|| format!("no running agent named {target} in this project"))?;
    let wt = &worktrees[index];
    let template = resolve_prompt(&parsed, &presets::load(presets::dir().as_deref(), Some(&project.repo_root)))?;
    if template.trim().is_empty() {
        return Err("nothing to send: the prompt is empty".into());
    }
    let info = pr_info(&project, wt, &template);
    let pr_url = info.as_ref().and_then(|info| info.pr.as_ref()).map(|pr| pr.url.as_str());
    let vars = session::prompt_vars(&project, wt, None, info.as_ref());
    let prompt = finish_prompt(&template, vars, &parsed.vars, Some(&wt.path), pr_url)?;
    let deferred = session::send_to(&mut client, &target, &prompt)?;
    let mut result = Map::new();
    result.insert("agent", Value::str(target));
    result.insert("deferred", Value::Bool(deferred));
    out(&(pyjson::dumps_indent(&Value::Dict(result), 2) + "\n"));
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
struct CleanArgs {
    cwd: Option<String>,
    path: Option<String>,
    merged: bool,
    dry_run: bool,
}

fn parse_clean(args: &[String]) -> Result<CleanArgs, String> {
    let mut parsed = CleanArgs::default();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let switch = match flag.as_str() {
            "--merged" => Some(&mut parsed.merged),
            "--dry-run" => Some(&mut parsed.dry_run),
            _ => None,
        };
        if let Some(switch) = switch {
            if *switch { return Err(format!("{flag} specified more than once")); }
            *switch = true;
            continue;
        }
        let slot = match flag.as_str() {
            "--cwd" => &mut parsed.cwd,
            "--path" => &mut parsed.path,
            _ => return Err(format!("unknown clean option: {flag}")),
        };
        if slot.is_some() { return Err(format!("{flag} specified more than once")); }
        let value = args.next().filter(|value| !value.is_empty() && !value.starts_with("--")).ok_or_else(|| format!("{flag} needs a value"))?;
        *slot = Some(value.clone());
    }
    if parsed.path.is_some() == parsed.merged {
        return Err("clean needs exactly one of --path CHECKOUT or --merged".into());
    }
    Ok(parsed)
}

/// `clean`: the popup's alt+d (`--path`) or alt+shift+d (`--merged`), under the
/// same refusals. A refused or failed `--path` is an error; `--merged` reports
/// each worktree it kept and why.
fn clean(args: &[String]) -> Result<(), String> {
    let args = parse_clean(args)?;
    let mut client = HerdrClient::from_env();
    let cwd = args.cwd.clone().or_else(|| source_cwd(&[])).ok_or("no working directory: use --cwd PATH")?;
    let (project, worktrees) = project::load(&mut client, &cwd)?;
    let agents = project::agents(&mut client);
    let by_worktree = project::agents_by_worktree(&worktrees, &agents);
    let gh = crate::pr::github_repo(&project.repo_root);
    let cached = gh.as_ref().ok().and_then(crate::pr::cached);
    let prs = classify_prs(&project, &worktrees, &Config::load(), &gh, cached.as_ref());
    let entry = |wt: &Worktree| {
        let mut map = Map::new();
        map.insert("path", Value::str(wt.path.clone()));
        map.insert("branch", opt(wt.branch.as_ref()));
        map
    };
    let (mut removed, mut kept) = (Vec::new(), Vec::new());
    let mut keep = |wt: &Worktree, reason: String| {
        let mut map = entry(wt);
        map.insert("reason", Value::str(reason));
        kept.push(Value::Dict(map));
    };
    let targets: Vec<usize> = match &args.path {
        Some(path) => {
            let wt = matching_worktree(&worktrees, path)?;
            vec![worktrees.iter().position(|candidate| std::ptr::eq(candidate, wt)).unwrap()]
        }
        None => (0..worktrees.len()).filter(|&i| {
            let wt = &worktrees[i];
            match prs[i].state {
                PrState::Merged => true,
                // Missing PR data is not evidence of no PR: say so.
                PrState::Unknown if !(wt.is_main || wt.bare) => {
                    keep(wt, format!("its pull request is unknown: {}", prs[i].reason.as_deref().unwrap_or("no reason given")));
                    false
                }
                _ => false,
            }
        }).collect(),
    };
    for index in targets {
        let wt = &worktrees[index];
        let dirty = if wt.is_main || wt.bare || wt.prunable { None } else { project::git_facts(&wt.path, None).dirty };
        let blocker = session::cleanup_blocker(wt, &project.source_checkout, &by_worktree[index], dirty)
            .map(|reason| if dirty.is_none() && reason.contains("loading") { "its git status could not be read".to_owned() } else { reason });
        let delete_branch = session::deletes_branch(wt, &prs[index]);
        let outcome = match blocker {
            Some(reason) => Err(reason),
            None if args.dry_run => Ok(delete_branch),
            None => session::remove_worktree(&mut client, &project, wt, delete_branch).map(|done| done.branch_deleted),
        };
        match outcome {
            Ok(branch_deleted) => {
                let mut map = entry(wt);
                map.insert("workspace", opt(wt.open_workspace_id.as_ref()));
                map.insert("branch_deleted", Value::Bool(branch_deleted));
                removed.push(Value::Dict(map));
            }
            Err(reason) if args.path.is_some() => return Err(format!("{} stays: {}", wt.title(), reason)),
            Err(reason) => keep(wt, reason),
        }
    }
    let mut result = Map::new();
    result.insert("dry_run", Value::Bool(args.dry_run));
    result.insert("removed", Value::List(removed));
    result.insert("kept", Value::List(kept));
    out(&(pyjson::dumps_indent(&Value::Dict(result), 2) + "\n"));
    Ok(())
}

/// `--list`: the rows, with PRs fetched (or cached) and agents joined.
fn list(client: HerdrClient, config: Config, cwd: Option<String>) -> i32 {
    let mut client = client;
    let loaded = Loaded::load(&mut client, cwd.as_deref());
    let (project, worktrees) = match &loaded.result {
        Ok(found) => found.clone(),
        Err(error) => return fail(error),
    };
    let prs = classify_prs(&project, &worktrees, &config, &loaded.gh, loaded.cached.as_ref());
    let by = project::agents_by_worktree(&worktrees, &loaded.agents);
    let rows: Vec<Value> = worktrees
        .iter()
        .zip(&prs)
        .zip(&by)
        .map(|((wt, info), agents)| {
            let mut map = Map::new();
            map.insert("path", Value::str(wt.path.clone()));
            map.insert("branch", opt(wt.branch.as_ref()));
            map.insert("head", Value::str(wt.head.clone()));
            map.insert("main", Value::Bool(wt.is_main));
            map.insert("detached", Value::Bool(wt.detached));
            map.insert("workspace", opt(wt.open_workspace_id.as_ref()));
            map.insert("pr_state", Value::str(state_name(info.state)));
            map.insert("pr_number", info.pr.as_ref().map(|p| Value::Int(p.number as i128)).unwrap_or(Value::Null));
            map.insert("pr_url", opt(info.pr.as_ref().map(|p| &p.url)));
            map.insert("pr_via_upstream", Value::Bool(info.via_upstream));
            map.insert("pr_reason", opt(info.reason.as_ref()));
            map.insert(
                "agents",
                Value::List(
                    agents
                        .iter()
                        .map(|a| {
                            let mut m = Map::new();
                            m.insert("kind", Value::str(a.kind.clone()));
                            m.insert("name", opt(a.name.as_ref()));
                            m.insert("status", Value::str(a.status.clone()));
                            m.insert("pane_id", Value::str(a.pane_id.clone()));
                            Value::Dict(m)
                        })
                        .collect(),
                ),
            );
            Value::Dict(map)
        })
        .collect();
    let mut top = Map::new();
    top.insert("repo_root", Value::str(project.repo_root.clone()));
    top.insert("source_checkout", Value::str(project.source_checkout.clone()));
    top.insert("default_branch", opt(project.default_branch.as_ref()));
    top.insert("worktrees", Value::List(rows));
    out(&(pyjson::dumps_indent(&Value::Dict(top), 2) + "\n"));
    0
}

pub fn main(args: Vec<String>) -> i32 {
    // Dispatch before global flags so prompt text such as "--help" stays literal.
    if matches!(args.first().map(String::as_str), Some("start" | "send" | "clean" | "presets")) {
        if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
            out(USAGE);
            return 0;
        }
        return match args[0].as_str() {
            "presets" if args.len() == 1 => print_presets(),
            "presets" => fail("presets does not accept options"),
            "send" => match send(&args[1..]) { Ok(()) => 0, Err(error) => fail(&error) },
            "clean" => match clean(&args[1..]) { Ok(()) => 0, Err(error) => fail(&error) },
            _ => match start(&args[1..]) { Ok(()) => 0, Err(error) => fail(&error) },
        };
    }
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if has("--help") || has("-h") {
        out(USAGE);
        return 0;
    }
    if has("--version") {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        out(&version_text(isatty(1), no_color));
        return 0;
    }
    if has("--open") {
        return open_picker();
    }
    if args.first().map(|a| a == "--send-prompt").unwrap_or(false) {
        return session::send_prompt_main(&args[1..]);
    }

    let config = Config::load();
    let client = HerdrClient::from_env();
    let cwd = source_cwd(&args);
    if has("--list") {
        return list(client, config, cwd);
    }
    if !isatty(0) || !isatty(1) {
        return fail("needs a terminal; run it from Herdr with `plugin pane open`");
    }

    // The project load (herdr + git) and the font check run while the
    // terminal answers the appearance query.
    if config.agent_icons.as_str() == Some("auto") {
        icons::prefetch();
    }
    if config.icons == "auto" {
        icons::prefetch_nerd();
    }
    let (fetched, fetch) = std::sync::mpsc::channel();
    let mut loader = client.clone();
    let load_cwd = cwd.clone();
    std::thread::spawn(move || {
        let loaded = Loaded::load(&mut loader, load_cwd.as_deref());
        let _ = fetched.send(loaded);
    });

    let configured = config.selection_background.clone();
    let auto = configured.as_str() == Some("auto");
    let mut appearance = None;
    if auto || !config.colors.contains_key("muted") {
        appearance = detect_appearance(0.12);
    }
    let selection_background: Option<Value> =
        if auto { default_selection_background(appearance).map(Value::str) } else { Some(configured) };
    let mut theme = Theme::new(Some(&config.colors), selection_background.as_ref(), appearance);
    theme.agent_icons = icons::enabled(&config.agent_icons);
    theme.tree_icons = icons::tree_icons(Some(config.icons));

    let mouse = config.mouse;
    let loaded = fetch.recv().expect("the load thread panicked");
    // Never dropped: the process ends right after.
    let mut picker = std::mem::ManuallyDrop::new(Picker::new(client, config, theme, loaded));

    let outcome = loop {
        let outcome = {
            let mut terminal = match Terminal::enter(mouse) {
                Ok(terminal) => terminal,
                Err(error) => return fail(&host::os_error_str(&error)),
            };
            picker.run(&mut terminal)
        };
        match outcome {
            Outcome::Edit { path, created } => {
                let result = crate::presets::open_editor(&path);
                picker.edited(&path, created, result);
            }
            other => break other,
        }
    };

    // The terminal is restored: now move Herdr's focus.
    let result = match outcome {
        Outcome::Cancel => Ok(()),
        Outcome::Jump(index) => {
            let project = picker.project.clone().unwrap_or_default();
            let wt = picker.worktrees[index].clone();
            session::jump(&mut picker.client, &project, &wt)
        }
        Outcome::Started(started) => session::focus(&mut picker.client, &started),
        Outcome::Edit { .. } => unreachable!("editor outcomes are handled before focusing"),
    };
    match result {
        Ok(()) => 0,
        Err(error) => fail(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn version_shows_the_mark_only_on_a_terminal() {
        let line = format!("hertrees {}\n", VERSION);
        for no_color in [false, true] {
            assert_eq!(version_text(false, no_color), line, "pipes keep the bare version line");
        }
        let color = version_text(true, false);
        assert!(color.ends_with(&format!("\x1b[0m\n{}", line)));
        assert!(color.contains("\x1b[38;2;"));
        let first = color.lines().next().unwrap().replace(INK_CELL, "");
        assert!(first.contains("\x1b[38;2;"), "blank ink rows are dropped");
        let plain = version_text(true, true);
        assert!(plain.ends_with(&line));
        assert!(!plain.contains('\x1b'));
        assert!(!plain.lines().next().unwrap().trim().is_empty());
        assert_eq!(plain.lines().count(), render::mark_rows(render::MARK, |r| r.trim().is_empty()).len() + 1);
    }

    #[test]
    fn start_rejects_conflicting_or_missing_arguments() {
        for (input, error) in [
            (vec!["--agent", "claude"], "exactly one"),
            (vec!["--path", "/repo", "--branch", "new", "--agent", "claude"], "exactly one"),
            (vec!["--branch", "new"], "requires --agent"),
            (vec!["--branch", "new", "--agent"], "--agent needs a value"),
            (vec!["--branch", "--agent", "claude"], "--branch needs a value"),
            (vec!["--branch", "new", "--agent", "claude", "--var", "ticket"], "NAME=VALUE"),
            (vec!["--branch", "new", "--agent", "claude", "--var", "Bad=x"], "lowercase"),
            (vec!["--path", "/repo", "--agent", "claude", "--base", "main"], "--base requires --branch"),
            (vec!["--path", "/repo", "--agent", "claude", "--label", "x"], "--label requires --branch"),
            (vec!["--branch", "new", "--agent", "claude", "--agent", "codex"], "more than once"),
            (vec!["--branch", "new", "--agent", "claude", "--focus", "--focus"], "more than once"),
            (vec!["--branch", "new", "--agent", "claude", "--typo"], "unknown start option"),
        ] {
            assert!(parse_start(&args(&input)).unwrap_err().contains(error), "{input:?}");
        }
    }

    #[test]
    fn clean_needs_one_target_and_known_options() {
        for (input, expected) in [
            (vec![], "exactly one"),
            (vec!["--path", "/repo/.worktrees/x", "--merged"], "exactly one"),
            (vec!["--path"], "--path needs a value"),
            (vec!["--path", "--merged"], "--path needs a value"),
            (vec!["--merged", "--merged"], "more than once"),
            (vec!["--merged", "--force"], "unknown clean option"),
        ] {
            assert!(parse_clean(&args(&input)).unwrap_err().contains(expected), "{input:?}");
        }
        let parsed = parse_clean(&args(&["--dry-run", "--cwd", "/repo", "--merged"])).unwrap();
        assert_eq!(parsed, CleanArgs { cwd: Some("/repo".into()), path: None, merged: true, dry_run: true });
    }

    #[test]
    fn prompt_values_are_not_global_flags_and_options_can_be_reordered() {
        let parsed = parse_start(&args(&["--prompt", "--help", "--agent", "claude", "--focus", "--base", "main", "--branch", "new", "--cwd", "/repo"])).unwrap();
        assert_eq!(parsed.prompt.as_deref(), Some("--help"));
        assert_eq!(parsed.agent, "claude");
        assert_eq!(parsed.branch.as_deref(), Some("new"));
        assert_eq!(parsed.base.as_deref(), Some("main"));
        assert_eq!(parsed.cwd.as_deref(), Some("/repo"));
        assert!(parsed.focus);
        let parsed = parse_start(&args(&["--branch", "new", "--agent", "shell"])).unwrap();
        assert!(parsed.prompt.is_none());
    }

    #[test]
    fn preset_resolution_distinguishes_builtins_files_and_literal_prompts() {
        let presets = vec![
            Preset { name: "plan".into(), path: Some("/presets/plan.md".into()), prompt: Some("plan {branch}".into()), ..Preset::default() },
            Preset { name: "wrap".into(), path: Some("/presets/wrap.md".into()), prompt: Some("Task: {input}\nRules.".into()), ..Preset::default() },
        ];
        let mut input = StartArgs::default();
        assert_eq!(resolve_prompt(&input, &presets).unwrap(), "");
        input.preset = Some("BLANK".into());
        assert_eq!(resolve_prompt(&input, &presets).unwrap(), "");
        input.preset = Some("CuStOm".into());
        assert!(resolve_prompt(&input, &presets).unwrap_err().contains("--prompt"));
        input.preset = Some("missing".into());
        assert_eq!(resolve_prompt(&input, &presets).unwrap_err(), "unknown preset: missing");
        input.preset = Some("plan".into());
        assert_eq!(resolve_prompt(&input, &presets).unwrap(), "plan {branch}");
        input.preset = None;
        input.prompt = Some(String::new());
        assert_eq!(resolve_prompt(&input, &presets).unwrap(), "");
        input.prompt = Some("literal {branch}".into());
        assert_eq!(resolve_prompt(&input, &presets).unwrap(), "literal {branch}");
        // A preset and a prompt together: below the preset, or at its {input}.
        input.preset = Some("plan".into());
        input.prompt = Some("then fix it".into());
        assert_eq!(resolve_prompt(&input, &presets).unwrap(), "plan {branch}\n\nthen fix it");
        input.preset = Some("wrap".into());
        assert_eq!(resolve_prompt(&input, &presets).unwrap(), "Task: then fix it\nRules.");
    }

    #[test]
    fn vars_fill_fields_and_missing_ones_are_named() {
        let parsed = parse_start(&args(&["--branch", "new", "--agent", "claude", "--var", "ticket=ABC-1", "--var", "area=a=b"])).unwrap();
        assert_eq!(parsed.vars, [("ticket".to_string(), "ABC-1".to_string()), ("area".to_string(), "a=b".to_string())]);
        let mut vars = session::PromptVars::default();
        vars.set("branch", "topic");
        assert_eq!(finish_prompt("{ticket} on {branch} in {area}", vars.clone(), &parsed.vars, None, None).unwrap(), "ABC-1 on topic in a=b");
        assert_eq!(finish_prompt("{ticket} {owner} {team}", vars, &parsed.vars, None, None).unwrap_err(), "the prompt needs --var owner=VALUE --var team=VALUE");
    }

    #[test]
    fn checkout_matching_canonicalizes_and_trims_missing_paths() {
        use std::os::unix::fs::symlink;
        let temp = crate::testutil::TempDir::new();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let alias = temp.path().join("alias");
        symlink(&checkout, &alias).unwrap();
        let missing = temp.path().join("missing");
        let worktrees = vec![
            Worktree { path: checkout.to_string_lossy().into_owned(), ..Worktree::default() },
            Worktree { path: missing.to_string_lossy().into_owned(), prunable: true, ..Worktree::default() },
        ];
        assert!(std::ptr::eq(matching_worktree(&worktrees, &format!("{}/./", alias.display())).unwrap(), &worktrees[0]));
        assert!(std::ptr::eq(matching_worktree(&worktrees, &format!("{}/", missing.display())).unwrap(), &worktrees[1]));
        assert!(matching_worktree(&worktrees, temp.path().to_str().unwrap()).unwrap_err().contains("not a listed worktree"));
        assert!(matching_worktree(&worktrees, &format!("{}-other", checkout.display())).is_err());
    }
}
