# Working on Hertrees

For **using the plugin** to discover worktrees, launch sessions, manage prompt files,
or configure it, read [docs/agents.md](docs/agents.md). This file is for changing the repo.

## Map

- `src/main.rs`, `src/cli.rs`: entry point, headless commands, terminal/editor lifecycle.
- `src/app.rs`, `src/model.rs`: picker modes, input, state, worktree/PR/agent models.
- `src/project.rs`: focused-pane context, repository discovery, worktrees, local Git facts,
  and joining agents to checkouts.
- `src/pr.rs`: GitHub lookup, branch matching, PR classification and disk cache.
- `src/session.rs`: worktree open/create, fresh sessions, readiness, deferred prompt delivery.
- `src/presets.rs`: prompt files, initial seeding, name validation and editor invocation.
- `src/config.rs`: configuration lookup and defaults.
- `src/render.rs`, `src/paint.rs`, `src/theme.rs`, `src/icons.rs`: bar-style layout,
  terminal painting, colors, agent glyphs and OSC 8 links.
- `src/term.rs`, `src/keys.rs`, `src/sequences.rs`: terminal lifecycle and input decoding.
- `src/fuzzy.rs`, `src/textutil.rs`, `src/unicode_tables.rs`, `src/py*.rs`: matching,
  display widths and Python-compatible helpers originally derived from herdr-bar (MIT, LICENSE.herdr-bar).
- `src/client.rs`, `src/host.rs`, `src/opener.rs`, `src/proc.rs`, `src/par.rs`:
  Herdr/OS/process integration; `src/testutil.rs`: test fixtures and environment isolation.
- `scripts/install.sh`: release build and atomic replacement of `bin/hertrees`.
- `scripts/e2e.sh`: disposable live Herdr/Claude integration exercise.
- `branding/`: public brand assets and usage guide; see `branding/README.md`. `branding/terminal/`
  is compiled in (`include_str!` in `render.rs`, `cli.rs`), so keep those paths.
- `tests/data/`: test fixtures. Prefer current source or live `herdr --help` over any
  saved notes when they disagree.

## Build and test

Use the mise-managed Rust toolchain:

```sh
mise exec -- cargo build --release --locked
mise exec -- cargo test --release
sh scripts/install.sh
```

The first command builds `target/release/hertrees`; the install script also updates the
binary used by the linked plugin. Unit tests live beside their implementation. Reuse
`testutil` for temporary directories and process-environment changes. Keep the bar-derived
UI conventions and its attribution rather than introducing a second input/layout system.

## Decisions and integration traps

- Discover the project from `HERDR_PLUGIN_CONTEXT_JSON.focused_pane_cwd`: a popup's own
  cwd is the plugin checkout. Herdr worktree create/open must receive `Project.repo_root`,
  not a linked checkout's path.
- For PR `Unknown` versus “no PR”, preserve the [discovery semantics](docs/agents.md#discover-checkouts)
  in the UI as well as the CLI. Match branch plus owner, with upstream-name fallback
  only when that upstream is not the default branch.
- Restore the terminal before moving Herdr focus or invoking an editor. Keep the editor
  return/re-entry loop in `cli.rs`.
- `agent.start` returning is not proof of input readiness. Preserve the `session.rs`
  gate: idle/done, no launch pending, and `interactive_ready` not false; submit through
  `agent.prompt` waiting for working. Startup “blocked during startup” / “not ready”
  errors are non-fatal; blocked/slow startup uses the detached prompt sender. Treat the
  public result as described in [session results](docs/agents.md#session-results).
- In Claude's disposable-folder trust dialog, “No, exit” is preselected: after deciding
  the test folder is safe, send `down` then `enter`, not `enter` alone.
- Herdr `pane send-keys` has no `delete` key name. To drive the popup's forward-delete
  binding, send CSI `ESC[3~` through `pane send-text` instead.
- Editor commands may include arguments: preserve the `sh -c` invocation and separately
  passed filename in `presets::open_editor`. The agent shell has `EDITOR=true`; for live
  editor tests, explicitly choose a real editor or a deterministic editing script
  (and account for `VISUAL` taking precedence).
- Use disposable repositories for mutating tests; never run them against a real project.

## Live verification

Build first, then run from a Herdr pane with authenticated Claude, `git`, `jq` and
`python3` available:

```sh
bash scripts/e2e.sh                    # blank prompt; exercises startup and cleanup
bash scripts/e2e.sh --preset plan      # also submits a real planning request to Claude
E2E_TIMEOUT=120 bash scripts/e2e.sh --preset plan --keep
```

The script checks checkout creation, tree grouping and Claude's checkout; plan mode
also checks prompt delivery. It uses isolated config and cleans up its tab, workspaces,
agent and files, then restores caller focus. `--keep` retains artifacts even on failure
and prints identifiers and cleanup instructions. `E2E_PRESET` selects the default preset;
`E2E_TIMEOUT` is seconds per wait (default 90). Run `bash scripts/e2e.sh --help` for the
current options. This script does not cover every preset-manager/editor interaction;
exercise those separately when changing them.
