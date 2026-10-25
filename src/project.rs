//! Discover a project's checkouts and associate local Git and agent state.

use crate::client::{self, HerdrClient};
use crate::model::{AgentHere, GitFacts, Project, Worktree};
use crate::pyjson::{self, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where the popup was opened from.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub cwd: Option<String>,
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

fn parse_context(json: &str) -> Context {
    let Ok(value) = pyjson::loads(json) else { return Context::default() };
    Context {
        cwd: text(value.get("focused_pane_cwd"))
            .or_else(|| text(value.get("workspace_cwd")))
            .or_else(|| text(value.get("worktree").get("checkout_path")))
            .map(str::to_owned),
    }
}

/// Parse the frozen invoking-pane context, not the popup process's own cwd.
pub fn context_from_env() -> Context {
    std::env::var("HERDR_PLUGIN_CONTEXT_JSON").map(|json| parse_context(&json)).unwrap_or_default()
}

/// Run a read-only Git command with a bounded lifetime.
fn git(path: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut argv = Vec::with_capacity(args.len() + 4);
    argv.extend(["git", "--no-optional-locks", "-C", path].into_iter().map(str::to_owned));
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    let output = crate::proc::run(&argv, 3.0).ok()?;
    output.success.then_some(output.stdout)
}

/// Discover through Herdr so linked checkouts resolve to their main repository.
pub fn load(client: &mut HerdrClient, cwd: &str) -> Result<(Project, Vec<Worktree>), String> {
    let result = client
        .call("worktree.list", client::dict(&[("cwd", Value::str(cwd))]), &["worktree", "list", "--cwd", cwd])
        .map_err(|error| {
            let message = error.message.trim();
            if message.is_empty() { "herdr unreachable".to_owned() } else { message.to_owned() }
        })?;
    let source = result.get("source").ok_or("not a git project")?;
    let repo_root = text(source.get("repo_root")).ok_or("not a git project")?;
    let mut worktrees = Vec::new();
    for row in result.get("worktrees").map(Value::list_or_empty).unwrap_or_default() {
        let Some(path) = text(row.get("path")) else { continue };
        let bare = row.get("is_bare").truthy();
        let detached = row.get("is_detached").truthy();
        worktrees.push(Worktree {
            path: path.to_owned(),
            branch: if bare || detached { None } else {
                text(row.get("branch")).map(|branch| branch.strip_prefix("refs/heads/").unwrap_or(branch).to_owned())
            },
            detached,
            bare,
            prunable: row.get("is_prunable").truthy(),
            is_main: Path::new(path) == Path::new(repo_root),
            open_workspace_id: text(row.get("open_workspace_id")).map(str::to_owned),
            ..Worktree::default()
        });
    }
    if worktrees.is_empty() {
        return Err("not a git project".to_owned());
    }

    // Remote's merge ref is authoritative: the local branch name can differ,
    // and a remote's fetch refspec need not use refs/remotes/<remote>/.
    let refs = git(repo_root, &[
        "for-each-ref",
        "--format=%(refname:lstrip=2)%09%(objectname)%09%(upstream:remotename)%09%(upstream:remoteref)",
        "refs/heads",
    ]).ok_or("could not read git branches")?;
    let refs = String::from_utf8_lossy(&refs);
    let branches: HashMap<_, _> = refs.lines().filter_map(|line| {
        let mut fields = line.splitn(4, '\t');
        Some((fields.next()?, (fields.next()?, fields.next()?, fields.next()?)))
    }).collect();
    for worktree in &mut worktrees {
        let Some((head, remote, merge_ref)) = worktree.branch.as_deref().and_then(|branch| branches.get(branch)) else { continue };
        worktree.head = (*head).to_owned();
        if !remote.is_empty() && !merge_ref.is_empty() {
            worktree.upstream_remote = Some((*remote).to_owned());
            worktree.upstream_branch = Some(merge_ref.strip_prefix("refs/heads/").unwrap_or(merge_ref).to_owned());
        }
    }

    // One extra process for all detached rows, including paths with newlines.
    if worktrees.iter().any(|worktree| worktree.detached) {
        if let Some(output) = git(repo_root, &["worktree", "list", "--porcelain", "-z"]) {
            let output = String::from_utf8_lossy(&output);
            let mut path = None;
            let mut heads = HashMap::new();
            for field in output.split('\0') {
                if let Some(value) = field.strip_prefix("worktree ") {
                    path = Some(value);
                } else if let Some(head) = field.strip_prefix("HEAD ") {
                    if let Some(path) = path { heads.insert(path, head); }
                } else if field.is_empty() {
                    path = None;
                }
            }
            for worktree in &mut worktrees {
                if worktree.detached {
                    if let Some(head) = heads.get(worktree.path.as_str()) {
                        worktree.head = (*head).to_owned();
                    }
                }
            }
        }
    }

    let default_branch = git(repo_root, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|output| String::from_utf8(output).ok())
        .and_then(|output| output.trim().strip_prefix("origin/").filter(|name| !name.is_empty()).map(str::to_owned))
        .or_else(|| ["main", "master"].into_iter().find(|name| branches.contains_key(name)).map(str::to_owned));
    // Herdr may resolve the invoking workspace to main even for an explicit
    // linked cwd. The caller's directory identifies the actual source checkout.
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
    let source_checkout = worktrees.iter().filter_map(|worktree| {
        let root = std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| PathBuf::from(&worktree.path));
        canonical_cwd.starts_with(&root).then(|| (worktree, root.components().count()))
    }).max_by_key(|&(_, depth)| depth)
        .map(|(worktree, _)| worktree.path.as_str())
        .unwrap_or_else(|| text(source.get("source_checkout_path")).unwrap_or(cwd))
        .to_owned();
    Ok((Project {
        repo_root: repo_root.to_owned(),
        repo_name: text(source.get("repo_name")).unwrap_or_else(|| {
            Path::new(repo_root).file_name().and_then(|name| name.to_str()).unwrap_or(repo_root)
        }).to_owned(),
        source_checkout,
        default_branch,
    }, worktrees))
}

