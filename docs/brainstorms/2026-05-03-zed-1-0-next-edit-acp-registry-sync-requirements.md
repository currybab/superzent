---
date: 2026-05-03
topic: zed-1-0-next-edit-acp-registry-sync
---

# Zed 1.0 Next-Edit And ACP Registry Sync

## Summary

Backport Zed 1.0-era next-edit and ACP registry/external-agent polish that improves Superzent's default build without importing upstream hosted AI, telemetry, or docked agent product surfaces.

---

## Problem Frame

Zed 1.0 makes AI-native editing and ACP-based agents central to the editor experience. Superzent shares part of that direction, but its product shape is narrower: local-first multi-workspace editing, terminal-heavy external agent workflows, center-pane ACP chat tabs, and non-Zed-hosted next-edit in the default build.

The previous upstream sync phase focused on editor, search, terminal, and picker improvements while explicitly deferring broad upstream agent/sidebar/chat architecture. The next sync should continue that selectivity. The useful work now is not to copy Zed's Parallel Agents or Threads Sidebar shape, but to keep Superzent's existing next-edit and external ACP-agent surfaces reliable as Zed 1.0-era fixes land upstream.

---

## Actors

- A1. Superzent user: Uses the default build for local repositories, worktrees, center-pane ACP tabs, and next-edit.
- A2. External ACP agent: Runs through ACP registry or agent-server configuration and is expected to work inside Superzent's ACP tab flow.
- A3. Sync implementer: Selects upstream commits, adapts them to Superzent's feature split, and verifies default-build behavior.

---

## Key Flows

- F1. Candidate selection
  - **Trigger:** A sync pass evaluates Zed 1.0-era upstream changes.
  - **Actors:** A3
  - **Steps:** Review next-edit/provider and ACP registry/external-agent commits; classify each candidate as must, maybe, or skip; reject candidates that require upstream hosted AI or docked agent surfaces.
  - **Outcome:** Planning receives a bounded candidate list rather than a broad upstream merge target.
  - **Covered by:** R1, R2, R3, R4, R15

- F2. Default-build next-edit validation
  - **Trigger:** A next-edit/provider candidate is selected.
  - **Actors:** A1, A3
  - **Steps:** Apply the candidate; verify allowed providers remain discoverable and usable in the default build; confirm unsupported Zed-hosted provider paths stay hidden or treated as unconfigured.
  - **Outcome:** Superzent keeps next-edit parity without expanding into hosted AI.
  - **Covered by:** R5, R6, R7, R8, R9

- F3. ACP registry/external-agent validation
  - **Trigger:** An ACP registry or external-agent candidate is selected.
  - **Actors:** A1, A2, A3
  - **Steps:** Apply the candidate; verify registry-installed or configured external agents can start or resume through Superzent's center-pane ACP tab flow; confirm debug and configuration surfaces help local diagnosis without requiring upstream Agent Panel restoration.
  - **Outcome:** External ACP agents become more reliable while preserving Superzent's center-pane product model.
  - **Covered by:** R10, R11, R12, R13, R14

---

## Requirements

**Selection Strategy**

- R1. This sync phase must use upstream commit or PR-level cherry-picks rather than merging all of `upstream/main`, `v1.0.0`, or `v1.0.x`.
- R2. Candidate selection must be limited to default-build next-edit parity and ACP registry/external-agent polish.
- R3. Any candidate touching AI, ACP, providers, or agent UI must be reviewed against Superzent's default `lite + acp_tabs + next_edit` build and the heavier `full` feature split before inclusion.
- R4. Selected changes must preserve Superzent's center-pane ACP tab model and must not require restoring Zed's docked Agent Panel, native text-thread product surface, or Threads Sidebar.

**Next-Edit Parity**

- R5. The default build must continue to support Superzent's existing non-Zed-hosted next-edit provider scope.
- R6. Provider setup and status entry points for allowed providers should remain discoverable in the default build.
- R7. Correctness fixes for prediction display, cursor movement, provider setup entry points, and prediction menu behavior should be prioritized when they do not depend on Zed-hosted provider behavior.
- R8. Provider protocol or model-support updates may be included only when they improve allowed provider behavior without exposing Zed-hosted provider choices in the default build.
- R9. Existing or stale Zed-hosted provider settings must continue to recover through the unsupported/unconfigured path rather than activating a hosted provider in the default build.

**ACP Registry And External Agents**

- R10. ACP registry install, update, debug, and configuration flows should be evaluated only insofar as they support external agents in Superzent's default build.
- R11. Registry-installed or configured external agents should open, resume, and report failures through Superzent's center-pane ACP tab flow.
- R12. External agent working-directory, environment, and configuration behavior should remain scoped to the user's active project/workspace context.
- R13. ACP debug and logging improvements should be included when they help diagnose local external-agent failures without adding telemetry or hosted-service dependence.
- R14. External-agent session history, import, or resume fixes may be selected only when they fit Superzent's ACP tab/history direction and do not require the upstream Threads Sidebar as the primary surface.
- R15. Candidates that mainly support upstream Parallel Agents, hosted Zed Agent, Zeta telemetry, Business/team management, collaboration, calls, or DeltaDB should be classified as skip for this phase.

---

## Initial Candidate Shortlist

**Must Evaluate: Next-Edit**

