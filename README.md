<p align="center">
  <img src="assets/branding/logo_nightly.png" alt="superzent" width="180" />
  <h1 align="center">superzent</h1>
  <p align="center">Superzent is a fork of the <a href="https://github.com/zed-industries/zed">Zed</a> editor inspired by <a href="https://github.com/superset-sh/superset">superset.sh</a>,<br/> designed to make AI workflows a first-class part of the development environment.</p>
</p>

<p align="center">
  <img src="assets/images/superzent_screenshot.png" alt="superzent screenshot" width="800" />
</p>

One window, multiple local workspaces with git worktree, fast file navigation, diff views, terminal-heavy agent workflows, and center-pane ACP chat tabs.

## Status

This repository is in early alpha.

Current focus:

- local repositories and git worktrees
- native editor, split panes, and diff views
- terminal-first use of external coding agents
- center-pane ACP tabs built from selected pieces of the existing ACP / `agent_ui` stack
- public macOS Apple Silicon releases

Deliberately out of scope for the default build:

- cloud collaboration
- calls / WebRTC
- hosted AI surfaces from upstream Zed
- Zed's docked agent panel and native text-thread product surface

## Roadmap

Now:

- stabilize the local-first workspace shell
- polish center-pane ACP tabs, history, and preset handoff
- keep release, update, and docs surfaces aligned with `superzent`

Next:

- remote project fix
- session restore
- next-edit integration
- native alarm

Later:

- workspace shell polish across startup, empty states, and worktree flows
- smoother terminal and agent handoff across presets, diffs, and tabs

Not planned:

- cloud collaboration and calls / WebRTC
- hosted AI surfaces in the default build
- Zed's own docked agent panel and native text-thread product surface
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

The default build includes `acp_tabs`, so external ACP agents open in center-pane tabs without enabling the heavier upstream AI surface.

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
- The default app build is `lite + acp_tabs`.
- `superzent` reuses selected ACP / `agent_ui` pieces to open external ACP chats in center-pane tabs.
- That does not mean bringing back Zed's own docked agent panel, native text-thread surface, or hosted AI product flow in the default build.

## Project Docs

- [Getting Started](./docs/src/getting-started.md)
- [Installation](./docs/src/installation.md)
- [Development](./docs/src/development.md)
- [Contributing](./CONTRIBUTING.md)
- [Release](./docs/src/release.md)
- [Security](./SECURITY.md)

## License

This repository remains GPL-3.0-or-later, consistent with the current fork base.
