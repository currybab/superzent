---
title: Releasing superzent
description: Stable release, update feed, and operator setup for superzent.
---

# Releasing superzent

## Public Release Model

The current public release flow is:

- channel: `stable`
- platform: macOS Apple Silicon
- asset host: GitHub Releases
- update feed: `releases.nangman.ai/releases/...` via Cloudflare worker

## Tagging

Publish a release by pushing a tag shaped like:

```sh
git tag v0.1.0
git push origin v0.1.0
```

That triggers the release workflow, which:

1. builds a stable bundle with `SUPERZENT_RELEASE_CHANNEL=stable`
2. notarizes `superzent-aarch64.dmg`
3. uploads the DMG, Linux remote-server assets, and `sha256sums.txt` to the GitHub Release for that tag

## Required GitHub Configuration

Repository secrets:

- `MACOS_CERTIFICATE`
- `MACOS_CERTIFICATE_PASSWORD`
- `APPLE_NOTARIZATION_KEY`
- `APPLE_NOTARIZATION_KEY_ID`
- `APPLE_NOTARIZATION_ISSUER_ID`
- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_ACCOUNT_ID`

Repository variables:

- `MACOS_SIGNING_IDENTITY`

Optional Cloudflare worker secret:

- `GITHUB_RELEASES_TOKEN`

## Required Cloudflare Setup

Deploy the workers in `.cloudflare/` so this route exists:

- `releases.nangman.ai/releases*`

The release worker serves two behaviors:

- `/releases/{channel}/{version}/asset?...` returns JSON for the app updater
- `/releases/{channel}/{version}` redirects to the matching GitHub Release page

If you expect more than light tester traffic, set `GITHUB_RELEASES_TOKEN` on the worker so GitHub API lookups are not limited to anonymous request quotas.

## Update Feed Contract

The updater expects:

```json
{
  "version": "0.1.0",
  "url": "https://github.com/currybab/superzent/releases/download/..."
}
```

## Current Non-Goals

These are not part of the first public release flow:

- Linux or Windows desktop binary publishing
- collab deployment
- hosted AI deployment
- Sentry or other upstream release automation