/// Agent discovery is best-effort; an unavailable server is an empty list.
pub fn agents(client: &mut HerdrClient) -> Vec<AgentHere> {
    let Ok(result) = client.call("agent.list", client::dict(&[]), &["agent", "list"]) else { return Vec::new() };
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
        }
    }).collect()
}

/// Assign each agent to only the deepest enclosing checkout, resolving symlinks.
pub fn agents_by_worktree(worktrees: &[Worktree], agents: &[AgentHere]) -> Vec<Vec<AgentHere>> {
    let mut paths: HashMap<&str, PathBuf> = HashMap::with_capacity(worktrees.len() + agents.len());
    for path in worktrees.iter().map(|worktree| worktree.path.as_str()).chain(agents.iter().map(|agent| agent.cwd.as_str())) {
        if !path.is_empty() {
            paths.entry(path).or_insert_with(|| std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path)));
        }
    }
    let roots: Vec<_> = worktrees.iter().map(|worktree| paths.get(worktree.path.as_str())).collect();
    let mut result = vec![Vec::new(); worktrees.len()];
    for agent in agents {
        let Some(cwd) = paths.get(agent.cwd.as_str()) else { continue };
        let best = roots.iter().enumerate().filter_map(|(index, root)| {
            let root = (*root)?;
            cwd.starts_with(root).then(|| (index, root.components().count()))
        }).max_by_key(|&(_, depth)| depth);
        if let Some((index, _)) = best { result[index].push(agent.clone()); }
    }
    result
}

fn parse_status(output: &[u8]) -> GitFacts {
    let mut facts = GitFacts { dirty: Some(0), ..GitFacts::default() };
    let mut records = output.split(|&byte| byte == 0);
    while let Some(record) = records.next() {
        if let Some(counts) = record.strip_prefix(b"# branch.ab ") {
            if let Ok(counts) = std::str::from_utf8(counts) {
                if let Some((ahead, behind)) = counts.split_once(' ') {
                    facts.ahead = ahead.strip_prefix('+').and_then(|n| n.parse().ok());
                    facts.behind = behind.strip_prefix('-').and_then(|n| n.parse().ok());
                }
            }
        } else if matches!(record.first(), Some(b'1' | b'2' | b'?' | b'u')) && record.get(1) == Some(&b' ') {
            facts.dirty = facts.dirty.map(|n| n.saturating_add(1));
            if record[0] == b'2' {
                // Rename/copy records have a second NUL-delimited original path.
                let _ = records.next();
            }
        }
    }
    facts
}

