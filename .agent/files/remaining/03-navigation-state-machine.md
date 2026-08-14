# Packet N1: portable page and navigation state machine

## Goal

Move browser/page navigation semantics into a target-neutral state machine
owned by WASM. Node performs I/O but must not become the source of truth for
URL, loader, history, document generation or lifecycle ordering.

## Dependencies

- Packet R source ABI is stable.
- Existing DOM/page revision and metadata ABIs remain authoritative.

## Core principle

A navigation is an explicit transaction with opaque IDs and bounded messages:

```text
beginNavigation(url, options)
  -> host actions (fetch request, cancel previous work)
hostResponseHeaders(...)
hostResponseChunk(...)
hostResponseEnd(...)
  -> commit decision + new document generation
  -> resource/script actions
  -> lifecycle events
```

WASM validates state transitions. Node may not invent a committed URL,
loader ID or lifecycle event.

## Small tasks

### N1.1. Inventory native page semantics

- Trace `obscura-browser::Page`, context, navigation, redirects, history,
  loader IDs and lifecycle events.
- Produce a testable transition table before editing.
- Identify target-neutral code versus Tokio/network/V8 wrappers.
- List exact current quirks that Playwright depends on.

### N1.2. Define navigation ABI v1

Define versioned JSON or compact binary envelopes for:

- begin/cancel navigation
- host response headers/chunks/end/error
- commit document
- poll host actions
- poll lifecycle/CDP events
- same-document navigation and history traversal
- page close/reset

Every message carries browser-context ID, page ID, navigation ID, document
generation and loader ID where applicable. IDs are monotonic and never reused
within a Worker.

### N1.3. Implement target-neutral state

- Page URL, referrer, encoding, title and origin.
- Pending/current navigation and abort reason.
- Redirect chain and final response metadata.
- Monotonic frame/loader/navigation/document identities.
- History entries and same-document fragment changes.
- Deterministic cancellation when a newer navigation begins.
- `about:blank` and bounded `data:` URL commits without host fetch.

### N1.4. Parse and commit HTML

- Decode response bytes using shared encoding operations.
- Apply MIME/charset rules needed by current Obscura behavior.
- Parse into a replacement DOM only after the navigation is commit-ready.
- Invalidate old node handles/realm/tasks/resources exactly once.
- Set document metadata before bootstrap initialization.
- Emit commit/lifecycle actions in a source-locked order.

### N1.5. Expose Worker/client page API

Proposed high-level host methods:

```text
createPage(contextOptions)
navigate(pageId, url, options)
reload(pageId)
goBack(pageId)
goForward(pageId)
closePage(pageId)
pageStatus(pageId)
```

Start with one page per Worker if necessary, but make page identity explicit so
the CDP layer can later pool Workers without changing semantics.

### N1.6. Transition tests

- about:blank, data URL and HTTP success.
- redirect success, loops and maximum redirects.
- navigation replacement and cancellation races.
- response error before and after headers.
- empty body and unsupported MIME behavior.
- encoding, title, base URL and referrer.
- same-document hash changes and history.
- lifecycle ordering and exactly-once document reset.
- stale host messages rejected after cancel/reset/close.

## Acceptance criteria

- Node can drive navigation only through host actions emitted by WASM.
- The committed page state survives host polling and cannot be forged by a
  stale response.
- Real HTML navigation produces a new Node V8 realm connected to the new WASM
  document.
- Replacement and cancellation leak no task, resource or callback.

## Copyable LLM prompt

```text
Perform one numbered N1 task from
.agent/files/remaining/03-navigation-state-machine.md. Read AGENTS.md and first
audit the existing native Page implementation relevant to your task. Browser
state belongs in target-neutral Rust/WASM; Node is only an I/O and V8 adapter.
Do not introduce a native runtime dependency or duplicate semantics in JS.
State the transition contract and file ownership before editing. Add focused
nextest coverage. Do not touch README/docs/.agent/root index.js and do not
commit unless told.
```
