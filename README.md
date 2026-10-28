<p align="center">
  <picture>
    <source type="image/svg+xml" srcset="branding/banner/banner.svg">
    <img src="branding/banner/banner@2x.png" alt="Hertrees: every worktree, every agent, one popup." width="860">
  </picture>
</p>

# Hertrees

A [Herdr](https://herdr.dev) plugin that lists every git worktree of your project in one
popup, with its pull request state and running agents, and starts a new agent session in
any of them or in a brand-new worktree. It also runs under plain [tmux](#tmux-beta) (beta).

<p align="center"><img src="docs/media/demo.gif" alt="Hertrees popup: cycle scopes with Tab, press ^x for a new worktree, pick from 24 supported agents, choose a preset or write and save a custom prompt" width="860"></p>

- **Any project:** shows the repository of the pane you opened it from, even inside a linked worktree.
- **PR state per worktree:** `✓ merged`, `◉ open`, `◐ draft`, `● closed`, `no PR`, or `○ PR ?` when it could not check. `#N` is a clickable link.
- **Agents:** the left glyph is Herdr's status for the agents in that checkout (`◉` needs you, `◐` working, `●` done, `✓` idle).
- **Spaces and tabs:** each worktree's Herdr space and its tabs sit under it in the list (`⎇` worktree, `◫` space, `▭` tab; Nerd Font glyphs when you have one). Name the space when you create the worktree, rename spaces and tabs with `alt+r`, open a tab with `⏎`.
- **New sessions:** start an agent in a worktree, or create a worktree and start one, with a preset first prompt (`blank`, `plan`, `research`, your own, or `custom`). `^t` opens a new tab with an agent, a plain shell, or [yazi](https://yazi-rs.github.io) to browse its files when it is installed.
- **Send to an agent:** `^s` on a tab sends a preset or your own prompt straight to the agent in it.
- **Clean up:** `alt+d` removes a finished worktree: its space, its checkout, and its branch once GitHub merged it. `alt+⇧d` cleans up every merged one. Nothing with uncommitted changes or a working agent is touched. Agents and scripts use `hertrees clean`.
- **Local facts:** dirty files `±N`, ahead/behind `↑↓`, last commit age, recent commits, and in the preview what the branch changed against `main`: commits both ways and the files with their `+`/`−` lines.

## Quick start

Needs Herdr 0.9.3+ on Linux, macOS or Windows, `git`, and an authenticated `gh` for PR
state (optional). Install downloads the prebuilt binary for your machine (x86_64 or
aarch64) from the GitHub release; without one it builds from source, which needs Rust
1.88+ ([rustup](https://rustup.rs); on Windows with the MSVC toolchain).

1. Install:

   ```bash
   herdr plugin install janooh37-hue/hertrees
   ```

2. Bind a key (Herdr adds none) in `~/.config/herdr/config.toml`
   (`%APPDATA%\herdr\config.toml` on Windows):

   ```toml
   [[keys.command]]
   key = "prefix+w"
   type = "plugin_action"
   command = "hertrees.open"
   description = "worktrees"
   ```

   `prefix+w` is Herdr's workspace picker by default. This binding takes precedence, but
   to free the key explicitly, add `workspace_picker = ""` to your `[keys]` table (create
   it if the file has none). Then run `herdr server reload-config`.

   On Windows the action is `hertrees.open-windows`: Herdr allows one id per action, and
   the `open` one runs through `sh`.

3. Press `prefix+w` in any pane of a git project.

## tmux (beta)

tmux support is in beta. Outside Herdr, Hertrees drives tmux 3.2+ instead (Linux and macOS). Get the binary with
`sh scripts/install.sh` in a clone (or `cargo build --release`), then bind a key in
`~/.tmux.conf`:

```tmux
bind-key w run-shell '/path/to/hertrees/bin/hertrees --open'
```

`prefix w` opens the popup on the project of the focused pane. Each worktree gets a tmux
window in the current session, named after its branch (or the name you give it); a new
tab is a new pane in that window. `⏎` selects the window, switching session if needed;
windows Hertrees did not open are left alone. What tmux lacks compared with Herdr:

- No plugin install or Herdr action: you place the binary and bind the key yourself.
  Linux and macOS only; there is no tmux support on Windows.
- tmux reports no agent states: no `◉` needs you, `◐` working, `●` done or `✓` idle.
  Agents show as `running` until they exit, and a running
  agent always blocks cleaning up its worktree.
- The first prompt is passed on the agent's command line, so it arrives even behind a
  startup question (Claude's folder trust). Agents that take one: `claude`, `codex`,
  `cursor`, `pi`, `omp`, `opencode`, `gemini`, `copilot`; the others start blank.
- `^s` pastes the prompt into the agent's pane and presses Enter at once, whether or not
  the agent is busy.
  Herdr waits until the agent can take input; tmux has no such readiness gate.
- No grouped spaces: worktrees are flat windows in the current session rather than
  spaces grouped under the main checkout, and there is no live end-to-end test yet.

Hertrees uses Herdr whenever it runs in a Herdr pane or popup. When tmux and Herdr are
nested, set `HERTREES_MUX=tmux` (or `herdr`) to choose.

## Keys

| Key | Action |
|---|---|
| type | fuzzy search: branch, path, `#123`, PR title, PR state, agent |
| `↑↓` `^p` `^n` `^k` `^j` `PgUp` `PgDn` | move |
| `⏎` / click selected row | focus the worktree's space (opening it if needed), or the selected tab and its agent |
| `^t` | new tab in the worktree's space: an agent with a preset, a shell, or yazi (when installed) |
| `^s` | send a preset or prompt to a running agent; on a tab row, to that tab's agent |
| `^x` | new worktree: branch name (`⇥` names its space) → agent (or shell, or yazi) → preset |
| `→` `←` (empty query) | show or fold a worktree's space and tabs |
| `alt+r` | rename the selected space or tab |
| `alt+d` | clean up the selected worktree; on a space or tab row, close it |
| `alt+⇧d` | clean up every merged worktree that is safe to remove |
| `?` | the keys for where you are |
| `^b` / click `#N` | open the PR in the browser |
| `⇥` `⇧⇥` | scope: everything → no PR → open → draft → merged → closed → agents |
| `@` (empty query) | agents scope |
| `^r` / `^o` | reload worktrees, agents and PRs / toggle the preview column |
| `^l` | next wordmark style, shown at once and saved to `config.json` |
| `^u` `^w` `^a` `^e` `^d` | clear, delete word, home, end, delete forward |
| `esc` | back / close |

Cleaning up asks first, then closes the worktree's space, removes the checkout and, only
when GitHub merged the branch at exactly its current commit, deletes the local branch.
It refuses the main checkout, the checkout you opened the popup from, one with
uncommitted changes, and one where an agent is still working; Herdr and git refuse
uncommitted changes again on their own. Tab names come from the bar; `alt+r` sets your own.

In the agent picker, `⇥` cycles the preset, `⏎` sends it as is, `^o` opens the prompt
editor to add your own text under it, and `^e` manages presets (`⏎` edits, `^x` creates,
`^r` renames, `⌦`/`^d` deletes).

The prompt editor shows the preset dimmed above your text. `⏎` sends, `^j` or `alt+⏎`
starts a new line, `⇥` completes a `{placeholder}` (type `{bra` then `⇥`, or `⇥` alone to
pick from the list), `↑`/`↓` bring back earlier prompts and `^s` saves the result as a
preset. If a prompt uses a placeholder of its own, such as `{ticket}`, the popup asks for
its value before sending.

### Presets

Presets are Markdown files in `<config_dir>/presets/` (`plan.md` and `research.md` are
seeded on first use). A repository can share its own in `.hertrees/presets/`; yours win
on a name clash. An optional block at the top sets how a preset behaves:

```markdown
---
ask: true            # ⏎ opens the editor so you add text every time
agent: codex         # the agent chosen first with this preset
description: house rules, then your task
---
Follow the conventions in AGENTS.md. Run the tests before you finish.
```

Put `{input}` in a preset to place your text there instead of below it, e.g.
`Task: {input}` followed by rules. Placeholders: `{branch}` `{base}` `{path}` `{repo}`
`{pr}` `{pr_title}` `{pr_body}` `{commits}` `{diffstat}` `{input}`, plus any `{name}`
of your own, which the popup asks you to fill in.

## Configuration

Everything is optional. Edit `~/.config/herdr/plugins/config/hertrees/config.json`:

```json
{
  "agents": ["claude", "codex", "omp"],
  "default_agent": "claude",
  "scope": "all",
  "preview": "auto",
  "colors": { "pr_merged": "bright_green", "pr_draft": "#c78a1f" }
}
```

- `agents`: agent kinds offered, in order. Default: every supported agent found on `PATH`.
- `wordmark`: `"search"` (default, `❯ hertrees │` on the search line), `"braille"` (lettering
  above it) or `"icon"` (icon, name, tagline and counts above it).
  `^l` in the popup steps through them and saves the choice here.
- `icons`: glyphs before worktree, space and tab rows. `"auto"` (default) uses Nerd Font
  glyphs when a Nerd Font is installed and `⎇ ◫ ▭` otherwise; `"nerd"` or `"unicode"`
  forces a set, `"off"` draws none. Set it when the popup runs on another machine than
  your terminal (SSH), where detection sees the server's fonts.
- Presets: see [Presets](#presets). The preset manager opens `$VISUAL`, then `$EDITOR`,
  then `nvim` or `vi`.
- Fonts and icons: everything is plain Unicode by default. For vendor logos before agent
  rows, install the optional "Herdr Agent Icons Max" font from
  [herdr-radar](https://github.com/hhdebb/herdr-radar) (not bundled here because its marks
  are third-party trademarks):
  [**download HerdrAgentIconsMax-Regular.ttf**](https://raw.githubusercontent.com/hhdebb/herdr-radar/main/dist/HerdrAgentIconsMax-Regular.ttf).

  ```bash
  # Linux
  mkdir -p ~/.local/share/fonts && curl -fLo ~/.local/share/fonts/HerdrAgentIconsMax-Regular.ttf \
    https://raw.githubusercontent.com/hhdebb/herdr-radar/main/dist/HerdrAgentIconsMax-Regular.ttf && fc-cache -f
  # macOS
  curl -fLo ~/Library/Fonts/HerdrAgentIconsMax-Regular.ttf \
    https://raw.githubusercontent.com/hhdebb/herdr-radar/main/dist/HerdrAgentIconsMax-Regular.ttf
  ```

  Hertrees detects it in your user or system font folder. Set `"agent_icons": "font"` when
  the font is only installed on the machine running your terminal. When Herdr runs on
  another machine (SSH, e.g. Termius on iOS), the font is detected on that server, so
  set `"agent_icons": "off"` if logos show as boxes.

All keys, defaults and colour roles are in [docs/agents.md](docs/agents.md#configuration).

## Command line

```bash
bin/hertrees --list --cwd /path/to/repo   # worktrees, PR state and agents as JSON
bin/hertrees start --cwd /path/to/repo --branch feat/cache --agent claude --preset plan
bin/hertrees start --cwd /path/to/repo --path /path/to/repo --agent claude \
  --preset rules --prompt 'fix the login bug' --var ticket=HT-42
bin/hertrees send --cwd /path/to/repo --agent claude-main --prompt 'run the tests'
bin/hertrees clean --cwd /path/to/repo --merged --dry-run   # what alt+⇧d would remove
bin/hertrees --help                       # all flags
bin/hertrees --version                    # version, under the pixel mark on a terminal
```

`bin/` is inside the installed plugin checkout (`plugin_root` in `herdr plugin list --json`).
Scripts and agents can start, prompt and clean up sessions headlessly; see
[Using Hertrees from an agent](docs/agents.md).

## Development

```bash
mise exec -- cargo test --release
sh scripts/install.sh                     # bin/hertrees: release binary, or a build of local changes
bash scripts/e2e.sh --help                # disposable live Herdr/Claude test
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

## Credits

Made by Ahmed Al Ali. The logo, banner, palette and terminal mark are in
[`branding/`](branding), with a guide to using them. The terminal UI layer
(painter, input decoder, theme, fuzzy matcher, row layout) started from
[herdr-bar](https://github.com/jeffarese/herdr-bar) by Jeff Arese (MIT,
`LICENSE.herdr-bar`). Hertrees is now an independent project.

## License

MIT, see [LICENSE](LICENSE). Code derived from herdr-bar is under its own MIT licence, see
[LICENSE.herdr-bar](LICENSE.herdr-bar).

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: light)" srcset="branding/light/lockup-light.svg">
    <img src="branding/logo/lockup/lockup.svg" alt="Hertrees" width="240">
  </picture>
</p>
