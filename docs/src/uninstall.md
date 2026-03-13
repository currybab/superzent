---
title: Uninstall
description: Remove superzent from your machine.
---

# Uninstall

## macOS

If you installed the release DMG build:

1. quit `superzent`
2. drag `/Applications/superzent.app` to the Trash
3. empty the Trash if you want the bundle removed immediately

## Optional: Remove Local Data

To remove local app data as well, delete these paths if they exist:

- `~/Library/Application Support/superzent`
- `~/Library/Caches/superzent`
- `~/Library/Logs/superzent`
- `~/.config/superzent`
- `~/.local/state/superzent`
- `~/Library/Saved Application State/ai.nangman.superzent.savedState`

If you also use dev builds, remove the matching `superzent-dev` and `ai.nangman.superzent-dev` paths as well.

## Source Builds

If you only ran `superzent` from source, removing the checkout and its build output is enough:

```sh
rm -rf target
```

Remove local app data separately if you want a clean reset.
