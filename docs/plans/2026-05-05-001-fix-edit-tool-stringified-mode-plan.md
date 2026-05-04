---
title: "fix: Support stringified edit-tool mode input"
type: fix
status: active
date: 2026-05-05
---

# Support Stringified Edit-Tool Mode Input

## Summary

Backport the upstream Zed edit-tool input robustness fix so Superzent accepts double-encoded JSON fields from agent tool calls. The change should keep normal edit-tool inputs unchanged while allowing stringified `mode` and `edits` values on both final and streaming partial inputs.

---

## Problem Frame

Zed PR #55500 cherry-picks upstream PR #55498, which fixed agent edit failures caused by models sending the edit tool's `mode` parameter as a JSON string containing a JSON string. Superzent carries the same `streaming_edit_file_tool` shape but currently only accepts native enum/array values for `mode` and `edits`.

---

## Assumptions

*This plan was authored without synchronous user confirmation. The items below are agent inferences that fill gaps in the input -- un-validated bets that should be reviewed before implementation proceeds.*

- The requested comparison is specifically about whether Zed PR #55500's edit-tool deserialization fix applies to Superzent.
- Superzent should accept the same stringified `mode` and `edits` inputs upstream now accepts, without broadening scope to unrelated agent tool changes.
- Directly adapting the upstream helper is preferable to introducing a new compatibility layer elsewhere in the agent stack because the malformed data reaches serde at this boundary.

---

## Requirements

- R1. Confirm whether Zed PR #55500 applies to Superzent's current code.
- R2. Accept normal native JSON `mode` and `edits` values exactly as before.
- R3. Accept double-encoded string JSON for final `StreamingEditFileToolInput.mode`.
- R4. Accept double-encoded string JSON for final `StreamingEditFileToolInput.edits`.
- R5. Accept double-encoded string JSON for streaming partial `mode` and `edits`.
- R6. Preserve absent and `null` optional partial fields as `None`.
- R7. Add focused tests for the compatibility behavior.

---

## Scope Boundaries

- Do not change edit matching, diff rendering, authorization, or file-write behavior.
- Do not alter agent tool schemas beyond serde input tolerance.
- Do not import unrelated upstream agent panel or ACP changes from nearby Zed commits.

---

## Context & Research

### Relevant Code and Patterns

- `crates/agent/src/tools/streaming_edit_file_tool.rs` defines `StreamingEditFileToolInput`, `StreamingEditFileToolPartialInput`, `StreamingEditFileMode`, and the tests for streaming edit behavior.
- Current final input serde accepts `mode: "edit"` and `edits: [...]`, but not `mode: "\"edit\""` or `edits: "[...]"`.
- Current partial input serde uses default `Option` fields and feeds successful partial parses into the streaming edit session startup path.
- Existing tests in `crates/agent/src/tools/streaming_edit_file_tool.rs` already use direct `serde_json::from_value` and `ToolInput` helpers, so this fix should add coverage in the same module.

### Institutional Learnings

- `docs/solutions/integration-issues/acp-stderr-logs-and-loading-session-close-2026-05-04.md` reinforces keeping upstream sync work scoped to the local Superzent boundary and adding focused tests for the actual integration path.
- `docs/plans/2026-05-03-001-feat-zed-1-0-next-edit-acp-sync-plan.md` establishes the current selective-upstream-sync posture: accept targeted reliability fixes without drifting into unrelated upstream surfaces.

### External References

- Zed PR #55500: `https://github.com/zed-industries/zed/pull/55500`
- Upstream source PR #55498 diff: `crates/agent/src/tools/streaming_edit_file_tool.rs` adds a generic stringified-value deserializer for `mode` and `edits`.

---

## Key Technical Decisions

- Apply the compatibility at serde boundaries: this catches malformed final inputs and partial snapshots before business logic sees them.
- Use one generic helper for enum, vector, and option-shaped values: the upstream approach keeps behavior consistent and avoids another single-purpose `edits` parser.
- Keep the helper local to `streaming_edit_file_tool.rs`: no other local tool currently shows the same pattern, and a shared utility would be premature.

---

## Open Questions

### Resolved During Planning

- Does the upstream fix apply? Yes. Superzent has the same `StreamingEditFileToolInput`/partial input structures but lacks the new `deserialize_maybe_stringified` annotations.

### Deferred to Implementation

- Exact test placement among the existing large test module: choose the nearby serde/input parsing section during implementation so the test remains discoverable.

---

## Implementation Units

- U1. **Add stringified-value serde support**

**Goal:** Allow edit-tool final and partial inputs to deserialize values that arrive as JSON strings containing JSON.

**Requirements:** R2, R3, R4, R5, R6

**Dependencies:** None

