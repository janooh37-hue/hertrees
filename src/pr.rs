//! Repository-wide GitHub PR history, last-good disk cache, and branch matching.

use crate::model::{PrInfo, PrState, PullRequest, Worktree};
use crate::proc::{self, RunError};
use crate::pyjson::{self, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhRepo {
    pub host: String,
    pub owner: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct PrSnapshot {
    pub fetched_at: i64,
    /// Newest PR number first, regardless of subsequent comments or updates.
    pub prs: Vec<PullRequest>,
    pub capped: bool,
}

pub const LIMIT: usize = 300;

/// Prefer origin, otherwise the first configured remote. Never contacts a remote.
pub fn github_repo(repo_root: &str) -> Result<GhRepo, String> {
    let argv = ["git", "-C", repo_root, "remote", "-v"].map(str::to_owned);
    let output = proc::run(&argv, 5.0).map_err(|_| "no git remote".to_owned())?;
    if !output.success {
        return Err("no git remote".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut selected = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(name), Some(url)) = (fields.next(), fields.next()) else { continue };
        if selected.is_none() || name == "origin" {
            selected = Some(url);
        }
        if name == "origin" {
            break;
        }
    }
    let url = selected.ok_or_else(|| "no git remote".to_owned())?;
    parse_remote(url, std::env::var("GH_HOST").ok().as_deref())
        .ok_or_else(|| "remote is not on GitHub".to_owned())
}

fn parse_remote(url: &str, enterprise: Option<&str>) -> Option<GhRepo> {
    if url.chars().any(char::is_control) {
        return None;
    }
    let (authority, path) = if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("ssh://")) {
        rest.split_once('/')?
    } else {
        let (authority, path) = url.split_once(':')?;
        // SCP syntax requires a user; do not mistake a local path for a host.
        if !authority.contains('@') {
            return None;
        }
        (authority, path)
    };
    let host = authority.rsplit('@').next()?;
    // An explicit SSH port belongs to the transport, not GitHub's repo identity.
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    if !host.eq_ignore_ascii_case("github.com")
        && !enterprise.is_some_and(|allowed| host.eq_ignore_ascii_case(allowed))
    {
        return None;
    }
    let path = path.trim_end_matches('/');
    let (owner, name) = path.split_once('/')?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    let safe = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if !safe(owner) || !safe(name) {
        return None;
    }
    Some(GhRepo { host: host.to_ascii_lowercase(), owner: owner.into(), name: name.into() })
}

/// Read the last successful snapshot. Missing or malformed files are a cache miss.
pub fn cached(repo: &GhRepo) -> Option<PrSnapshot> {
    let bytes = fs::read(cache_path(repo)?).ok()?;
    decode_snapshot(&pyjson::loads_bytes(&bytes).ok()?)
}

/// Fetch one repository-wide batch; failures leave the last-good cache untouched.
pub fn fetch(repo: &GhRepo) -> Result<PrSnapshot, String> {
    let argv = vec![
        "gh".into(), "pr".into(), "list".into(), "-R".into(),
        format!("{}/{}/{}", repo.host, repo.owner, repo.name),
        "--state".into(), "all".into(), "--limit".into(), LIMIT.to_string(),
        "--json".into(),
        "number,title,url,state,isDraft,headRefName,headRefOid,headRepositoryOwner,baseRefName,createdAt,mergedAt".into(),
    ];
    let output = proc::run(&argv, 20.0).map_err(|error| match error {
        RunError::Timeout => "timeout".into(),
        RunError::Spawn(error) if error.kind() == std::io::ErrorKind::NotFound => "gh not installed".into(),
        RunError::Spawn(error) => gh_failure(&error.to_string()),
    })?;
    if !output.success {
        return Err(gh_failure(&String::from_utf8_lossy(&output.stderr)));
    }
    let value = pyjson::loads_bytes(&output.stdout).map_err(|_| "gh failed: invalid response".to_owned())?;
    let prs = parse_prs(&value).ok_or_else(|| "gh failed: invalid response".to_owned())?;
    let snapshot = PrSnapshot { fetched_at: now(), capped: prs.len() >= LIMIT, prs };
    if let Some(path) = cache_path(repo) {
        let _ = write_cache(&path, &snapshot);
    }
    Ok(snapshot)
}

fn gh_failure(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("rate limit") || lower.contains("rate_limit") || lower.contains("rate-limit") {
        "rate limited".into()
    } else if lower.contains("gh auth login") || lower.contains("not logged")
        || lower.contains("authentication") || lower.contains("bad credentials")
        || lower.contains("http 401") || lower.contains("gh_token") || lower.contains("github_token")
    {
        "gh not logged in (run gh auth login)".into()
    } else if lower.contains("timeout") || lower.contains("timed out") || lower.contains("deadline exceeded") {
        "timeout".into()
    } else if lower.contains("error connecting") || lower.contains("could not resolve")
        || lower.contains("no such host") || lower.contains("network is unreachable")
        || lower.contains("connection refused") || lower.contains("connection reset")
        || lower.contains("dial tcp") || lower.contains("temporary failure in name resolution")
        || lower.contains("offline")
    {
        "offline".into()
    } else {
        let first = stderr.lines().next().unwrap_or("").trim();
        let first: String = first.chars().filter(|c| !c.is_control()).take(80).collect();
        format!("gh failed: {}", if first.is_empty() { "unknown error" } else { &first })
    }
}

fn parse_prs(value: &Value) -> Option<Vec<PullRequest>> {
    let Value::List(rows) = value else { return None };
    let mut prs = Vec::with_capacity(rows.len());
    for row in rows {
        let Value::Int(number) = row.get("number") else { return None };
        let number = u64::try_from(*number).ok().filter(|n| *n > 0)?;
        let draft = match row.get("isDraft") {
            Value::Bool(draft) => *draft,
            _ => return None,
        };
        let state = match row.get("state").as_str()? {
            "OPEN" if draft => PrState::Draft,
            "OPEN" => PrState::Open,
            "MERGED" => PrState::Merged,
            "CLOSED" => PrState::Closed,
            _ => return None,
        };
        let head_owner = match row.get("headRepositoryOwner") {
            Value::Null => String::new(),
            owner => owner.get("login").as_str()?.to_owned(),
        };
        let merged_at = match row.get("mergedAt") {
            Value::Null => None,
            Value::Str(timestamp) => Some(timestamp.clone()),
            _ => return None,
        };
        prs.push(PullRequest {
            number, state, head_owner, merged_at,
            title: row.get("title").as_str()?.into(),
            url: row.get("url").as_str()?.into(),
            head_ref: row.get("headRefName").as_str()?.into(),
            // Absent from caches written before 0.2.5.
            head_oid: row.get("headRefOid").as_str().unwrap_or("").into(),
            base_ref: row.get("baseRefName").as_str()?.into(),
            created_at: row.get("createdAt").as_str()?.into(),
        });
    }
    prs.sort_unstable_by(|a, b| b.number.cmp(&a.number));
    Some(prs)
}

fn encode_snapshot(snapshot: &PrSnapshot) -> Value {
    let rows = snapshot.prs.iter().map(|pr| {
        let mut row = Map::new();
        row.insert("number", Value::Int(pr.number.into()));
        row.insert("title", Value::str(&pr.title));
        row.insert("url", Value::str(&pr.url));
        row.insert("state", Value::str(match pr.state {
            PrState::Merged => "MERGED",
            PrState::Closed => "CLOSED",
            _ => "OPEN",
        }));
        row.insert("isDraft", Value::Bool(pr.state == PrState::Draft));
        row.insert("headRefName", Value::str(&pr.head_ref));
        row.insert("headRefOid", Value::str(&pr.head_oid));
        let mut owner = Map::new();
        owner.insert("login", Value::str(&pr.head_owner));
        row.insert("headRepositoryOwner", Value::Dict(owner));
        row.insert("baseRefName", Value::str(&pr.base_ref));
        row.insert("createdAt", Value::str(&pr.created_at));
        row.insert("mergedAt", pr.merged_at.as_deref().map_or(Value::Null, Value::str));
        Value::Dict(row)
    }).collect();
    let mut result = Map::new();
    result.insert("fetched_at", Value::Int(snapshot.fetched_at.into()));
    result.insert("capped", Value::Bool(snapshot.capped));
    result.insert("prs", Value::List(rows));
    Value::Dict(result)
}

fn decode_snapshot(value: &Value) -> Option<PrSnapshot> {
    let Value::Int(fetched_at) = value.get("fetched_at") else { return None };
    let fetched_at = i64::try_from(*fetched_at).ok().filter(|time| *time >= 0)?;
    let Value::Bool(capped) = value.get("capped") else { return None };
    let prs = parse_prs(value.get("prs"))?;
    Some(PrSnapshot { fetched_at, capped: *capped, prs })
}

fn cache_path(repo: &GhRepo) -> Option<PathBuf> {
    // The plugin's own state directory, so nothing lands outside Herdr's plugin dirs.
    let base = crate::config::state_dir()?;
    if !base.is_absolute() {
        return None;
    }
    // Public GhRepo fields must not be able to introduce path separators.
    let component = |text: &str| {
        let mut result = String::new();
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                result.push(char::from(byte));
            } else {
                use std::fmt::Write;
                let _ = write!(result, "%{byte:02X}");
            }
        }
        result
    };
    Some(base.join("pr-v1").join(format!("{}__{}__{}.json",
        component(&repo.host), component(&repo.owner), component(&repo.name))))
}

