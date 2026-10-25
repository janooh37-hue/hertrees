# Changelog

## Unreleased

- The PR cache and deferred prompt files live in Herdr's plugin state directory
  (`HERDR_PLUGIN_STATE_DIR`, else `$XDG_STATE_HOME/herdr/plugins/hertrees`) instead of
  `~/.cache/hertrees` and the system temp directory. The old cache can be deleted.
- After a failed session start the popup reloads its worktrees, so a checkout that was
  created before the failure is listed instead of having its agent shown under main.
- README: key binding precedence, where to get the optional icon font, where `bin/` is.

## 0.2.1 (2026-10-25)

- New worktrees are created under the project's `.worktrees/` directory.
- `hertrees --version` shows the pixel mark on a terminal; piped output is unchanged.
- The popup's "not a git project" and "no worktrees" states show the mark when there is room.
- Brand assets and usage guide in `branding/`; README rewritten with a demo GIF and quick start.
- Standalone project: herdr-bar is credited for the original UI layer, with no runtime dependency.

## 0.2.0 (2026-10-25)

- Presets are user-owned Markdown files in `<config_dir>/presets/<name>.md`, seeded
  with `plan` and `research` only when the directory is missing. Deleting them sticks.
- Popup preset manager (`^e` from the agent picker): edit, create, rename and delete;
  Custom prompts can be saved as presets with `^s`.
- Removed the JSON `presets` configuration. **Migration:** move each prompt from the
  old object into `presets/<name>.md` inside the plugin config directory, then remove
  that object from `config.json`. File contents are the prompt; placeholders still work.
- Headless `presets` and `start` commands for agents, with JSON output, existing/new
  checkout targets, explicit prompt or preset, optional focus, and shell-only creation.
- Session results (`Started` and headless `start` JSON) include the actual checkout
  `path`, alongside workspace/tab/pane IDs, agent name and prompt-deferred status.
- Config-directory fallback outside Herdr uses `XDG_CONFIG_HOME`, then `~/.config`,
  so popup and headless commands share configuration and presets.
- `agent.start` startup “blocked during startup” / “not ready” errors are non-fatal;
  readiness checks and deferred delivery handle startup questions instead.
- `scripts/e2e.sh` exercises a disposable Herdr worktree and Claude session, with
  optional plan-prompt delivery, retained artifacts and automatic cleanup.
- Repository agent guidance in `AGENTS.md` and plugin operation guide in
  [docs/agents.md](docs/agents.md).

## 0.1.0

- Popup listing every worktree of the focused pane's repository, with herdr-bar's look:
  fuzzy search, scopes by PR state, preview column, running agents per checkout.
- PR state from one cached `gh pr list` per repository; fork-safe branch matching, with a
  fallback to the upstream branch name; OSC 8 links, click and `^b` to open.
- `^t` starts an agent session in a worktree, `^x` creates a worktree first; presets
  `blank`, `plan`, `research`, `custom` send a first prompt, delivered after startup
  questions are answered.
- `--list` prints the rows as JSON.
