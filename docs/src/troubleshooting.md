---
title: Troubleshooting
description: Common ways to debug superzent issues before filing a bug.
---

# Troubleshooting

## Version and System Specs

When filing a bug, include:

- app version from {#action zed::About}
- system specs from {#action zed::CopySystemSpecsIntoClipboard}

## Logs

Start with the app log:

- {#action zed::OpenLog} shows the recent log output
- {#action zed::RevealLogInFileManager} reveals the full file on disk

Common macOS log location:

- `~/Library/Logs/superzent/superzent.log`

If your build or bundle still writes to inherited upstream paths, include the actual path you found in the issue.

## Startup Problems

If the app refuses to start or restores into a broken state:

1. move the local database out of the app data directory
2. restart the app
3. confirm whether the issue reproduces with a fresh local state

The local state currently lives under the app support directory for the active release channel.

## Performance Problems

For performance investigations, run a release build with measurements enabled:

```sh
export ZED_MEASUREMENTS=1
cargo run -p superzent --release
```

Then compare runs with:

```sh
script/histogram version-a version-b
```

## Filing an Issue

Use the bug or crash issue templates in this repository and include:

- reproduction steps
- system specs
- relevant logs
- relevant settings or keymap overrides
