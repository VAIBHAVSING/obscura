# Packet S1: classic script loading and execution

## Goal

Execute document classic scripts in Node's V8 with browser-correct ordering
while all parser/document state remains in WASM.

## Dependencies

- N1 document commit state.
- N2 host fetch.
- Existing bootstrap and task runtime.

## Small tasks

### S1.1. Define parser-script action ABI

WASM emits data-only actions describing:

- inline source and source URL
- external URL and request metadata
- parser-blocking, async, defer or dynamically inserted class
- type/language/nomodule eligibility
- integrity/crossorigin/referrer policy when supported
- document generation and monotonic script ID

Host reports fetched/compiled/executed/error outcomes using only primitive
records. A stale generation is rejected.

### S1.2. Parser-blocking execution

- Pause parser progression at a blocking script.
- Fetch external source through N2.
- Evaluate in the document's existing `vm.Context` under the watchdog.
- Flush microtasks and mutation effects before parser resumes.
- Support `document.write` only if the existing native behavior is preserved;
  otherwise fail explicitly and add it as a separately tracked gap.

### S1.3. Async and defer queues

- Async executes when fetched, serialized with other page work.
- Defer preserves document order and runs after parse, before
  `DOMContentLoaded`.
- Failed scripts dispatch error without blocking later eligible scripts.
- Reset/cancel invalidates queued fetches and execution.

### S1.4. Dynamic classic scripts

- Observe script insertion/src/text mutation through existing DOM hooks.
- Start each eligible script exactly once.
- Dispatch load/error in the proper task queue.
- Preserve `async=false` ordering for ordered dynamic scripts.

### S1.5. Realm-safe evaluator

- Source text enters through a quoted data binding, never string-concatenated
  host code.
- Result is ignored for script elements.
- Errors are sanitized inside the VM timeout boundary.
- Stack/sourceURL information is retained without leaking Node paths/globals.
- Infinite compile/evaluate/getter work is bounded and the Worker remains
  reusable or is deterministically terminated.

### S1.6. Tests

- Inline order and DOM visibility.
- External blocking, defer and async ordering under controlled delays.
- DOMContentLoaded/load timing.
- Dynamic insertion, duplicate start prevention and load/error events.
- `document.currentScript` if supported by native bootstrap.
- Navigation during fetch/execution.
- Thrown and infinite scripts; later page reuse.
- No `process`, `require`, `Buffer` or host callback escape.

## Acceptance criteria

- A deterministic framework-style fixture builds its DOM from external and
  inline scripts using Node V8.
- Script ordering matches the native engine's tested behavior.
- No script executes for a replaced document.

## Copyable LLM prompt

```text
Implement one S1 task from
.agent/files/remaining/06-classic-script-orchestration.md. Read AGENTS.md,
bootstrap.js and native Page script orchestration first. Node V8 evaluates
source, but WASM owns parser/script/lifecycle state. Keep every source and error
inside the bounded vm execution path. Add deterministic ordering and reset
tests. Do not touch docs/README/.agent/index.js or commit unless told.
```
