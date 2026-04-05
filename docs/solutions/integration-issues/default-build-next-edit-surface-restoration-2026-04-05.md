---
title: Restoring default-build next-edit requires separating it from hosted AI surfaces
date: 2026-04-05
last_updated: 2026-04-05
category: integration-issues
module: default-build next edit
problem_type: integration_issue
component: tooling
symptoms:
  - Default `superzent` builds did not expose next-edit even though the runtime, provider setup page, and entry UI already existed in the repo
  - Re-enabling next-edit through the broad `ai` feature also pulled hosted AI/chat surfaces back into the default build
  - Provider setup and switching behavior diverged because some call sites treated provider lists as "supported by this build" while others treated them as "ready to use right now"
  - Gating `CopilotChat` provider registration on `CopilotChat::global(cx)` during startup broke full `ai` builds because `language_models::init()` runs before `copilot_chat::init()`
root_cause: logic_error
resolution_type: code_fix
severity: high
related_components:
  - assistant
  - documentation
tags:
  [
    next-edit,
    edit-prediction,
    feature-gating,
    provider-policy,
    disable-ai,
    copilot-chat,
    footer-entry,
  ]
---

# Restoring default-build next-edit requires separating it from hosted AI surfaces

## Problem

`superzent` wanted default-build next-edit in ordinary editor buffers, but the inherited upstream wiring coupled edit prediction to the broader `ai` surface. Turning next-edit back on naïvely either kept it hidden or reopened hosted AI/chat behavior that the default build is explicitly trying to avoid.

## Symptoms

- The default build compiled without a visible next-edit affordance even though edit prediction UI and provider setup code already existed.
- Treating `EditPredictionProvider::Zed` as the default left users in a hidden or broken state when the default build was not supposed to expose Zed-hosted prediction.
- Moving too much under the new slice accidentally reintroduced `copilot_chat` initialization and language-model provider registration in the default build.
- Provider selection UX regressed when one helper started meaning "supported by this build" instead of "available to switch to right now."

## What Didn't Work

- Reusing the upstream `ai` feature wholesale was too broad. It brought hosted AI/chat behavior back with next-edit.
- Leaving the default upstream provider alone (`provider: "zed"`) made the default build look unconfigured or invisible in the wrong ways instead of guiding users into setup.
- Using one provider helper for both setup and runtime switching caused UX drift:
  - Setup wants the full supported list.
  - Runtime switching wants providers that are actually ready now.
- Guarding `CopilotChat` provider registration on `CopilotChat::global(cx)` inside `language_models::init()` looked like an easy way to keep chat models out of the default build, but it made provider registration depend on startup order and removed Copilot Chat models from full builds too.
- Keeping the entry on the hidden global status bar made the feature effectively undiscoverable in the current `superzent` shell.

## Solution

Split next-edit into its own default-build feature slice and then separate three concerns that had been coupled together: hosted AI/chat boot, provider support policy, and provider readiness.

1. Add a narrow `next_edit` feature to the app crate and make it part of the default build:

```toml
[features]
default = ["lite", "acp_tabs", "next_edit"]

next_edit = [
  "dep:codestral",
  "dep:copilot",
  "dep:copilot_chat",
  "dep:edit_prediction",
  "dep:edit_prediction_ui",
  "language_tools/edit-prediction",
  "settings_ui/edit_prediction",
]

ai = [
  "next_edit",
  "acp_tabs",
  "dep:agent-client-protocol",
  "dep:agent_settings",
  "edit_prediction_ui/zed-hosted-provider",
  "git_ui/ai",
  "settings_ui/ai",
  "terminal_view/assistant",
]
```

2. Centralize default-build provider policy so stale `provider: "zed"` becomes an effective `None` state outside full builds:

```rust
pub fn normalize_edit_prediction_provider(
    provider: EditPredictionProvider,
) -> EditPredictionProvider {
    if edit_prediction_provider_supported(provider) {
        provider
    } else {
        EditPredictionProvider::None
    }
}
```

3. Keep hosted chat/model surfaces out of the default build while preserving them in full builds by separating chat boot from provider registration:

```rust
#[cfg(feature = "ai")]
{
    copilot_chat::init(..., cx);
    language_models::register_copilot_chat_provider(cx);
}

#[cfg(feature = "next_edit")]
{
    edit_prediction_registry::init(..., cx);
}
```

Inside `language_models`, use an explicit registration hook instead of an order-sensitive `CopilotChat::global(cx)` guard during base provider init:

```rust
pub fn register_copilot_chat_provider(cx: &mut App) {
    let registry = LanguageModelRegistry::global(cx);
    registry.update(cx, |registry, cx| {
        registry.unregister_provider(
            LanguageModelProviderId::from("copilot_chat".to_string()),
            cx,
        );
        registry.register_provider(Arc::new(CopilotChatLanguageModelProvider::new(cx)), cx);
    });
}
```

4. Split provider helpers by purpose:

- `supported_edit_prediction_providers()` for setup UI
- `get_available_providers()` for runtime switching menu

5. Put the discoverability entry where the current `superzent` shell actually shows it: the center-pane footer. For the no-provider state, clicking the icon goes directly to `edit_predictions.providers`.

6. Keep next-edit working even when `disable_ai` is being used to suppress hosted AI/chat surfaces. In `superzent`, next-edit is treated as a narrower exception capability rather than as part of the full hosted AI product surface.

7. Add an explicit `Off` label for `EditPredictionProvider::None` so setup UI can show a first-class disabled state instead of silently omitting it.

## Why This Works

The real problem was not "next-edit is missing." It was that multiple layers had been coupled in ways that were safe upstream but wrong for `superzent`'s default shell:

- build feature gating
- provider policy
- provider readiness
- hosted chat/model boot
- entry discoverability

The fix works because each layer now has a narrower contract:

- `next_edit` turns on edit prediction without implying hosted AI/chat.
- Unsupported hosted providers normalize to `None` instead of leaving hidden dead states behind.
- Setup uses the supported provider list; runtime switching uses the currently ready list.
- Default builds no longer initialize Copilot Chat just because Copilot is allowed for next-edit.
- Full builds still recover Copilot Chat models because provider registration happens explicitly after `copilot_chat::init()`, not opportunistically during earlier startup.
- Users can always find the feature from the footer entry and reach setup directly.

## Prevention

- Keep "supported by this build" and "usable right now" as separate helpers. Do not reuse one provider list for both setup and runtime menus.
- If a feature slice is supposed to exclude hosted AI/chat surfaces, audit boot code and provider registration, not just Cargo features.
- Avoid order-sensitive global-presence guards during startup. If a provider depends on a later init step, expose an explicit `register_*` hook and call it after the dependency is initialized.
- Add targeted regression coverage for:
  - default build does not initialize hosted chat/model surfaces
  - full builds still register Copilot Chat providers after `copilot_chat::init()`
  - stale `provider: "zed"` normalizes to an unconfigured next-edit state
  - `disable_ai` policy still does what the product intends after any exception is introduced
  - the user-visible entry surface matches the actual shell surface (`footer` vs hidden status bar)
- When documentation or plan language drifts from implementation, fix the contract quickly. "Status bar" vs "footer entry" sounds cosmetic, but it changes discoverability and review outcomes.

## Related Issues

- Related requirements: `docs/brainstorms/2026-04-05-default-build-next-edit-requirements.md`
- Related plan: `docs/plans/2026-04-05-001-feat-default-build-next-edit-plan.md`
- Related solution with low overlap: `docs/solutions/integration-issues/managed-terminal-popup-notifications-2026-04-04.md`
