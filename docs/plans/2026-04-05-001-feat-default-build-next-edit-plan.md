---
title: feat: Restore default-build next edit
type: feat
status: completed
date: 2026-04-05
origin: docs/brainstorms/2026-04-05-default-build-next-edit-requirements.md
---

# feat: Restore default-build next edit

## Overview

Re-enable next-edit in the default `superzent` build by carving a narrower edit-prediction
feature slice out of the current `ai` gate, then reusing the existing provider setup flow,
edit-prediction runtime, and a footer-mounted shell entry for non-Zed-hosted providers only. The
default build must gain editor-buffer next-edit without reintroducing Zed-hosted AI, Zed login,
or the broader upstream docked AI surface.

## Problem Frame

The product roadmap explicitly calls for next-edit integration, but the default app build is still
`lite + acp_tabs`, and `docs/src/development.md`/`docs/src/getting-started.md` currently say the
default build excludes edit prediction stacks. At the same time, the repository already contains:

- runtime provider wiring in `crates/zed/src/zed/edit_prediction_registry.rs`
- an existing edit prediction entry UI in `crates/edit_prediction_ui/src/edit_prediction_button.rs`
- a provider setup page in `crates/settings_ui/src/pages/edit_prediction_provider_setup.rs`
- a visual test that already expects `edit_predictions.providers` to open a settings subpage in
  `crates/zed/src/visual_test_runner.rs`

This is not a greenfield feature. The work is primarily controlled reactivation and feature-slice
separation.

## Requirements Trace

- R1. Default build supports next-edit in general code editor buffers.
- R2. Reuse the existing upstream-like edit prediction experience instead of inventing a new one.
- R3. Do not implicitly restore Zed-hosted AI/login, collab, or docked agent/text-thread surfaces.
- R4. Support Ollama, OpenAI-compatible API, GitHub Copilot, Codestral, Sweep, and Mercury.
- R5. Hide the Zed-hosted provider in default-build UI.
- R6. Preserve existing provider-specific auth/config flows.
- R7. Treat stale `provider: "zed"` config as effectively unconfigured in the default build.
- R8. Show automatic inline prediction in normal editor buffers when a supported provider is
  configured.
- R9. Expose the existing edit prediction entry in the default build.
- R10. Preserve provider switching and setup entry points in the entry flow.
- R11. Make the existing "Configure Providers" settings flow available in the default build.
- R12. Keep the entry visible when no supported provider is configured.
- R13. Limit v1 support to general code editor buffers.

## Scope Boundaries

- Do not support Zed-hosted/Zeta in the default build.
- Do not add next-edit to agent text threads or arbitrary text inputs in v1.
- Do not redesign the edit prediction UX; controlled upstream reuse is the goal.
- Do not broaden the default build into the full `ai` surface if a narrower feature slice can
  satisfy the requirements.

## Context & Research

### Relevant Code and Patterns

- `crates/zed/Cargo.toml` currently keeps edit prediction dependencies under the broad `ai`
  feature, while default remains `lite + acp_tabs`.
- `crates/zed/src/main.rs` and `crates/zed/src/zed.rs` gate edit prediction init and entry UI
  behind `#[cfg(feature = "ai")]`.
- `crates/settings_ui/Cargo.toml` also ties edit prediction setup UI to a broad `ai` feature,
  but the provider page itself already exists in
  `crates/settings_ui/src/pages/edit_prediction_provider_setup.rs`.
- `crates/edit_prediction_ui/src/edit_prediction_button.rs` already contains most of the desired
  v1 UX: status item rendering, provider switching, "Predict Edit at Cursor", and a
  `Configure Providers` action that dispatches `OpenSettingsAt { path: "edit_predictions.providers" }`.
- `crates/zed/src/visual_test_runner.rs` already expects `edit_predictions.providers` to resolve
  to a single subpage link and auto-open in settings UI. That makes subpage restoration a
  compatibility fix, not new product invention.
- `crates/zed/src/zed/edit_prediction_registry.rs` already maps supported providers into runtime
  delegates. It treats Sweep, Mercury, and OpenAI-compatible/FIM variants as part of the existing
  edit-prediction runtime rather than as separate product surfaces.
- `crates/edit_prediction_ui/src/edit_prediction_button.rs:get_available_providers` currently
  always includes `EditPredictionProvider::Zed`, so default-build provider filtering needs to be
  centralized there and in the registry.
- `crates/language/src/language_settings.rs` already resolves a missing provider to
  `EditPredictionProvider::None`, so "default off until configured" is consistent with current
  runtime behavior.

### Institutional Learnings

