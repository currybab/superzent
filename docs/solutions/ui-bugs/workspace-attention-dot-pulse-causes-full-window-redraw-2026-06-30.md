---
title: Animated workspace attention dot pins CPU via full-window redraw
date: 2026-06-30
category: ui-bugs
module: superzent_ui sidebar attention indicator
problem_type: ui_bug
component: superzent_ui
symptoms:
  - Main thread sits at ~70% CPU the whole time an agent task is running (Working/Permission state)
  - CPU returns to idle when the sidebar is collapsed, even though the agent is still running
  - A sampling profile shows the main thread looping in `gpui::Window::draw` → `taffy::compute::flexbox` every frame, never going idle
  - The app feels unusable during long agent sessions
root_cause: wrong_api
resolution_type: code_fix
severity: high
applies_when:
  - "Adding a continuously animated element (pulse/spin/blink) to an always-present part of the UI in GPUI"
  - "Using `with_animation(...).repeat()` for an indefinite animation"
  - "Investigating high idle CPU / runaway redraw in a GPUI app"
tags:
  - gpui
  - animation
  - performance
  - redraw-loop
  - attention-indicator
  - with_animation
  - request_animation_frame
---

# Animated workspace attention dot pins CPU via full-window redraw

## Problem

The sidebar workspace attention dot pulsed (opacity fade) while a workspace was in the `Working` or `Permission` state, using:

```rust
div()
    .child(Indicator::dot().color(Color::Warning))
    .with_animation(
        id,
        Animation::new(Duration::from_millis(900)).repeat(),
        |indicator, delta| indicator.opacity(/* sine pulse */),
    )
```

`gpui::with_animation` calls `window.request_animation_frame()` on every layout pass (`crates/gpui/src/elements/animation.rs`), with no frame-rate cap. Because GPUI rebuilds the Taffy layout tree on every `draw()` (`layout_engine.clear()` in `crates/gpui/src/window.rs`), each requested frame re-lays-out and repaints the **entire window** — sidebar, editor, and terminal — on the main thread. With `.repeat()` this never stops, so the window redraws at the display refresh rate (120Hz on ProMotion) the whole time the dot is on screen, just to fade one 12px dot.

## Symptoms

See frontmatter. The tell-tale sign is that collapsing the sidebar (removing the animated element from the tree) immediately drops CPU to idle while the agent keeps running, and a `sample` profile shows a perpetual `Window::draw → request_layout → taffy flexbox` loop on `com.apple.main-thread`.

## Root Cause

`with_animation(...).repeat()` ties the redraw cadence to the display refresh rate. A 900ms opacity pulse is visually identical at ~20fps and at 120fps, so the higher rate buys nothing but burns ~6× the CPU. The cost is not the dot — it is that GPUI re-lays-out the whole window per animation frame (no cross-frame layout cache unless a view is explicitly `.cached(...)`; no compositor-level opacity animation).

## Solution

Replace `with_animation` for the attention dot with a small, file-private `PulsingDot` element in `crates/superzent_ui/src/lib.rs` that schedules its own redraw on a fixed ~20fps interval instead of every frame:

- In `request_layout`, compute opacity from `start.elapsed()` and, instead of `window.request_animation_frame()`, `cx.spawn` a task that awaits `cx.background_executor().timer(50ms)` then `cx.update(|cx| cx.notify(window.current_view()))`.
- Store `start` and the redraw `Task` in GPUI element state via `with_element_state`, keyed by id. No `SuperzentSidebar` struct fields and no start/stop lifecycle wiring are needed.
- When the dot leaves the tree (status returns to `Idle`/`Review`), its element state is dropped, which cancels the redraw task — the pulse stops on its own.

Measured: an active pulse drops from ~70% CPU to under 10% on a 120Hz machine, with no visible change to the pulse.

## Why This Works

GPUI marks only the notified view and its **ancestors** dirty (`Window::mark_view_dirty`), and the cost of a redraw scales with how often it happens. Capping the cadence at ~20fps cuts the number of full-window relayouts by ~6× while keeping the pulse smooth, because the eye cannot distinguish a 900ms fade at 20fps from one at 120fps.

## Alternatives Considered

- **Static dot** (no animation): cheapest, but loses the pulse affordance.
- **`.cached(...)` on heavy sibling panels**: since dirtiness only propagates to ancestors, wrapping the editor/terminal subtrees in `.cached(StyleRefinement)` would let a pulse frame `reuse_prepaint`/`reuse_paint` those subtrees and re-lay-out only the dirty ancestor chain. More thorough but more invasive (cached-view staleness risk), so it was left as a possible follow-up rather than bundled with this fix.
- There is no GPUI equivalent of a browser/compositor `transform`/`opacity` animation, so a truly zero-CPU pulse (à la CSS `animate-ping`) is not possible without framework-level work.

## Prevention

In GPUI, treat any `with_animation(...).repeat()` on an element that can stay on screen indefinitely as a continuous full-window redraw driver. For low-frequency indicators (pulse/blink), drive the redraw with an explicit interval timer so the cadence is decoupled from the display refresh rate, and/or `.cached(...)` the expensive, unrelated subtrees.

## Related

- `crates/gpui/src/elements/animation.rs` — `AnimationElement::request_layout` unconditionally calls `request_animation_frame`.
- `crates/gpui/src/window.rs` — `Window::draw` clears the layout engine each frame; `mark_view_dirty` propagates to ancestors; `View::cached` reuse path in `crates/gpui/src/view.rs`.
- upstream PR: currybab/superzent#38
