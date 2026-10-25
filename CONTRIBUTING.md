# Contributing

Bug reports and pull requests are welcome.

- Code map, build and test commands, and integration traps are in [AGENTS.md](AGENTS.md); read it before changing session, prompt or PR logic.
- Build and test: `cargo build --release --locked && cargo test --release`.
- Live check against a disposable repository: `bash scripts/e2e.sh` (needs Herdr, `git`, `jq`, `python3`, authenticated Claude).
- Keep changes focused, add or update tests beside the code, and note user-visible changes in [CHANGELOG.md](CHANGELOG.md).
- Brand assets live in [branding/](branding); follow its usage rules.