- `2460a5c5df` ep: Fix moving cursor to a predicted position (#55079)
- `a7ca17fcb2` edit_prediction_ui: Add configure providers menu item to Copilot and Codestral (#53691)
- `7a6a95c2cd` editor: Fix edit predictions polluting completions menu (#50403)
- `8b822f9e10` Fix regression preventing new predictions from being previewed in subtle mode (#51887)
- `db7bc734e2` Fix ep preview closing menus (#54194)

**Must Evaluate: ACP Registry / External Agents**

- `2ca94a6032` acp: Register ACP sessions before load replay (#54431)
- `102805a73f` agent_ui: Preserve session resume state after load errors (#54411)
- `a5e78b02de` Fix double borrow panic when ACP process dies (#54135)
- `f7ab907216` Fix agent servers loading environment from home dir instead of project dir (#52763)
- `73127da4b7` acp_tools: Always capture ACP transport and stderr into the log ring (#54536)

**Maybe Evaluate**

- `6e900b43ec` agent_ui: Handle pagination of session/list correctly when importing (#54427)
- `c91b917383` agent_ui: Sort thread import agents by display name (#54417)
- `1c1b03c3d6` acp: Improve ACP debug view (#54769)
- `07e9a2d25a` acp: Use npm --prefix for registry npx agents (#53560)
- `d1177dc43` acp: Support terminal auth methods (#51999)
- `8f0826f543` acp: Set agent server cwd from project paths (#52005)
- `3ee2f5b811` acp: Add agent websites to the registry page (#52002)
- `809e701163` acp: Notify when we receive new versions from the registry (#52818)
- `57e01b3701` language_models: Fix misleading copy when hosted models are disabled (#53971)
- `f051447a8d` open_ai: Use responses API for all models (#54910)

**Skip By Default**

- Zed Agent-native changes whose value depends on hosted Zed Agent behavior.
- Zeta model, training, telemetry, or Agent Metrics changes.
- Parallel Agents, Threads Sidebar, docked Agent Panel, and native text-thread product-shape changes.
- Collaboration, calls, Business/team management, and DeltaDB-oriented changes.

---

## Acceptance Examples

- AE1. **Covers R5, R6, R7, R9.** Given a Superzent default-build user with a supported non-Zed-hosted next-edit provider configured, when next-edit parity fixes are applied, inline predictions and setup/status entry points continue to work without exposing Zed-hosted provider choices.
- AE2. **Covers R10, R11, R13.** Given a registry-installed external ACP agent that fails to start, when the user opens the relevant debug surface, the failure has enough local transport or stderr context to diagnose it without relying on hosted telemetry.
- AE3. **Covers R4, R14, R15.** Given an upstream candidate whose user value depends on the Threads Sidebar or docked Agent Panel, when it is reviewed for this phase, it is skipped or deferred even if the underlying fix is useful to Zed.
- AE4. **Covers R1, R3.** Given a candidate that touches shared provider or ACP code, when planning evaluates it, the plan explicitly checks default-build behavior before treating the cherry-pick as accepted.

---

## Success Criteria

- The next sync plan has a small, defensible must/maybe/skip list for Zed 1.0-era next-edit and ACP registry work.
- The default Superzent build preserves non-Zed-hosted next-edit and center-pane ACP tabs after selected candidates are applied.
- Users get reliability, setup, and debugging improvements without seeing new Zed-hosted AI, telemetry, Agent Metrics, docked Agent Panel, or Threads Sidebar surfaces.
- Planning can proceed without inventing product scope, inclusion criteria, or explicit exclusions for this sync phase.

---

## Scope Boundaries

- Do not merge broad upstream AI, ACP, agent UI, or sidebar architecture.
- Do not restore Zed's docked Agent Panel or native text-thread product surface in the default build.
- Do not expose Zed-hosted next-edit providers, Zeta defaults, Zeta telemetry, or Agent Metrics.
- Do not adopt Parallel Agents, Threads Sidebar, worktree-thread orchestration, or agent metrics dashboards in this phase.
- Do not include collaboration, calls, Business/team management, or DeltaDB work.
- Do not treat general editor/search/terminal sync as part of this phase except where a candidate directly affects next-edit or ACP external-agent behavior.

---

## Key Decisions

- Start with next-edit parity before ACP registry polish: the next-edit surface is already part of Superzent's default build and has a narrower validation path.
- Treat ACP registry work as external-agent reliability, not product-shape migration: registry-installed agents should improve center-pane ACP tabs, not pull in the upstream Agent Panel.
- Keep cherry-picks small and auditable: upstream 1.0 includes broad agentic product work, but Superzent should only absorb fixes that support its current product identity.
- Preserve explicit provider boundaries: non-Zed-hosted provider support is in scope; Zed-hosted provider exposure is not.

---

## Dependencies / Assumptions

- Zed 1.0 shipped on 2026-04-29 and emphasizes AI-native editing, multiple agents, edit predictions, and ACP.
- Superzent's README defines the default app build as `lite + acp_tabs + next_edit` and explicitly excludes Zed-hosted AI surfaces, cloud collaboration, calls, the docked Agent Panel, and native text threads.
- `docs/brainstorms/2026-04-05-default-build-next-edit-requirements.md` remains the baseline for default-build next-edit provider scope.
- `docs/brainstorms/2026-04-23-upstream-sync-editor-search-requirements.md` remains the baseline for selective upstream sync and broad-agent/sidebar exclusion.
- Planning must re-check whether each candidate is already present in Superzent under a different local commit or has been superseded by a Superzent-specific adaptation.

---

## Outstanding Questions

### Deferred to Planning

- [Affects R1-R3][Technical] Which shortlisted candidates cherry-pick cleanly, which are already present under different local commits, and which require Superzent-specific adaptation?
- [Affects R5-R9][Technical] Which next-edit fixes are reachable in the default build without bringing in Zed-hosted provider UI or telemetry paths?
- [Affects R10-R14][Technical] Which ACP registry and external-agent fixes fit Superzent's center-pane ACP tab flow without requiring the upstream Threads Sidebar or docked Agent Panel?
- [Affects R8][Needs research] Does the OpenAI responses API update improve allowed provider behavior in Superzent, or is it too coupled to upstream hosted/model churn for this phase?