**Files:**
- Modify: `crates/agent/src/tools/streaming_edit_file_tool.rs`

**Approach:**
- Add a local untagged enum representing either a native value or a JSON string.
- Add a generic deserializer that first accepts native values, then parses string contents with `serde_json`.
- Annotate final `mode`, final `edits`, partial `mode`, and partial `edits` with that helper while preserving existing `default` and `skip_serializing_if` behavior.

**Patterns to follow:**
- The upstream Zed PR #55498 helper shape.
- Existing local serde field annotations in `StreamingEditFileToolInput` and `StreamingEditFileToolPartialInput`.

**Test scenarios:**
- Covered by U2 and U3.

**Verification:**
- Native `mode`/`edits` inputs still deserialize.
- Stringified `mode`/`edits` inputs deserialize to the same Rust values as native inputs.
- Missing and `null` optional partial values remain `None`.

- U2. **Cover final input deserialization**

**Goal:** Prove `StreamingEditFileToolInput` accepts double-encoded final fields without changing optional-field semantics.

**Requirements:** R2, R3, R4, R6, R7

**Dependencies:** U1

**Files:**
- Modify: `crates/agent/src/tools/streaming_edit_file_tool.rs`
- Test: `crates/agent/src/tools/streaming_edit_file_tool.rs`

**Approach:**
- Add direct `serde_json::from_value` tests for final edit-tool inputs.
- Cover double-encoded `mode`, double-encoded `edits`, omitted `edits`, and explicit `edits: null`.

**Patterns to follow:**
- Existing direct serde assertions and `json!` fixtures in the module's test block.

**Test scenarios:**
- Happy path: final input with `mode: "\"edit\""` and stringified `edits` deserializes to `Edit` mode with one edit.
- Edge case: final edit input with double-encoded `mode` and omitted `edits` deserializes with `edits == None`.
- Edge case: final edit input with double-encoded `mode` and `edits: null` deserializes with `edits == None`.

**Verification:**
- The new tests fail before U1 and pass after U1.

- U3. **Cover streaming partial deserialization**

**Goal:** Prove streaming partial snapshots can carry stringified `mode` and `edits`, which is necessary for the progressive edit session path.

**Requirements:** R5, R6, R7

**Dependencies:** U1

**Files:**
- Modify: `crates/agent/src/tools/streaming_edit_file_tool.rs`
- Test: `crates/agent/src/tools/streaming_edit_file_tool.rs`

**Approach:**
- Add direct `serde_json::from_value` tests for `StreamingEditFileToolPartialInput`.
- Cover double-encoded `mode`, stringified partial `edits`, omitted optionals, and explicit `null` optionals.

**Patterns to follow:**
- Existing partial-input startup behavior in `StreamingEditFileTool::run`.

**Test scenarios:**
- Happy path: partial input with `mode: "\"edit\""` and stringified partial edits deserializes with `Some(Edit)` and partial edit text.
- Edge case: partial input with only path/description and no `mode` or `edits` keeps both fields `None`.
- Edge case: partial input with `mode: null` and `edits: null` keeps both fields `None`.

**Verification:**
- The partial parser accepts malformed-but-recoverable snapshots instead of falling back to `DEFAULT_UI_TEXT`/ignored partial behavior.

---

## System-Wide Impact

- **Interaction graph:** Only serde input parsing for the streaming edit file tool changes; downstream edit session logic should receive the same enum and edit vector values it already expects.
- **Error propagation:** Invalid stringified JSON should continue to produce a serde error with context instead of silently accepting bad data.
- **State lifecycle risks:** None expected; no edit application state is changed.
- **API surface parity:** This improves compatibility with upstream Zed's current agent edit-tool contract.
- **Integration coverage:** Direct deserialization tests cover the contract boundary where the failure occurs.
- **Unchanged invariants:** Authorization, path resolution, diff generation, file mutation, and rejection behavior are intentionally unchanged.

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| The generic helper accidentally changes native deserialization behavior | Include native and stringified cases in focused tests, and preserve existing field defaults |
| Optional `null` handling regresses for partial inputs | Cover omitted and explicit `null` optionals in tests |
| Error messages become less specific than the old edits-only helper | Use a clear generic parse failure message because the helper now applies to multiple fields |

---

## Documentation / Operational Notes

- No user-facing documentation changes are required.
- PR release notes should be user-facing because this can fix agent edit failures.

---

## Sources & References

- Related upstream PR: `https://github.com/zed-industries/zed/pull/55500`
- Related upstream source PR: `https://github.com/zed-industries/zed/pull/55498`
- Related code: `crates/agent/src/tools/streaming_edit_file_tool.rs`
- Related local plan: `docs/plans/2026-05-03-001-feat-zed-1-0-next-edit-acp-sync-plan.md`
