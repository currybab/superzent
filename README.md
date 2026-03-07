# superzet

<p align="center">
  <img src="assets/branding/logo_default.png" alt="superzet" width="180" />
</p>

`superzet` is a native workspace shell for coding agents.

It is built on top of a Zed fork, but the product direction is different: a Superset-inspired shell for managing local repositories, git worktrees, terminals, diffs, and file navigation in one app.

## Status

This repository is in early alpha.

Current focus:

- single-user, local-first workflow
- repository and worktree navigation in the left sidebar
- native editor, terminal, split panes, and diff views from Zed
- changes and file explorer surfaces in the right sidebar
- terminal-first use of CLI agents such as Claude Code and Codex

Not in scope right now:

- team features
- login or cloud collaboration
- custom hosted agent surface inside the app

## Build From Source

### macOS

```bash
git clone git@github.com:nerdface-ai/superzed.git
cd superzed
cargo run -p superzet
```

If the build complains about the Metal toolchain, install it once:

```bash
xcodebuild -downloadComponent MetalToolchain
```

### Release build

```bash
cargo build -p superzet --release
```

## Project Config

`superzet` reads optional per-project automation from `.superzet/config.json`.

```json
{
  "setup": ["./.superzet/setup.sh"],
  "teardown": ["./.superzet/teardown.sh"]
}
```

## Notes

- The app brand is `superzet`, but extension downloads continue to use the upstream Zed marketplace.
- Much of the editor, terminal, settings infrastructure, and platform integration still come from upstream Zed code and are intentionally kept close for easier maintenance.
- A lot of the upstream docs in `docs/` are still Zed-oriented and have not been fully reworked yet.

## Development Docs

- [macOS build notes](./docs/src/development/macos.md)
- [Linux build notes](./docs/src/development/linux.md)
- [Windows build notes](./docs/src/development/windows.md)

## License

This repository remains GPL-3.0-or-later, consistent with the current fork base.