- No `docs/solutions/` corpus is present in this repo, so the plan is grounded in current code and
  existing docs/tests rather than prior solution notes.

### External References

- None. This is a repo-internal feature-slicing and UX restoration problem.

## Key Technical Decisions

- Introduce a narrower default-build feature slice for next-edit instead of enabling the entire
  existing `ai` bundle.
  Rationale: the current `ai` feature also brings back broader hosted/upstream AI surfaces that
  violate the origin requirements.
- Preserve `full` as the place where upstream Zed-hosted behavior can still exist.
  Rationale: the default build and the heavier/debug build need different provider policy without
  forking the codebase.
- Normalize unsupported `EditPredictionProvider::Zed` to an effective "unconfigured" runtime state
  in the default build rather than rewriting the user's settings file.
  Rationale: this avoids destructive migration while keeping full/debug builds free to honor the
  same settings file.
- Reuse the existing edit prediction menu and provider setup page with build-aware filtering.
  Rationale: the UX already exists and is closer to product intent than inventing a shell-specific
  replacement.

## Open Questions

### Resolved During Planning

- Should the implementation maximize reuse or prioritize a bespoke lighter UX?
  Resolution: maximize upstream reuse and carve a narrower feature slice underneath it.
- How should stale `provider: "zed"` settings behave in the default build?
  Resolution: treat them as unsupported/unconfigured at runtime and route the user back into setup.
- Do Sweep and Mercury belong in the supported provider set?
  Resolution: yes; they use their own provider/API-key flows and stay within the allowed scope.

### Deferred to Implementation

- What exact feature names best express the split between default-build next-edit and the broader
  upstream `ai` surface?
  Why deferred: this is a codebase fit-and-finish decision, but it must land as a narrow slice and
  not as a broad `ai` re-enable.
- Whether the settings provider page should be restored as a static `SubPageLink`, a dynamic
  subpage, or both.
  Why deferred: the existing code supports both patterns, and the cleanest choice depends on how
  much of the prior settings wiring still exists.

## High-Level Technical Design

> _This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce._

| Build / state    | Provider in settings       | Entry behavior                                   | Prediction runtime         |
| ---------------- | -------------------------- | ------------------------------------------------ | -------------------------- |
| Default build    | `none` or missing          | Visible footer entry; click opens provider setup | Disabled                   |
| Default build    | Supported non-Zed provider | Visible footer entry with provider-specific menu | Enabled in editor buffers  |
| Default build    | `zed`                      | Visible footer entry; treated like unconfigured  | Disabled                   |
| Full/debug build | `zed`                      | Existing upstream behavior                       | Existing upstream behavior |

## Implementation Units

- [x] **Unit 1: Carve a default-build next-edit feature slice**

**Goal:** Separate edit prediction from the broader `ai` feature so the default build can include
next-edit without reopening unrelated hosted AI surfaces.

**Requirements:** R1, R2, R3, R11

**Dependencies:** None

**Files:**

- Modify: `crates/zed/Cargo.toml`
- Modify: `crates/settings_ui/Cargo.toml`
- Modify: `crates/language_tools/Cargo.toml`
- Modify: `crates/zed/src/main.rs`
- Modify: `crates/zed/src/zed.rs`
- Modify: `crates/settings_ui/src/components.rs`
- Modify: `crates/settings_ui/src/pages.rs`
- Test: `crates/zed/src/main.rs` (or the nearest existing app bootstrap coverage)
- Test: `crates/zed/src/zed.rs`

**Approach:**

- Introduce a narrower feature slice in the app crate for default-build next-edit and make it part
  of the default build.
- Move edit-prediction-specific dependency gates from `ai` to that narrower slice where possible,
  keeping `ai` reserved for hosted/upstream-heavy surfaces.
- Split `settings_ui` gating so the edit prediction provider setup page and supporting components
  can exist without pulling in unrelated AI-only settings surfaces such as tool permissions.
- Re-gate startup init and edit prediction entry creation in `crates/zed/src/main.rs` and
  `crates/zed/src/zed.rs` to the narrower feature instead of `ai`.

**Patterns to follow:**

- Existing `acp_tabs` vs `ai` feature split in `crates/zed/Cargo.toml`
- Existing crate-local feature split in `crates/settings_ui/Cargo.toml`

**Test scenarios:**

- Happy path: the default app feature set includes the new next-edit slice and compiles with edit
  prediction support available.
- Edge case: enabling `full` still compiles and retains the heavier upstream AI surface.
- Integration: the default build path initializes edit prediction without re-enabling unrelated
  hosted AI or agent-panel setup code.

**Verification:**

- The default build can instantiate next-edit surfaces.
- The `ai` feature remains a broader surface than the new default-build slice.

