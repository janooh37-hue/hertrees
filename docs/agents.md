<p align="center"><img src="../branding/logo/icon/icon-64.png" alt="" width="32"></p>

# Using Hertrees from an agent

Use this guide when operating the plugin, not changing its implementation. Start with
`bin/hertrees --help` in the plugin checkout (or `hertrees --help` if it is on `PATH`).
Examples below use `bin/hertrees` and an example repository at `/tmp/demo`; substitute
real paths and installed agent kinds. Session commands require a running Herdr server.

For exact interfaces, inspect `src/cli.rs` (`list`, `print_presets`, `parse_start`,
`start`), `src/presets.rs` (files and names), and `src/config.rs` (lookup and keys).
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

Returns `{"dir": "/…/presets", "presets": […]}`. Each entry has `name`, `path` and
`prompt` strings; `prompt` is the unexpanded template. `dir` can be null if no config
location is available. This command lists only file presets, not the built-in `blank`
(no first prompt) or `custom` (interactive input) choices. It may seed the initial
files as described under [preset files](#preset-files).

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

# A new worktree containing only a shell, with no agent or prompt.
bin/hertrees start --cwd /tmp/demo --branch scratch --agent shell --focus
```

New worktrees are created at `<repo_root>/.worktrees/<branch>`; branch names with
slashes keep their directory structure (for example, `/tmp/demo/.worktrees/feat/cache`).
This uses the main repository root even when `--cwd` points inside a linked checkout.
Hertrees adds `/.worktrees/` to Git's local `info/exclude` when the directory is not
already ignored; it does not change or commit your `.gitignore`.

Omitting both prompt options selects `blank`. `--preset NAME` selects a file by its
case-sensitive name, or `blank`; `custom` requires `--prompt TEXT` instead. `--preset`
and `--prompt` are mutually exclusive. Both file and direct prompts expand the
[placeholders](#placeholders). `--agent shell` is only valid with `--branch`; leave
prompt options off because a shell has no agent to receive them. Focus remains
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

## Preset files

Treat presets as user-owned Markdown files: `<config_dir>/presets/<name>.md`, with the
whole file as the prompt (no frontmatter required). Internal newlines are preserved;
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

Other brace expressions are left alone. `{pr}` being `none` does not distinguish a
failed lookup from absence; use the discovery JSON for that distinction.

## Configuration

`config_dir` is the first available location:

1. Nonempty `HERDR_PLUGIN_CONFIG_DIR`.
2. `$XDG_CONFIG_HOME/herdr/plugins/config/hertrees` when `XDG_CONFIG_HOME` is nonempty.
3. `~/.config/herdr/plugins/config/hertrees`.

Hertrees writes its private state (the PR cache in `pr-v1/`, and short-lived deferred
prompt files) to `HERDR_PLUGIN_STATE_DIR`, otherwise `$XDG_STATE_HOME/herdr/plugins/hertrees`
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
| `refresh_ms` | Popup refresh interval, default 900 ms; minimum 200 |
| `selection_background` | `"auto"` (default), a color, or `null` for no background override |
| `colors` | Role-to-color object, default `{}`; roles in `src/theme.rs::DEFAULTS` |
| `wordmark` | Where the name is drawn: `"search"` (default) puts `❯ hertrees │` on the search line; `"pixel"` (3 rows), `"pixel-small"`, `"braille"`, `"sextant"` (2 rows each) and `"icon"` (icon, name, tagline and counts; 9 rows) draw it above a plain search line; `"off"` shows no name. Unknown values use the default. A pixel style falls back to `"search"` when the popup's inner width is narrower than the art (36, 35, 20, 18 and 46 columns) or the list would keep fewer than 5 rows below it; `"search"` itself drops to the plain prompt under 60 columns. `"sextant"` needs a font with Unicode 13 sextants (U+1FB00), else it shows boxes |

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
  agent, then `⇥` cycles presets; `^e` manages them and `^s` saves a custom prompt. If the agent
  stops at a startup question (Claude's folder-trust prompt), the prompt waits until you answer.
