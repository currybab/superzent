---
title: Release Notes
description: How to write PR release notes for superzent.
---

# Release Notes

Every pull request should keep the `Release Notes:` section from the PR template:

```md
Release Notes:

- N/A
```

or:

```md
Release Notes:

- Added ...
- Fixed ...
- Improved ...
```

## Why We Still Require Them

The current preview release workflow publishes GitHub-generated release notes, not a custom notes compiler.

We still require PR release notes because they make code review easier and give us a clean source for future curated release summaries.

## Guidelines

- Use `N/A` for docs-only or internal-only changes
- Mention user-facing settings and keybindings explicitly
- Write in product language, not internal implementation language
- If a change is visible in the preview build, it should usually have a note