- [x] **Unit 2: Restore provider policy and runtime assignment for the default build**

**Goal:** Make supported providers work in the default build while hiding or neutralizing the
Zed-hosted provider.

**Requirements:** R3, R4, R5, R6, R7, R8, R13

**Dependencies:** Unit 1

**Files:**

- Modify: `crates/zed/src/zed/edit_prediction_registry.rs`
- Modify: `crates/edit_prediction_ui/src/edit_prediction_button.rs`
- Modify: `crates/settings_content/src/language.rs` or a nearby policy/helper location if needed
- Test: `crates/zed/src/zed/edit_prediction_registry.rs`
- Test: `crates/edit_prediction_ui/src/edit_prediction_button.rs`

**Approach:**

- Introduce a centralized build-aware provider policy helper so the registry and UI make the same
  decision about which providers are supported in the default build.
- In the default build, treat `EditPredictionProvider::Zed` as unsupported and normalize it to an
  effective unconfigured state rather than assigning a runtime provider.
- Preserve runtime delegate assignment for Copilot, Codestral, Ollama/OpenAI-compatible FIM,
  Sweep, and Mercury.
- Filter `get_available_providers` and any setup/provider lists so Zed-hosted never appears in the
  default-build UI.
- Keep full/debug builds free to preserve upstream Zed-provider behavior.

**Patterns to follow:**

- Existing provider assignment flow in `crates/zed/src/zed/edit_prediction_registry.rs`
- Existing provider availability logic in `crates/edit_prediction_ui/src/edit_prediction_button.rs`
- Existing settings-change regression test style in `crates/zed/src/zed/edit_prediction_registry.rs`

**Test scenarios:**

- Happy path: selecting or configuring Codestral/Copilot/Ollama/OpenAI-compatible/Sweep/Mercury
  in the default build results in an assigned edit prediction provider.
- Edge case: `provider: "zed"` in user settings leaves the editor without a runtime provider in the
  default build.
- Edge case: provider lists used for switching/setup omit Zed in the default build but retain the
  non-Zed supported providers.
- Integration: changing settings from unsupported/no provider to a supported provider updates
  existing editors without reopening windows.

**Verification:**

- Supported providers assign correctly in the default build.
- Stale Zed-provider settings no longer strand the user in a broken hidden state.

- [x] **Unit 3: Restore the footer entry and settings setup flow in the default build**

**Goal:** Re-enable the edit prediction UX surface so users can discover, configure, and switch
supported providers from the default build.

**Requirements:** R2, R8, R9, R10, R11, R12

**Dependencies:** Unit 1, Unit 2

**Files:**

- Modify: `crates/edit_prediction_ui/src/edit_prediction_button.rs`
- Modify: `crates/settings_ui/src/pages.rs`
- Modify: `crates/settings_ui/src/page_data.rs`
- Modify: `crates/settings_ui/src/settings_ui.rs`
- Modify: `crates/settings_ui/src/pages/edit_prediction_provider_setup.rs`
- Modify: `crates/zed/src/visual_test_runner.rs`
- Test: `crates/edit_prediction_ui/src/edit_prediction_button.rs`
- Test: `crates/zed/src/visual_test_runner.rs`
- Test: `crates/settings_ui/src/settings_ui.rs`

**Approach:**

- Change the `EditPredictionProvider::None`/unsupported rendering path so the footer entry stays
  visible and routes the user into provider setup instead of hiding entirely.
- Reuse the existing menu structure for configured-provider states, including `Predict Edit at
Cursor`, mode toggles where they still apply, provider switching, and `Configure Providers`.
- Restore or re-add the `edit_predictions.providers` settings subpage path so
  `OpenSettingsAt { path: "edit_predictions.providers" }` works in the default build and aligns
  with the existing visual test expectation.
- Re-enable the provider setup page and its supporting renderers/components under the narrower
  feature slice rather than the broad `ai` gate.
- Keep Zed-hosted provider cards/choices out of the default-build settings/setup UX.

**Patterns to follow:**

- Existing status item and menu composition in `crates/edit_prediction_ui/src/edit_prediction_button.rs`
- Existing dynamic subpage pattern in `crates/settings_ui/src/pages/tool_permissions_setup.rs`
- Existing `OpenSettingsAt` subpage auto-open path tested in `crates/zed/src/visual_test_runner.rs`

**Test scenarios:**

- Happy path: with no supported provider configured, the footer edit prediction entry remains
  visible and opens provider setup.
- Happy path: `OpenSettingsAt { path: "edit_predictions.providers" }` opens the provider setup
  subpage in the default build.
