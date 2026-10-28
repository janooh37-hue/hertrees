<p align="center"><img src="../branding/logo/icon/icon-64.png" alt="" width="32"></p>

# Using Hertrees from an agent

Use this guide when operating the plugin, not changing its implementation. Start with
`bin/hertrees --help` in the plugin checkout (or `hertrees --help` if it is on `PATH`).
Examples below use `bin/hertrees` and an example repository at `/tmp/demo`; substitute
real paths and installed agent kinds. Session commands require a running Herdr server.

For exact interfaces, inspect `src/cli.rs` (`list`, `print_presets`, `parse_start`,
`start`, `send`, `clean`), `src/presets.rs` (files and names), and `src/config.rs` (lookup and keys).
These are authoritative if an installed binary differs from this guide. Headless commands
print JSON to stdout; command errors use exit status **2** and stderr prefixed `hertrees:`.
Parse JSON rather than scraping the popup. Exit status 0 from `start` reports a launched
session, not completion of the agent's task.

## Discover checkouts

```sh
bin/hertrees --list --cwd /tmp/demo
```

`--cwd` selects the repository; without it, Hertrees uses the focused-pane context when
provided, otherwise the shell cwd outside plugin mode. The JSON object contains:

- `repo_root`: main repository path; `source_checkout`: checkout containing the supplied
  directory; `default_branch`: branch name or null.
- `worktrees`: an array of objects, each with `path`, `branch` (nullable), `head`, `main`
  and `detached` booleans, and `workspace` (Herdr workspace ID or null).
- Each worktree also has `pr_state` (`merged`, `open`, `draft`, `closed`, `none`,
  `unknown`), `pr_number` and `pr_url` (nullable), `pr_via_upstream` (boolean),
  `pr_reason` (nullable), and `agents` (an array of `kind`, nullable `name`, `status`,
  and `pane_id` objects).

Choose an existing checkout by its returned `path`, not its branch label. For
`pr_state: "unknown"`, inspect `pr_reason`; missing data is not evidence of no PR.

## Choose a prompt

```sh
bin/hertrees presets
```

