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
use crate::pyjson::{self, Map, Value};
use crate::session;
use crate::term::{detect_appearance, isatty, Terminal};
use crate::theme::{default_selection_background, Theme};
use std::io::Write;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const USAGE: &str = "hertrees - every worktree of a project, its pull request, and agent sessions

usage: hertrees [options]
       hertrees presets
       hertrees start [--cwd PATH] (--path CHECKOUT | --branch NEW [--base REF])
                      --agent KIND [--preset NAME | --prompt TEXT] [--focus]

options:
  --open           open the picker popup through the Herdr CLI
  --list           print the worktrees the picker would show, as JSON
  --cwd PATH       use the project at PATH instead of the focused pane's
  --version        print the plugin version
  --help           show this message

subcommands:
  presets          print the preset directory and file templates as JSON
  start            start an installed agent in an existing or new worktree
    --agent KIND   agent kind, or shell (new worktree only)
    --preset NAME  file preset or blank; custom requires --prompt instead
    --prompt TEXT  first prompt; expands {branch}, {base}, {path}, {repo}, {pr}
    --focus        focus the started session (default: leave focus unchanged)
  start prints workspace_id, tab_id, pane_id, agent (name, empty for shell),
  path, and prompt_deferred as JSON. Errors go to stderr with exit status 2.

Normally Herdr launches this itself:
  herdr plugin action invoke open --plugin hertrees
";

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
    agent: String,
    preset: Option<String>,
    prompt: Option<String>,
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
        let slot = match flag.as_str() {
            "--cwd" => &mut parsed.cwd,
            "--path" => &mut parsed.path,
            "--branch" => &mut parsed.branch,
            "--base" => &mut parsed.base,
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
    if parsed.preset.is_some() && parsed.prompt.is_some() {
        return Err("--preset and --prompt cannot be used together".into());
    }
    parsed.agent = agent.ok_or("start requires --agent KIND")?;
    if parsed.agent == "shell" && parsed.path.is_some() {
        return Err("--agent shell requires --branch, not --path".into());
    }
    Ok(parsed)
}

fn resolve_prompt(args: &StartArgs, presets: &[Preset]) -> Result<String, String> {
    if let Some(prompt) = &args.prompt { return Ok(prompt.clone()); }
    let name = args.preset.as_deref().unwrap_or(presets::BLANK);
    if name.eq_ignore_ascii_case(presets::BLANK) { return Ok(String::new()); }
    if name.eq_ignore_ascii_case(presets::CUSTOM) {
        return Err("the custom preset needs --prompt TEXT instead".into());
    }
    presets.iter().find(|preset| preset.path.is_some() && preset.name == name)
        .and_then(|preset| preset.prompt.clone())
        .ok_or_else(|| format!("unknown preset: {name}"))
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
    let entries = presets::load(dir.as_deref()).into_iter().filter(|preset| preset.path.is_some()).map(|preset| {
        let mut entry = Map::new();
        entry.insert("name", Value::str(preset.name));
        entry.insert("path", Value::str(preset.path.unwrap().to_string_lossy().into_owned()));
        entry.insert("prompt", Value::str(preset.prompt.unwrap_or_default()));
        Value::Dict(entry)
    }).collect();
    let mut top = Map::new();
    top.insert("dir", dir.map(|path| Value::str(path.to_string_lossy().into_owned())).unwrap_or(Value::Null));
    top.insert("presets", Value::List(entries));
    out(&(pyjson::dumps_indent(&Value::Dict(top), 2) + "\n"));
    0
}

fn start(args: &[String]) -> Result<(), String> {
    let args = parse_start(args)?;
    let template = resolve_prompt(&args, &presets::load(presets::dir().as_deref()))?;
    if args.agent != "shell" && session::installed_agents(std::slice::from_ref(&args.agent)).is_empty() {
        return Err(format!("unknown or uninstalled agent kind: {}", args.agent));
    }
    let mut client = HerdrClient::from_env();
    let cwd = args.cwd.clone().or_else(|| source_cwd(&[])).ok_or("no working directory: use --cwd PATH")?;
    let (project, worktrees) = project::load(&mut client, &cwd)?;
    let new_worktree;
    let wt = if let Some(path) = &args.path {
        matching_worktree(&worktrees, path)?
    } else {
        new_worktree = Worktree { branch: args.branch.clone(), path: "{path}".into(), ..Worktree::default() };
        &new_worktree
    };
    let info = if template.contains("{pr}") {
        let gh = crate::pr::github_repo(&project.repo_root);
        let cached = gh.as_ref().ok().and_then(crate::pr::cached);
        classify_prs(&project, std::slice::from_ref(wt), &Config::load(), &gh, cached.as_ref()).pop()
    } else { None };
    let prompt = session::expand_prompt(&template, &session::prompt_vars(&project, wt, args.base.as_deref(), info.as_ref()));
    let prompt = if prompt.is_empty() { None } else { Some(prompt.as_str()) };
    let started = if args.agent == "shell" {
        session::create_shell(&mut client, &project, args.branch.as_deref().unwrap(), args.base.as_deref())?
    } else {
        let taken = project::agents(&mut client).into_iter().filter_map(|agent| agent.name).collect::<Vec<_>>();
        if let Some(branch) = &args.branch {
            session::start_new(&mut client, &project, branch, args.base.as_deref(), &args.agent, prompt, &taken, &mut |_| {})?
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
    if matches!(args.first().map(String::as_str), Some("start" | "presets")) {
        if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
            out(USAGE);
            return 0;
        }
        return match args[0].as_str() {
            "presets" if args.len() == 1 => print_presets(),
            "presets" => fail("presets does not accept options"),
            _ => match start(&args[1..]) { Ok(()) => 0, Err(error) => fail(&error) },
        };
    }
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if has("--help") || has("-h") {
        out(USAGE);
        return 0;
    }
    if has("--version") {
        out(&format!("hertrees {}\n", VERSION));
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
    fn start_rejects_conflicting_or_missing_arguments() {
        for (input, error) in [
            (vec!["--agent", "claude"], "exactly one"),
            (vec!["--path", "/repo", "--branch", "new", "--agent", "claude"], "exactly one"),
            (vec!["--branch", "new"], "requires --agent"),
            (vec!["--branch", "new", "--agent"], "--agent needs a value"),
            (vec!["--branch", "--agent", "claude"], "--branch needs a value"),
            (vec!["--branch", "new", "--agent", "claude", "--preset", "plan", "--prompt", "hi"], "cannot be used together"),
            (vec!["--path", "/repo", "--agent", "claude", "--base", "main"], "--base requires --branch"),
            (vec!["--path", "/repo", "--agent", "shell"], "shell requires --branch"),
            (vec!["--branch", "new", "--agent", "claude", "--agent", "codex"], "more than once"),
            (vec!["--branch", "new", "--agent", "claude", "--focus", "--focus"], "more than once"),
            (vec!["--branch", "new", "--agent", "claude", "--typo"], "unknown start option"),
        ] {
            assert!(parse_start(&args(&input)).unwrap_err().contains(error), "{input:?}");
        }
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
        let presets = vec![Preset { name: "plan".into(), path: Some("/presets/plan.md".into()), prompt: Some("plan {branch}".into()) }];
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
