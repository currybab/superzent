# superzent

<p align="center">
  <img src="assets/branding/logo_nightly.png" alt="superzent" width="180" />
</p>

`superzent` is a local-first workspace shell for coding agents.

It is built on top of a Zed fork, but the product scope is narrower: one window, multiple local workspaces with git worktree, fast file navigation, diff views, and terminal-heavy agent workflows.

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

## Public Release

The current public desktop release flow is macOS Apple Silicon only.

- Tag releases as `vX.Y.Z`
- GitHub Actions builds `superzent-aarch64.dmg`
- The release workflow also uploads Linux `remote_server` support assets
- `releases.nangman.ai/releases/...` is served by a thin Cloudflare worker that points the app at those GitHub assets

## Release Infrastructure

To publish a release with in-app updates, you need:

- GitHub Releases enabled for this repository
- Cloudflare configured for `nangman.ai` with a `releases.nangman.ai/releases*` route
- the release worker deployed from `.cloudflare/release-assets`
- optional but recommended: a Cloudflare worker secret named `GITHUB_RELEASES_TOKEN` to avoid GitHub API rate limits on update checks
- Apple signing and notarization credentials in GitHub secrets:
  - `MACOS_CERTIFICATE`
  - `MACOS_CERTIFICATE_PASSWORD`
  - `APPLE_NOTARIZATION_KEY`
  - `APPLE_NOTARIZATION_KEY_ID`
  - `APPLE_NOTARIZATION_ISSUER_ID`
- the mac signing identity in the `MACOS_SIGNING_IDENTITY` repository variable

The app prefers `SUPERZENT_*` runtime env vars for release/update overrides, but legacy `ZED_*` aliases still work during the transition.

## Open Source Notes

- Extensions still use the upstream Zed marketplace.
- Much of the editor and platform code still comes from upstream Zed and is intentionally kept close for easier maintenance.
- The ACP roadmap is about external ACP agent tabs only. It does not mean bringing back Zed's own agent product surface.

## Project Docs

- [Getting Started](./docs/src/getting-started.md)
- [Installation](./docs/src/installation.md)
- [Development](./docs/src/development.md)
- [Contributing](./CONTRIBUTING.md)
- [Security](./SECURITY.md)

## License

This repository remains GPL-3.0-or-later, consistent with the current fork base.
