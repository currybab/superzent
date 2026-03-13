<p align="center">
  <img src="assets/branding/logo_nightly.png" alt="superzent" width="180" />
  <h1 align="center">superzent</h1>
  <p align="center">A local-first workspace shell for coding agents.<br/>
  Built on <a href="https://github.com/zed-industries/zed">Zed</a>, inspired by <a href="https://github.com/superset-sh/superset">superset</a>.</p>
</p>

<p align="center">
  <img src="assets/images/superzent_screenshot.png" alt="superzent screenshot" width="800" />
</p>

One window, multiple local workspaces with git worktree, fast file navigation, diff views, and terminal-heavy agent workflows.

## Status

This repository is in early alpha.

Current focus:

- local repositories and git worktrees
- native editor, split panes, and diff views
- terminal-first use of external coding agents
- public macOS Apple Silicon releases

Deliberately out of scope for the default build:

- cloud collaboration
- calls / WebRTC
- hosted AI surfaces from upstream Zed
- Zed's own agent panel and text-thread product surface

## Roadmap

Now:

- stabilize the local-first workspace shell
- keep release, update, and docs surfaces aligned with `superzent`

Next:

- center-pane tabs for external ACP agents using selected pieces of the existing ACP / `agent_ui` stack, without reviving Zed's own agent panel
- remote project
- session restore
- next-edit integration
- native alarm

Later:

- workspace shell polish across startup, empty states, and worktree flows
- smoother terminal and agent handoff across presets, diffs, and tabs

Not planned:

- cloud collaboration and calls / WebRTC
- hosted AI surfaces in the default build
- Zed's own agent panel and text-thread product surface
- public Windows or Linux desktop releases

## Build From Source

```bash
git clone git@github.com:currybab/superzent.git
cd superzent
cargo run -p superzent
```

For day-to-day development, stay on the default lightweight shell:

```bash
cargo check -p superzent
```

Before cutting a release, run the local maintainer preflight:

```bash
./script/check-local-ci
```

Only use the inherited upstream surface when you are explicitly debugging it:

```bash
cargo check -p superzent --features full
```

For a signed macOS bundle:

```bash
./script/bundle-mac aarch64-apple-darwin
```

## Open Source Notes

- Extensions still use the upstream Zed marketplace.
- Much of the editor and platform code still comes from upstream Zed and is intentionally kept close for easier maintenance.
- The ACP roadmap is about external ACP agent tabs only. It does not mean bringing back Zed's own agent product surface.

## Project Docs

- [Getting Started](./docs/src/getting-started.md)
- [Installation](./docs/src/installation.md)
- [Development](./docs/src/development.md)
- [Contributing](./CONTRIBUTING.md)
- [Release](./docs/src/release.md)
- [Security](./SECURITY.md)

## License

This repository remains GPL-3.0-or-later, consistent with the current fork base.