/// Blocking local detail collection; callers run this away from the UI thread.
pub fn git_facts(path: &str) -> GitFacts {
    let Some(status) = git(path, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=normal"]) else {
        return GitFacts::default();
    };
    let Some(log) = git(path, &["log", "-5", "--format=%ct%x09%h %s"]) else { return GitFacts::default() };
    let mut facts = parse_status(&status);
    for (index, line) in String::from_utf8_lossy(&log).lines().enumerate() {
        if let Some((epoch, commit)) = line.split_once('\t') {
            if index == 0 { facts.last_commit_epoch = epoch.parse().ok(); }
            facts.recent_commits.push(commit.to_owned());
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_prefers_focused_pane_then_workspace_then_checkout() {
        let context = parse_context(r#"{"focused_pane_cwd":"/pane","workspace_cwd":"/workspace","worktree":{"checkout_path":"/checkout"},"workspace_id":"w3"}"#);
        assert_eq!(context.cwd.as_deref(), Some("/pane"));
        assert_eq!(parse_context(r#"{"focused_pane_cwd":null,"workspace_cwd":"/workspace","worktree":{"checkout_path":"/checkout"}}"#).cwd.as_deref(), Some("/workspace"));
        assert_eq!(parse_context(r#"{"focused_pane_cwd":"","workspace_cwd":42,"worktree":{"checkout_path":"/checkout"}}"#).cwd.as_deref(), Some("/checkout"));
    }

    #[test]
    fn invalid_or_missing_context_fields_are_empty() {
        for json in ["not JSON", "null", "[]", "{}", r#"{"workspace_id":3,"worktree":false}"#] {
            let context = parse_context(json);
            assert!(context.cwd.is_none());
        }
    }

    #[test]
    fn porcelain_counts_renames_once_even_when_original_path_looks_like_a_record() {
        let facts = parse_status(b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +12 -3\0\
            1 .M N... 100644 100644 100644 aaa aaa modified\0\
            2 R. N... 100644 100644 100644 aaa aaa R100 renamed\0? misleading original\0\
            ? untracked\0u UU N... 100644 100644 100644 100644 aaa aaa aaa conflicted\0");
        assert_eq!(facts.dirty, Some(4));
        assert_eq!(facts.ahead, Some(12));
        assert_eq!(facts.behind, Some(3));
    }

    #[test]
    fn porcelain_without_upstream_has_unknown_counts_and_ignores_ignored_paths() {
        let facts = parse_status(b"# branch.oid (initial)\0# branch.head topic\0! ignored\0? new\nfile\0");
        assert_eq!(facts.dirty, Some(1));
        assert_eq!(facts.ahead, None);
        assert_eq!(facts.behind, None);
        assert_eq!(parse_status(b"# branch.ab +0 -0\0").ahead, Some(0));
        assert_eq!(parse_status(b"").dirty, Some(0));
    }

    #[test]
    fn agents_choose_longest_component_prefix_and_preserve_order() {
        let worktrees = ["/a/app", "/a/app/.worktrees/x", "/a/other"].map(|path| Worktree { path: path.into(), ..Worktree::default() });
        let agents = ["/a/app/src", "/a/app/.worktrees/x/src", "/a/app-wt", "/a/app", "/a/app/.worktrees/xyz", ""]
            .map(|cwd| AgentHere { cwd: cwd.into(), ..AgentHere::default() });
        let grouped = agents_by_worktree(&worktrees, &agents);
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped[0].iter().map(|a| a.cwd.as_str()).collect::<Vec<_>>(), ["/a/app/src", "/a/app", "/a/app/.worktrees/xyz"]);
        assert_eq!(grouped[1].len(), 1);
        assert_eq!(grouped[1][0].cwd, "/a/app/.worktrees/x/src");
        assert!(grouped[2].is_empty());
        assert!(agents_by_worktree(&[], &agents).is_empty());
    }
}
