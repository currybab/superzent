---
title: Developing superzent
description: Build, run, and release superzent from source.
---

# Developing superzent

See the platform-specific setup docs for build prerequisites:

- [macOS](./development/macos.md)
- [Linux](./development/linux.md)
- [Windows](./development/windows.md)

## Core Commands

Run the app:

```sh
cargo run -p superzent
```

Day-to-day development should stay on the default lightweight build:

```sh
cargo check -p superzent
```

Run clippy:

```sh
./script/clippy
```

Build the heavier upstream-like surface only when you actually need it:

```sh
cargo check -p superzent --features full
```

## Release Flavors

The default app build is `lite`.

That excludes:

- collab
- calls / WebRTC
- inherited agent panel and text-thread UI
- hosted AI and edit prediction stacks

`--features full` is still available for debugging inherited upstream behavior.

## Validation Paths

For day-to-day work:

```sh
cargo check -p superzent
./script/clippy
```

Before cutting a release:

```sh
./script/check-local-ci
```

## Keychain Access

Development builds use a less intrusive credential path so you do not get repeated system keychain prompts while iterating locally.

If you need to test real keychain access in a development build:

```sh
ZED_DEVELOPMENT_USE_KEYCHAIN=1 cargo run -p superzent
```

## Performance Measurements

You can still use the inherited frame measurement tooling:

```sh
export ZED_MEASUREMENTS=1
cargo run -p superzent --release
```

Then compare runs with:

```sh
script/histogram version-a version-b
```

## Release and Docs Workflow

- [Releasing](./development/releasing.md)
- [Upstream Sync](./development/upstream-sync.md)
- [Release Notes](./development/release-notes.md)
- [Debugging Crashes](./development/debugging-crashes.md)
- [Contributing](https://github.com/currybab/superzent/blob/main/CONTRIBUTING.md)
- [Security Policy](https://github.com/currybab/superzent/blob/main/SECURITY.md)
