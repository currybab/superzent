<p align="center">
  <img src="assets/branding/logo_nightly.png" alt="superzent" width="180" />
  <h1 align="center">superzent</h1>
  <p align="center">Superzent is a fork of the <a href="https://github.com/zed-industries/zed">Zed</a> editor inspired by <a href="https://github.com/superset-sh/superset">superset.sh</a>,<br/> designed to make AI workflows a first-class part of the development environment.</p>
</p>

<p align="center">
  <img src="assets/videos/demo.gif" alt="superzent demo" width="800" />
</p>

One window, multiple local workspaces with git worktree, fast file navigation, diff views, terminal-heavy agent workflows, and center-pane ACP chat tabs.

## Why superzent

- Compared with upstream Zed: more opinionated around managing multiple local projects and workspaces in one window, especially for git-worktree-heavy flows, with external ACP chats treated as a first-class center-pane workflow.
- Compared with `superset.sh`: keeps a native editor in the loop, with language-server-backed navigation, diagnostics, and quick in-place edits alongside terminal agent workflows.
- All telemetry is off. Metrics and crash diagnostics are disabled by default and there is no collection endpoint configured.

## Keyboard Shortcuts

Shortcuts that differ from upstream Zed:

| Shortcut | Action |
|----------|--------|
| `Cmd+T` | Open terminal in center pane |
| `Cmd+Shift+C` | Copy project-relative path |
| `Ctrl+\` | Next workspace |
| `Ctrl+\|` | Previous workspace |

`Cmd+W` never closes the OS window — quit with `Cmd+Q` instead.

## Status

This repository is in early alpha.

Current focus:

- local repositories and git worktrees
- native editor, split panes, and diff views
- terminal-first use of external coding agents
- center-pane ACP tabs built from selected pieces of the existing ACP / `agent_ui` stack
- default-build next-edit with non-Zed-hosted providers
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

The default build includes `acp_tabs` and next-edit, so external ACP agents open in center-pane tabs and regular editor buffers can use non-Zed-hosted edit prediction without enabling the heavier upstream AI surface.

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
- This repository is regularly synced with upstream Zed, so GitHub contributor counts and graphs may include upstream contributors alongside superzent-specific work.
- The default app build is `lite + acp_tabs + next_edit`.
- `superzent` reuses selected ACP / `agent_ui` pieces to open external ACP chats in center-pane tabs.
- The default build also restores an edit prediction footer entry for non-Zed-hosted providers in regular editor buffers.
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