Returns `{"dir": "/…/presets", "repo_dir": "/…/.hertrees/presets", "presets": […]}`.
Each entry has `name`, `path` and `prompt` strings (`prompt` is the unexpanded template,
without its settings block), booleans `repo` (from the repository's `repo_dir`) and `ask`,
and `agent`/`description` strings or null. `dir` can be null if no config location is
available; `repo_dir` is null outside a git repository. This command lists only file
presets, not the built-in `blank` (no first prompt) or `custom` (interactive input)
choices. It may seed the initial files as described under [preset files](#preset-files).

## Start a session

Pass exactly one target: `--path` for a checkout returned by `--list`, or `--branch`
for a **new** branch/worktree. `--base` is valid only with `--branch`; omit it to use
Herdr's default base. An existing workspace gets a fresh tab for the new agent.
`--agent` is required and must name a supported agent installed on `PATH`; popup
`agents`/`default_agent` preferences do not select or restrict this explicit CLI choice.

```sh
# Existing checkout; plan must appear in the presets listing.
bin/hertrees start --cwd /tmp/demo --path /tmp/demo --agent claude --preset plan

# New branch with a literal first-prompt template, focusing the destination.
bin/hertrees start --cwd /tmp/demo --branch feat/cache --base main \
  --agent codex --prompt 'Implement the cache in {path} on {branch}.' --focus

# A new worktree containing only a shell, with no agent or prompt; its space named.
bin/hertrees start --cwd /tmp/demo --branch scratch --label 'scratch pad' --agent shell --focus

# A shell tab in an existing checkout's space.
bin/hertrees start --cwd /tmp/demo --path /tmp/demo --agent shell
```

New worktrees are created at `<repo_root>/.worktrees/<branch>`; branch names with
slashes keep their directory structure (for example, `/tmp/demo/.worktrees/feat/cache`).
This uses the main repository root even when `--cwd` points inside a linked checkout.
Hertrees adds `/.worktrees/` to Git's local `info/exclude` when the directory is not
already ignored; it does not change or commit your `.gitignore`.

Omitting both prompt options selects `blank`. `--preset NAME` selects a file by its
case-sensitive name, or `blank`; `custom` requires `--prompt TEXT`. Given both, `--prompt`
is added to the preset the way the popup's editor adds it: at the preset's `{input}`,
otherwise below it after a blank line. The result expands the
[placeholders](#placeholders); a placeholder the prompt defines itself (`{ticket}`)
needs `--var ticket=VALUE`, and a missing one is an error naming each `--var` required. `--agent shell` opens a tab (with `--path`) or a new
worktree (with `--branch`) with no agent; leave prompt options off because a shell has
no agent to receive them. `--label NAME` names a new worktree's space (default: the
branch) and is valid only with `--branch`. Focus remains
unchanged unless `--focus` is supplied.

### Session results

`start` returns one object with string fields `workspace_id`, `tab_id`, `pane_id`,
`agent`, `path`, plus boolean `prompt_deferred`. `agent` is the assigned session name
(not its kind), or the empty string for a shell; `path` is the actual checkout.

`prompt_deferred: true` means the initial prompt is queued for startup readiness,
not yet acknowledged as submitted. A folder-trust question or slow startup can cause
this. Inspect the returned pane and resolve any startup question with the user's
permission; the queued prompt is delivered automatically after readiness. Avoid
resubmitting it yourself. The sender stops if the agent disappears or after 15 minutes,
so this flag is not a delivery guarantee. `false` means no prompt was requested or
submission succeeded; it is not evidence that the task finished.

## Prompt a running agent

```sh
bin/hertrees send --cwd /tmp/demo --agent claude-feat-cache --preset review --prompt 'focus on errors'
```

`--agent` is a running agent's herdr name (as in `--list` or `herdr agent list`) or its
pane id, and must belong to the project at `--cwd`. `--preset`, `--prompt` and `--var`
work as for `start` (at least one of the first two), with placeholders filled for that
agent's worktree. It returns `{"agent": NAME, "deferred": BOOL}`. `deferred: false` means
the agent was idle and started working on it; `true` means it was busy or waiting on a
question, and a detached sender delivers the prompt once it is idle (it stops after 15
minutes or if the agent exits). The popup's `^s` does the same.

## Clean up worktrees

```sh
bin/hertrees clean --cwd /tmp/demo --merged --dry-run       # what would go, and why the rest stay
bin/hertrees clean --cwd /tmp/demo --merged                 # every merged worktree that is safe
bin/hertrees clean --cwd /tmp/demo --path /tmp/demo/.worktrees/feat/cache
```

Pass exactly one target: `--path` for a checkout returned by `--list`, or `--merged` for
every worktree whose `pr_state` is `merged`. Each one's space is closed (with everything
running in it), its checkout removed and, only when GitHub merged the PR at exactly the
checkout's current commit, its local branch deleted; otherwise the branch stays. It is
the popup's `alt+d` / `alt+⇧d` under the same refusals: the main checkout, the checkout
`--cwd` is in, uncommitted or untracked changes, and an agent that is `working` or
`blocked`. Nothing is forced. Idle agents in the space are closed with it, so check
`--list` first when they matter.

It returns `{"dry_run": BOOL, "removed": […], "kept": […]}`. `removed` entries have `path`,
`branch` (nullable), `workspace` (the closed space's id, or null) and `branch_deleted`;
with `--dry-run` they are what would be removed and nothing changes. `kept` entries have
`path`, `branch` and `reason`. With `--merged`, a worktree whose `pr_state` is `unknown`
is kept with the PR's reason rather than treated as unmerged, and a removal that fails is
kept with the error; the exit status is still 0. With `--path`, a refusal or failure is an
error (`hertrees: feat stays: it has 1 uncommitted change`, exit status 2).

## Preset files

Treat presets as user-owned Markdown files: `<config_dir>/presets/<name>.md`, with the
whole file as the prompt, after an optional settings block:

```markdown
---
ask: true
agent: codex
description: one line shown in the preset manager
---
Prompt text…
```

`ask: true` (also `yes`/`on`) makes the popup's `⏎` open the prompt editor under the
preset; a preset containing `{input}` does that too. `agent` selects that agent first in
the popup. Unknown keys are ignored, and a file without a closing `---` is all prompt.
A repository can share presets in `<repo_root>/.hertrees/presets/` (the `repo_dir` of
`presets`); they are listed with `repo: true`, and a user preset with the same name
(case-insensitively) hides the repository's. Internal newlines are preserved;
outer whitespace is trimmed on load. Read the `dir` from `presets` rather than guessing
the user's config path. To create a preset there, write a new file without overwriting
an existing name; edit its content, rename the file, or delete it with normal file
operations. No server reload is needed; headless commands load fresh files and the
popup reloads when entering its preset manager or returning from its editor.

For example, once `preset_dir` is set to the returned `dir`:

```sh
# Create only if review.md does not already exist.
(set -C; printf '%s\n' 'Review {branch} against {base} in {path}; report issues.' > "$preset_dir/review.md")
# Edit review.md with your file-editing tool, then run:
bin/hertrees start --cwd /tmp/demo --path /tmp/demo --agent claude --preset review
# When no longer wanted, delete review.md with your file-removal tool.
```

Names are nonempty after trimming, at most 40 characters, cannot start with `.`, and
cannot contain `/` or NUL. `blank` and `custom` are reserved case-insensitively. Use
names without surrounding whitespace. Only valid `.md` files are loaded, sorted
case-insensitively by name (case-sensitive tie-break). `plan.md` and `research.md` are
seeded **only when the presets directory does not exist**; editing, renaming or deleting
them sticks. An existing empty directory stays empty. Built-in choices are not files.

### Placeholders

| Token | Expansion |
|---|---|
| `{branch}` | Checkout's branch label; new branch name when creating |
| `{base}` | Explicit base, otherwise detected default branch, otherwise `HEAD` |
| `{path}` | Actual destination checkout path |
| `{repo}` | Repository name, not its filesystem root |
| `{pr}` | `#N URL` when available, otherwise `none` |
| `{pr_title}` | The PR's title, otherwise `none` |
| `{pr_body}` | The PR's description (`gh pr view`), `none`, or `unavailable` |
| `{commits}` | `git log --oneline base..HEAD` (at most 50), `none` for a new worktree |
| `{diffstat}` | `git diff --stat base...HEAD`, `none` for a new worktree |
| `{input}` | Where `--prompt` (or the popup editor's text) goes inside a preset |

`{pr_body}`, `{commits}` and `{diffstat}` run only when a prompt uses them; `unavailable`
means the command failed. Any other `{name}` (a lowercase letter, then lowercase letters,
digits or `_`) is a field: the popup asks for it, headless commands need `--var`. Other
brace expressions (JSON, `{ spaced }`, `{Upper}`) are left alone. `{pr}` being `none`
does not distinguish a failed lookup from absence; use the discovery JSON for that.

## Configuration

`config_dir` is the first available location:

1. Nonempty `HERDR_PLUGIN_CONFIG_DIR`.
2. `$XDG_CONFIG_HOME/herdr/plugins/config/hertrees` when `XDG_CONFIG_HOME` is nonempty.
3. `~/.config/herdr/plugins/config/hertrees`.

Hertrees writes its private state (the PR cache in `pr-v1/`, short-lived deferred
prompt files, and the popup editor's last 50 prompts in `prompt-history.json`) to `HERDR_PLUGIN_STATE_DIR`, otherwise `$XDG_STATE_HOME/herdr/plugins/hertrees`
or `~/.local/state/herdr/plugins/hertrees`. Outside these two plugin directories it only
creates worktrees and the `/.worktrees/` entry in Git's local `info/exclude`.

`config.json` in that directory is optional. Missing or invalid files fall back to
defaults; unknown keys are ignored. Presets belong in files, not a JSON `presets` key.

| Key | Meaning / default |
|---|---|
| `agents` | Ordered popup agent kinds; `[]` means all supported agents found on `PATH` |
| `default_agent` | Initially selected popup agent; unset by default |
| `pr_cache_ttl_s` | PR cache freshness, default 60 seconds; clamped to 0–86400 |
| `scope` | Initial popup scope: `all` (default), `nopr`, `open`, `draft`, `merged`, `closed`, `agents` |
| `preview` | `"auto"` (default), `true` or `false`; auto fits a preview when space allows |
| `mouse` | Enable popup mouse input; default `true` |
| `agent_icons` | `"auto"` detects the icon font (default), `"font"` forces it; other values disable icons |
| `icons` | Worktree, space and tab glyphs: `"auto"` (default) uses Nerd Font glyphs when one is installed (fontconfig covers U+F418, or a `*NerdFont*` file in a font folder), else `⎇ ◫ ▭`; `"nerd"`, `"unicode"` or `"off"` |
| `refresh_ms` | Popup refresh interval, default 900 ms; minimum 200 |
| `selection_background` | `"auto"` (default), a color, or `null` for no background override |
| `colors` | Role-to-color object, default `{}`; roles in `src/theme.rs::DEFAULTS` |
| `wordmark` | Where the name is drawn: `"search"` (default) puts `❯ hertrees │` on the search line; `"braille"` (2 rows) and `"icon"` (icon, name, tagline and counts; 9 rows) draw it above a plain search line Unknown values (including the removed `"pixel"`, `"pixel-small"`, `"sextant"` and `"off"`) use the default. An art style falls back to `"search"` when the popup's inner width is narrower than the art (20 and 46 columns) or the list would keep fewer than 5 rows below it; `"search"` itself drops to the plain prompt under 60 columns. `^l` in the popup's list switches to the next style and writes this key, keeping the rest of the file; a `config.json` that is not a JSON object is left unchanged |

Colors accept ANSI names, 0–255 palette indices, or `#RRGGBB`. For a minimal example:

```json
{
  "agents": ["claude", "codex"],
  "default_agent": "codex",
  "pr_cache_ttl_s": 60,
  "colors": { "pr_merged": "bright_green" }
}
```

## Interface details

- **PR matching:** branches are matched by name and owner, so fork PRs with the same branch
  name are ignored; a branch whose upstream has another name is matched through the upstream
  (never when that upstream is the default branch).
- **`○ PR ?` reasons:** `gh` missing, not logged in, offline, non-GitHub remote, or detached
  HEAD. The picker never shows "no PR" when it could not check.
- **Links:** the `#N` chip is an OSC 8 hyperlink and a click target; the preview shows the PR
  title and URL.
- **New sessions:** `^t` opens a new tab if the worktree's workspace is already open. Pick the
  agent, then `⇥` cycles presets, `⏎` sends one as is and `^o` adds your own text under it;
  `^e` manages presets. `^s` on a worktree sends a preset or prompt to its running agent. If the agent
  stops at a startup question (Claude's folder-trust prompt), the prompt waits until you answer.