- Edge case: default-build provider setup UI excludes Zed-hosted while keeping the supported
  provider cards/controls visible.
- Integration: after configuring a supported provider, the same footer entry transitions from a
  direct setup affordance to provider-oriented behavior without requiring a build-flavor change.

**Verification:**

- A fresh default-build user can discover next-edit from the footer entry and reach provider setup.
- The restored setup path matches the current `superzent` shell UX instead of inventing a second,
  unrelated discoverability surface.

- [x] **Unit 4: Align docs and scope statements with the restored default-build surface**

**Goal:** Update product and development docs so they no longer claim the default build excludes
edit prediction.

**Requirements:** R1, R3, R4, R5, R11

**Dependencies:** Unit 1, Unit 2, Unit 3

**Files:**

- Modify: `README.md`
- Modify: `docs/src/development.md`
- Modify: `docs/src/getting-started.md`
- Modify: `docs/src/ai/edit-prediction.md`
- Test: none -- documentation-only changes for this unit

**Approach:**

- Remove or refine statements that say the default build excludes edit prediction stacks.
- Add superzent-specific guidance clarifying that the default build now supports next-edit with
  non-Zed-hosted providers only.
- Keep docs clear that this does not bring back Zed-hosted AI or the broader docked AI surface.

**Patterns to follow:**

- Existing scope/roadmap language in `README.md`
- Existing build-flavor documentation in `docs/src/development.md`
- Existing product-scope summary in `docs/src/getting-started.md`

**Test scenarios:**

- Test expectation: none -- documentation-only unit, but the text should be internally consistent
  with the implemented build surface and provider policy.

**Verification:**

- Docs no longer contradict the restored default-build next-edit behavior.
- Docs clearly preserve the boundary that Zed-hosted AI remains out of scope for the default build.

## System-Wide Impact

- **Interaction graph:** build features now affect app bootstrap, footer/status entry composition, provider
  registry assignment, settings UI subpage routing, and user-facing docs.
- **Error propagation:** unsupported-provider states should degrade into visible setup affordances,
  not hidden UI or silent no-op provider assignment.
- **State lifecycle risks:** stale `provider: "zed"` settings are the main compatibility hazard in
  the default build; runtime normalization must be consistent across registry and UI.
- **API surface parity:** `full` builds must retain the broader upstream provider behavior even as
  the default build narrows it.
- **Integration coverage:** compile/build-flavor coverage plus UI/settings navigation coverage are
  both necessary; unit tests alone will not prove the subpage path or status item behavior.
- **Unchanged invariants:** the default build still excludes hosted AI, collab, and docked upstream
  agent/text-thread surfaces.

## Risks & Dependencies

| Risk                                                                              | Mitigation                                                                                             |
| --------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| The narrow feature slice still drags in too much of `ai`                          | Audit dependencies crate-by-crate and split gates instead of reusing `ai` wholesale                    |
| `provider: "zed"` remains selected and hides the feature                          | Normalize unsupported Zed provider to an unconfigured visible-setup state everywhere that matters      |
| The provider setup page stays unreachable because the wiring is partially removed | Restore the `edit_predictions.providers` path explicitly and keep the visual test expectation in place |
| Docs drift and still claim edit prediction is absent from the default build       | Ship explicit docs updates in the same change                                                          |

## Documentation / Operational Notes

- This feature changes user-visible default-build scope, so `README.md`,
  `docs/src/development.md`, and `docs/src/getting-started.md` must be updated in the same work.
- `docs/src/ai/edit-prediction.md` should gain a superzent-specific note about default-build
  provider scope so users do not infer Zed-hosted support.
- No special post-deploy monitoring is expected beyond normal regression testing because this is a
  desktop feature-surface change rather than a server-side rollout.

## Sources & References

- Origin document: `docs/brainstorms/2026-04-05-default-build-next-edit-requirements.md`
- Related code: `crates/zed/Cargo.toml`
- Related code: `crates/zed/src/main.rs`
- Related code: `crates/zed/src/zed.rs`
- Related code: `crates/zed/src/zed/edit_prediction_registry.rs`
- Related code: `crates/edit_prediction_ui/src/edit_prediction_button.rs`
- Related code: `crates/settings_ui/src/pages/edit_prediction_provider_setup.rs`
- Related code: `crates/settings_ui/src/settings_ui.rs`
- Related code: `crates/settings_ui/src/page_data.rs`
- Related code: `crates/zed/src/visual_test_runner.rs`
- Related docs: `README.md`
- Related docs: `docs/src/development.md`
- Related docs: `docs/src/getting-started.md`
- Related docs: `docs/src/ai/edit-prediction.md`
