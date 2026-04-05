---
date: 2026-04-05
topic: default-build-next-edit
---

# Default Build Next Edit

## Problem Frame

`superzent`'s roadmap explicitly calls for next-edit integration, but the current default build is
intentionally narrow and excludes the inherited hosted AI and edit prediction stack. The repo
still contains substantial upstream edit prediction UI, provider setup, and runtime code, so the
highest-leverage move is to bring back next-edit in the default build without reopening Zed-hosted
AI or the broader upstream product surface.

## Requirements

**Build Surface**

- R1. The default `superzent` build must support next-edit in the general code editor surface.
- R2. The feature must reuse the existing upstream-like edit prediction experience as much as
  possible instead of introducing a superzent-specific replacement.
- R3. Re-enabling next-edit must not implicitly restore Zed-hosted AI surfaces, Zed login
  requirements, cloud collaboration, or the docked agent/text-thread product surface.

**Provider Scope**

- R4. The default build must support these non-Zed-hosted providers in v1:
  Ollama, OpenAI-compatible API, GitHub Copilot, Codestral, Sweep, and Mercury.
- R5. The Zed-hosted provider must not appear in the default-build UI for this feature.
- R6. Provider-specific setup must keep using the existing provider-auth/config flows where they
  already exist, rather than inventing a new superzent-only setup flow.
- R7. If existing user settings still point to the Zed-hosted provider, the default build must
  treat that state as effectively unconfigured and return the user to the setup path rather than
  trying to activate Zed-hosted edit prediction.

**User Experience**

- R8. Next-edit must appear as automatic inline prediction in normal editor buffers when a
  supported provider is configured.
- R9. The default build must expose the existing status bar edit prediction entry.
- R10. The status bar menu must continue to support provider switching and setup entry points.
- R11. The existing dedicated "Configure Providers" settings flow must be available from the
  default build.
- R12. When no supported provider is configured, the status bar entry must still be visible and
  guide the user into setup instead of disappearing.
- R13. In v1, next-edit support is limited to general code editor buffers and does not extend to
  agent text threads or arbitrary text inputs.

## Provider Matrix

| Provider              | v1 status in default build | Notes                                     |
| --------------------- | -------------------------- | ----------------------------------------- |
| Ollama                | Supported                  | Existing local/self-hosted path           |
| OpenAI-compatible API | Supported                  | Existing self-hosted/custom path          |
| GitHub Copilot        | Supported                  | Existing provider/auth flow               |
| Codestral             | Supported                  | Existing API-key flow                     |
| Sweep                 | Supported                  | Existing API-key flow                     |
| Mercury               | Supported                  | Existing API-key flow                     |
| Zed-hosted / Zeta     | Hidden                     | Requires Zed-hosted login/product surface |

## Success Criteria

- A user on the default build can discover next-edit from the status bar without enabling the full
  upstream AI surface.
- A user can configure one of the supported non-Zed-hosted providers from the existing settings
  flow and start receiving automatic inline predictions in regular code buffers.
- The default build does not expose Zed-hosted edit prediction as an available choice.
- A user with stale `provider: "zed"` settings is guided back into provider setup instead of being
  left in a broken or hidden state.
- Existing roadmap intent ("next-edit integration") is satisfied without broadening the default
  product into hosted AI or upstream docked AI surfaces.

## Scope Boundaries

- No support for Zed-hosted edit prediction in the default build.
- No requirement to support agent text threads or non-editor text fields in v1.
- No redesign of the upstream edit prediction UX; the goal is controlled reuse.
- No requirement to bring back the full `ai` feature bundle if a narrower default-build path can
  deliver the agreed surface.

## Key Decisions

- Reuse the upstream status bar menu and provider setup flow: this is the fastest path to a
  coherent user experience with the least product invention.
- Treat next-edit as part of the default shell again: roadmap value is in making it available
  where users already work, not in hiding it behind an opt-in debug build.
- Keep the provider set non-Zed-hosted: this preserves superzent's current stance against
  default-build dependence on Zed-hosted AI/login flows.
- Interpret legacy/default-build `provider: "zed"` state as unsupported and recover to setup:
  this avoids a confusing broken state for users carrying old settings into the narrower default
  build.
- Keep the initial state off until the user selects/configures a provider: avoids surprising
  network behavior and keeps the default build explicit.

## Dependencies / Assumptions

- Existing edit prediction runtime, status bar UI, and provider setup page are still present in the
  repo and primarily need feature-surface reactivation rather than net-new product design.
- Sweep and Mercury can remain within the allowed provider scope because they use their own
  provider/API-key flows rather than Zed-hosted auth.

## Outstanding Questions

### Deferred to Planning

- [Affects R1][Technical] What is the smallest feature/dependency slice that enables edit
  prediction in the default build without reintroducing unrelated hosted AI surfaces?
- [Affects R10][Technical] Which parts of the existing settings UI are gated behind `ai` today and
  need to move into the default build to preserve the upstream setup flow?
- [Affects R5][Technical] What is the cleanest way to hide Zed-hosted provider choices in default
  build UI while preserving upstream behavior for heavier/debug builds?

## Next Steps

-> /prompts:ce-plan for structured implementation planning
