---
title: superzent on macOS
description: Install and run the current public preview of superzent on macOS.
---

# superzent on macOS

macOS is the first public release target for `superzent`.

## Installing the Preview Build

Download the latest preview DMG from:

- [GitHub Releases](https://github.com/currybab/superzent/releases)

Then:

1. open `superzent-aarch64.dmg`
2. drag `superzent.app` into `/Applications`
3. launch the app from Applications

## Building From Source

For local development:

```sh
cargo run -p superzent
```

For a bundled macOS build:

```sh
./script/bundle-mac aarch64-apple-darwin
```

## Current Support Level

- public preview binary: Apple Silicon
- source builds: inherited upstream development paths still exist for Intel and other platforms, but they are not the focus of the public release flow

## Updates

Bundled preview builds can update through the `releases.nangman.ai/releases` feed.

Development builds and source builds should be updated manually.

## Troubleshooting

### Gatekeeper or quarantine warnings

If macOS blocks the app after download, remove the quarantine attribute:

```sh
xattr -cr /Applications/superzent.app
```

### Log file

Use the command palette to open the log, or inspect:

```sh
~/Library/Logs/superzent/superzent.log
```
