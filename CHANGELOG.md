# Changelog

## Unreleased

- `^t` and `^x` offer `yazi` after `shell` when yazi is on `PATH`: a new tab, or a new
  worktree, whose shell runs the yazi file manager in the checkout; quitting yazi leaves
  the shell. `hertrees start --agent yazi` does the same.
- Agent logos for Muse, Crush and Kimchi (Herdr Agent Icons Max 1.8.0). With a Nerd Font,
  `shell` gets a terminal mark, `yazi` its duck (in yellow), and agents the font has no
  logo for a robot, so every row of the agent picker lines up. The `shell` row no
  longer carries a description. The README's font links now point at herdr-radar v1.5.0.

## 0.2.8 (2026-10-28)

- tmux (beta): outside Herdr, Hertrees drives tmux (3.2+). `hertrees --open` opens the picker in
  a `display-popup` (bind it with `run-shell`); each worktree gets a window in the
  current session and a new tab is a pane in it. Agents get their first prompt on their
  command line (`claude`, `codex`, `cursor`, `pi`, `omp`, `opencode`, `gemini`,
  `copilot`), show as `running` while they run, and block cleanup until they exit; `^s`
  and `hertrees send` paste the prompt and press Enter. All headless commands work the
  same. `HERTREES_MUX=herdr|tmux` chooses when both are present.
- tmux is beta and lacks, compared with Herdr: plugin install and Windows support, agent
  states (agents only show `running`), a readiness gate before sending prompts, a first
  prompt for agents outside that list, spaces grouped under the main checkout, and a
  live end-to-end test.

## 0.2.7 (2026-10-28)

- Install downloads a prebuilt binary (Linux, macOS and Windows; x86_64 and aarch64) from
  the GitHub release, checked against its `SHA256SUMS`, instead of compiling; Rust is only
  needed when no binary fits or the download fails. Every `v*` tag now publishes them.
- Windows: the plugin builds and runs there. `scripts/install.ps1` is the Windows build
  step, and `hertrees.open-windows` is the action to bind. Agents are found on `PATH` by
  their `PATHEXT` extension (`claude.exe`, `omp.cmd`), PR links open in the default
  browser, the preset editor runs through `cmd` (`nvim`, else Notepad), and headless
  commands find Herdr's `%APPDATA%` and `%LOCALAPPDATA%` plugin directories.
- The `icon` wordmark draws the 16 px icon in its own truecolor palette, pixel for pixel
  (shaded cloak, cream eyes and flame, wood pole, three-tone leaf, earth), instead of a
  simplified redraw in theme roles. The brand ink is left transparent.
- The "not a git project" / "no worktrees" mark is the 32 px brand mark in its own
  colours, pixel for pixel, instead of the block outline in four theme roles.
- Removed the `pixel`, `pixel-small`, `sextant` and `off` wordmark styles; `^l` cycles
  `search`, `braille`, `icon`. A config naming a removed style gets `search`.

## 0.2.6 (2026-10-25)

- Headless clean up: `hertrees clean --path CHECKOUT` or `--merged` does what `alt+d` /
  `alt+⇧d` do, with the same refusals, and prints what it removed and what it kept and
  why as JSON. `--dry-run` reports without changing anything. A worktree whose PR state
  is unknown is kept, not treated as unmerged.

## 0.2.5 (2026-10-25)

- Spaces and tabs: each worktree's Herdr space and its tabs are listed under it, with an
  icon per level (Nerd Font glyphs when installed, else `⎇ ◫ ▭`; `icons` in
  `config.json`). `→`/`←` show or fold them; `⏎` on a tab focuses it and its agent.
- Name a new worktree's space with `⇥` after its branch; `alt+r` renames the selected
  space or tab. Headless: `hertrees start --branch … --label NAME`.
- `^t` offers a shell as well as agents: a new tab with no agent. Headless:
  `hertrees start --path … --agent shell`.
- `^s` on a tab row sends to that tab's agent only.
- Clean up: `alt+d` closes a worktree's space, removes its checkout and, when GitHub
  merged the branch at its current commit, deletes the branch; `alt+⇧d` does this for
  every merged worktree. Refused for the main or current checkout, uncommitted changes
  and working agents. On a space or tab row, `alt+d` closes just that.
- The preview shows the branch against the default branch (`origin/main` when fetched):
  commits ahead and behind, and the changed files with their added and removed lines.
- `?` lists the keys for the list, the agent picker, the preset manager or the branch
  input. The list footer is shorter and points to it.

## 0.2.4 (2026-10-25)

- Add your own text to a preset: `^o` in the agent picker opens the prompt editor with
  the preset shown dimmed above it; `⏎` sends the preset, a blank line, then your text.
  A preset with `{input}` takes your text there instead.
- The prompt editor is multi-line (`^j` or `alt+⏎`; pasted line breaks are kept),
  completes placeholders with `⇥`, and recalls earlier prompts with `↑`/`↓`
  (`prompt-history.json` in the plugin state directory).
- Presets can start with a settings block: `ask: true` makes `⏎` open the editor,
  `agent:` picks that agent first, `description:` shows in the preset manager.
- Repository presets in `.hertrees/presets/`, shared through git, marked `repo`; your own
  preset wins a name clash.
- New placeholders: `{pr_title}`, `{pr_body}`, `{commits}`, `{diffstat}` and `{input}`.
  Any other `{name}` is a field the popup asks you to fill in before sending.
- `^s` sends a preset or prompt to the agent already running in a worktree; a busy agent
  gets it once it is idle. Headless: `hertrees send --agent NAME …`.
- `hertrees start` accepts `--preset` with `--prompt` (combined as in the editor) and
  `--var NAME=VALUE`; `hertrees presets` also lists the repository's presets.

## 0.2.3 (2026-10-25)

- The search line carries the wordmark: `❯ hertrees │ search worktrees…`, "her" in the
  text colour and "trees" in the idle (leaf) colour, so colour overrides apply. Popups
  narrower than 60 columns keep the plain prompt.
- New `wordmark` config key picks the name's style: `search` (the default above),
  `pixel`, `pixel-small`, `braille`, `sextant` or `icon` (brand pixel lettering, or the
  icon with name, tagline and counts, drawn in the popup's first rows above a plain search
  line), or `off`. Pixel styles fall back to `search` when the popup is too narrow for the
  art or the list would keep fewer than 5 rows. `sextant` needs a font with Unicode 13
  sextants. Herdr 0.9.3 has no way to set a plugin pane's border title, so there are no
  border-title styles.
- `^l` in the popup steps through the wordmark styles, drawing each at once and saving
  it as `wordmark` in `config.json` (other keys kept). The footer notes when the popup
  is too small to show the chosen style.

## 0.2.2 (2026-10-25)

- The PR cache and deferred prompt files live in Herdr's plugin state directory
  (`HERDR_PLUGIN_STATE_DIR`, else `$XDG_STATE_HOME/herdr/plugins/hertrees`) instead of
  `~/.cache/hertrees` and the system temp directory. The old cache can be deleted.
- After a failed session start the popup reloads its worktrees, so a checkout that was
  created before the failure is listed instead of having its agent shown under main.
- README: key binding precedence, a download link and install commands for the optional
  icon font, where `bin/` is.
- CI: a preset-listing test no longer relies on a case-sensitive file system (macOS).

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
