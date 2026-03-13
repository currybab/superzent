---
title: Update superzent
description: How release updates work in superzent.
---

# Update superzent

## In-App Updates

Bundled macOS release builds check `releases.nangman.ai/releases` for new DMGs.

When an update is available:

- the app downloads the new bundle in the background
- the update is applied on restart
- release notes open from the same `releases.nangman.ai/releases` route and redirect to the matching GitHub release page

## When Auto-Update Does Not Run

Auto-update is not expected to run for:

- local development builds
- source builds
- non-bundled binaries

In those cases, update by rebuilding from source or installing a newer DMG manually.

## Operator Notes

The update feed is backed by GitHub Releases plus a thin Cloudflare worker. See [Releasing](./development/releasing.md) for the required infrastructure and secrets.