fn write_cache(path: &std::path::Path, snapshot: &PrSnapshot) -> std::io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or_else(|| std::io::Error::other("cache has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    crate::host::owner_only(&mut options);
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(pyjson::dumps(&encode_snapshot(snapshot), false).as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().min(i64::MAX as u64) as i64).unwrap_or(0)
}

pub fn is_fresh(snapshot: &PrSnapshot, ttl_s: i64) -> bool {
    now().checked_sub(snapshot.fetched_at).is_some_and(|age| age >= 0 && age < ttl_s)
}

/// Exact local heads win; an explicitly tracked non-default upstream is fallback only.
pub fn classify(wt: &Worktree, repo: &GhRepo, default_branch: Option<&str>, snapshot: &PrSnapshot) -> PrInfo {
    if wt.bare {
        return PrInfo::unknown("bare");
    }
    let branch = match wt.branch.as_deref() {
        Some(branch) if !branch.is_empty() && !wt.detached => branch,
        _ => return PrInfo::unknown("detached HEAD"),
    };
    let none = || PrInfo { state: PrState::None, pr: None, via_upstream: false, reason: None };
    if wt.is_main && Some(branch) == default_branch {
        return none();
    }
    let latest = |branch: &str| {
        snapshot.prs.iter().filter(|pr| pr.head_ref == branch
            && (pr.head_owner.is_empty() || pr.head_owner.eq_ignore_ascii_case(&repo.owner)))
            .max_by_key(|pr| pr.number)
    };
    let mut candidate = latest(branch);
    let mut via_upstream = false;
    if candidate.is_none() && wt.upstream_remote.as_deref().is_some_and(|remote| !remote.is_empty()) {
        if let Some(upstream) = wt.upstream_branch.as_deref()
            .filter(|upstream| !upstream.is_empty() && *upstream != branch && Some(*upstream) != default_branch)
        {
            candidate = latest(upstream);
            via_upstream = candidate.is_some();
        }
    }
    if let Some(pr) = candidate {
        PrInfo { state: pr.state, pr: Some(pr.clone()), via_upstream, reason: None }
    } else if snapshot.capped {
        PrInfo::unknown(format!("not in the latest {LIMIT} PRs"))
    } else {
        none()
    }
}

/// Launch a detached browser helper without inheriting the popup's TTY.
pub fn open_url(url: &str) -> Result<(), String> {
    // On Windows the URL protocol handler opens the default browser; `cmd /c
    // start` would read `&` and `^` in the URL as shell syntax.
    let (opener, args): (&str, &[&str]) = if cfg!(windows) {
        ("rundll32.exe", &["url.dll,FileProtocolHandler"])
    } else if cfg!(target_os = "macos") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    };
    let mut command = Command::new(opener);
    command.args(args).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::host::detach(&mut command);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{opener} not installed")
        } else {
            format!("cannot open browser: {error}")
        }
    })?;
    // Reap off-thread: the popup never waits for a browser, nor leaves zombies.
    std::thread::spawn(move || { let _ = child.wait(); });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> GhRepo {
        GhRepo { host: "github.com".into(), owner: "Owner".into(), name: "repo".into() }
    }

    fn pr(number: u64, branch: &str, state: PrState) -> PullRequest {
        PullRequest {
            number, title: "title".into(), url: format!("https://github.com/Owner/repo/pull/{number}"),
            state, head_ref: branch.into(), head_oid: String::new(), head_owner: "oWNER".into(), base_ref: "main".into(),
            created_at: "2026-10-25T00:00:00Z".into(), merged_at: None,
        }
    }

    fn wt(branch: &str) -> Worktree {
        Worktree { branch: Some(branch.into()), ..Worktree::default() }
    }

    fn snapshot(prs: Vec<PullRequest>) -> PrSnapshot {
        PrSnapshot { fetched_at: 123, prs, capped: false }
    }

    #[test]
    fn remote_url_forms_and_enterprise_allowlist() {
        for url in ["https://github.com/Owner/repo.git", "https://github.com/Owner/repo",
            "ssh://git@github.com/Owner/repo.git", "git@github.com:Owner/repo.git",
            "ssh://git@github.com:22/Owner/repo.git"]
        {
            assert_eq!(parse_remote(url, None), Some(repo()), "{url}");
        }
        assert_eq!(parse_remote("git@git.example.com:Owner/repo.git", Some("git.example.com")),
            Some(GhRepo { host: "git.example.com".into(), ..repo() }));
        for url in ["https://gitlab.com/Owner/repo.git", "git@git.example.com:Owner/repo.git",
            "https://github.com/Owner/repo/extra", "/tmp/local", "github.com:Owner/repo",
            "https://github.com/../repo", "https://github.com/Owner/repo\x1b"]
        {
            assert!(parse_remote(url, None).is_none(), "{url}");
        }
    }

    #[test]
    fn forks_are_ignored_and_highest_number_wins() {
        let mut foreign = pr(300, "topic", PrState::Open);
        foreign.head_owner = "fork".into();
        let history = snapshot(vec![pr(3, "topic", PrState::Merged), foreign, pr(5, "topic", PrState::Closed)]);
        let result = classify(&wt("topic"), &repo(), None, &history);
        assert_eq!(result.pr.unwrap().number, 5);
        assert_eq!(result.state, PrState::Closed);
        let mut deleted_owner = pr(8, "topic", PrState::Open);
        deleted_owner.head_owner.clear();
        assert_eq!(classify(&wt("topic"), &repo(), None, &snapshot(vec![deleted_owner])).state, PrState::Open);
        let mut foreign = pr(9, "topic", PrState::Open);
        foreign.head_owner = "fork".into();
        assert_eq!(classify(&wt("topic"), &repo(), None, &snapshot(vec![foreign])).state, PrState::None);
    }

    #[test]
    fn upstream_fallback_requires_remote_and_never_uses_default() {
        let mut tree = wt("renamed");
        tree.upstream_branch = Some("topic".into());
        let history = snapshot(vec![pr(10, "topic", PrState::Merged), pr(20, "main", PrState::Merged)]);
        assert_eq!(classify(&tree, &repo(), Some("main"), &history).state, PrState::None);
        tree.upstream_remote = Some("origin".into());
        let result = classify(&tree, &repo(), Some("main"), &history);
        assert!(result.via_upstream);
        assert_eq!(result.pr.unwrap().number, 10);
        tree.upstream_branch = Some("main".into());
        assert_eq!(classify(&tree, &repo(), Some("main"), &history).state, PrState::None);
        tree.upstream_branch = Some("topic".into());
        let history = snapshot(vec![pr(10, "topic", PrState::Merged), pr(2, "renamed", PrState::Open)]);
        let result = classify(&tree, &repo(), Some("main"), &history);
        assert!(!result.via_upstream);
        assert_eq!(result.pr.unwrap().number, 2);
    }

    #[test]
    fn capped_detached_bare_and_main_are_distinct() {
        let mut history = snapshot(vec![pr(2, "main", PrState::Merged)]);
        history.capped = true;
        let unknown = classify(&wt("missing"), &repo(), Some("main"), &history);
        assert_eq!(unknown.state, PrState::Unknown);
        assert_eq!(unknown.reason.as_deref(), Some("not in the latest 300 PRs"));
        let mut tree = wt("main");
        tree.is_main = true;
        let main = classify(&tree, &repo(), Some("main"), &history);
        assert_eq!(main.state, PrState::None);
        assert!(main.reason.is_none());
        tree.detached = true;
        assert_eq!(classify(&tree, &repo(), None, &history).reason.as_deref(), Some("detached HEAD"));
        tree.bare = true;
        assert_eq!(classify(&tree, &repo(), None, &history).reason.as_deref(), Some("bare"));
        assert_eq!(classify(&Worktree::default(), &repo(), None, &history).state, PrState::Unknown);
    }

    #[test]
    fn draft_is_only_a_state_for_open_prs() {
        let history = snapshot(vec![pr(2, "topic", PrState::Draft)]);
        let json = encode_snapshot(&history);
        let parsed = decode_snapshot(&json).unwrap();
        assert_eq!(classify(&wt("topic"), &repo(), None, &parsed).state, PrState::Draft);
        let mut json = encode_snapshot(&history);
        let Value::List(rows) = json.as_dict_mut().unwrap().get_mut("prs").unwrap() else { panic!() };
        rows[0].as_dict_mut().unwrap().insert("state", Value::str("MERGED"));
        assert_eq!(decode_snapshot(&json).unwrap().prs[0].state, PrState::Merged);
    }

    #[test]
    fn cache_json_roundtrip_preserves_special_titles_and_all_states() {
        let mut history = snapshot(vec![pr(2, "b", PrState::Draft), pr(1, "a", PrState::Merged),
            pr(3, "c", PrState::Closed), pr(4, "d", PrState::Open)]);
        history.capped = true;
        history.prs[0].title = "quotes \" backslash \\ newline\n tab\t café 日本語 🌳".into();
        history.prs[1].merged_at = Some("2026-10-25T01:02:03Z".into());
        let encoded = pyjson::dumps(&encode_snapshot(&history), false);
        let decoded = decode_snapshot(&pyjson::loads(&encoded).unwrap()).unwrap();
        assert_eq!(decoded.fetched_at, history.fetched_at);
        assert_eq!(decoded.capped, history.capped);
        assert_eq!(decoded.prs.iter().map(|pr| pr.number).collect::<Vec<_>>(), vec![4, 3, 2, 1]);
        let original = &history.prs[0];
        let restored = &decoded.prs[2];
        assert_eq!(restored.title, original.title);
        assert_eq!(restored.url, original.url);
        assert_eq!(restored.head_ref, original.head_ref);
        assert_eq!(restored.head_owner, original.head_owner);
        assert_eq!(restored.created_at, original.created_at);
        assert_eq!(restored.state, original.state);
        assert_eq!(decoded.prs[3].merged_at, history.prs[1].merged_at);
        assert!(decode_snapshot(&pyjson::loads("{\"fetched_at\":123,\"capped\":false,\"prs\":[{}]}").unwrap()).is_none());
        assert!(decode_snapshot(&pyjson::loads("{}").unwrap()).is_none());
    }

    #[test]
    fn gh_errors_are_short_and_actionable() {
        assert_eq!(gh_failure("To get started with GitHub CLI, please run: gh auth login"), "gh not logged in (run gh auth login)");
        assert_eq!(gh_failure("API rate limit exceeded"), "rate limited");
        assert_eq!(gh_failure("error connecting to api.github.com"), "offline");
        assert_eq!(gh_failure("context deadline exceeded"), "timeout");
        assert_eq!(gh_failure("  first error  \nsecond error"), "gh failed: first error");
        assert_eq!(gh_failure(&"é".repeat(100)).chars().count(), "gh failed: ".len() + 80);
    }
}
