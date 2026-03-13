---
title: Syncing ACP and Agent UI from upstream
description: How superzent selectively pulls ACP and native agent UI changes from upstream Zed.
---

# Syncing ACP and Agent UI from upstream

`superzent` keeps a narrow upstream intake lane for ACP and native agent UI work from Zed. The goal is to reuse upstream improvements in that area without reopening the full upstream product surface in the default build.

## Current Watchlist

The first watchlist is intentionally small:

- `crates/agent_ui/**`
- `crates/agent/**`
- `crates/acp_thread/**`
- `crates/agent_servers/**`
- `crates/agent_settings/**`
- `crates/zed/src/main.rs`
- `crates/zed/src/zed.rs`

## Tracking Markers

Update these markers as part of each import pass:

- Last reviewed upstream tip: `0a436bec1758`
- Last imported upstream base: `8a38d2d7b465bc5024bba37a21f68958926e2eb9`

## Find Candidate Commits

Use the helper script to list commits on `upstream/main` that touch the watchlist.

```sh
script/upstream-agent-ui-candidates --since 8a38d2d7b465bc5024bba37a21f68958926e2eb9
```

For a markdown report you can paste into an issue or notes file:

```sh
script/upstream-agent-ui-candidates \
  --since 8a38d2d7b465bc5024bba37a21f68958926e2eb9 \
  --format markdown
```

If you have already fetched `upstream/main`, skip the fetch step:

```sh
script/upstream-agent-ui-candidates \
  --since 8a38d2d7b465bc5024bba37a21f68958926e2eb9 \
  --no-fetch
```

## Intake Loop

1. Fetch and review candidate commits from `upstream/main`.
2. Classify each commit as `take now`, `defer`, or `ignore`.
3. Create a feature branch from `main`.
4. Cherry-pick the selected commits manually with plain `git cherry-pick`.
5. Run validation for the imported surface.
6. Update the tracking markers in this document.

## Validation Checklist

At minimum, run:

```sh
cargo check -p superzent
cargo check -p superzent --features full
```

Then run targeted tests or checks for any touched crates, especially when the import changes:

- `agent_ui`
- `agent`
- `acp_thread`
- `agent_servers`
- `agent_settings`

## Compatibility Checks

Review every import against these `superzent` constraints:

- The default app build is still `lite`.
- `superzent` does not ship hosted AI, cloud, or collab assumptions in the default flow.
- `AgentPanel` is still a dock panel upstream today, while `superzent`'s next product step is center-pane ACP agent tabs.
- Imported changes must not reintroduce upstream branding, docs links, or release assumptions.

## Backports vs. Upstream Intake

The existing `script/cherry-pick` helper and `.github/workflows/cherry_pick.yml` are for release branch backports inside `superzent`.

Do not use that workflow as the primary upstream intake mechanism. Upstream Zed imports should stay on normal feature branches so you can review conflicts, trim unwanted assumptions, and validate the result before merging.
