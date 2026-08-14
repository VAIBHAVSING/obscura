# Packet C1: portable CDP state and dispatch core

## Goal

Move CDP target/session/domain semantics behind a portable WASM ABI. Node owns
the socket and async action execution, but WASM owns browser contexts, pages,
targets, sessions, loader IDs, execution-context IDs and event ordering.

## Dependencies

- N1 stable page/navigation identity.
- Page evaluation, screenshot and PDF bridges.
- N2/N3 request/event model for Network and Fetch domains.

## Design

Do not compile the Tokio server in `obscura-cdp` to WASM. Extract or re-express
its target-neutral protocol/state logic as a shared core. The host contract is
action based:

```text
cdpCommand(connectionId, requestJson)
  -> immediate response/events
  -> optional host actions (evaluate, navigate, screenshot, PDF, IO stream)

completeCdpAction(actionId, resultJsonOrBytes)
  -> response/events

pollCdpEvents(connectionId, maxItems)
```

## Small tasks

### C1.1. Inventory Playwright handshake

Record the exact command/event sequence from the existing native server and a
current Playwright client. At minimum include Browser, Target, Page, Runtime,
Network, Fetch, DOM, Emulation and IO calls made before and during the required
smoke flow. Make this a source-locked fixture, not a prose-only list.

### C1.2. Extract protocol-neutral types

- Request ID, method, params and optional session ID.
- Response result/error and session routing.
- Event queue with bounded count/bytes.
- Target info always including `canAccessOpener`.
- Monotonic, non-reused target/session/context/loader/action IDs.
- Connection cleanup and target ownership.

### C1.3. Context/target/session domains

- Browser contexts create/dispose/list.
- Targets create/close/list/discover.
- Auto-attach and explicit flattened attachments.
- Multiple sessions per target without ID collision.
- Browser target attachment.
- Correct detach/destroy event routing.

### C1.4. Runtime/Page action dispatch

- Runtime enable/evaluate/callFunctionOn/releaseObject.
- Execution context creation/clear after navigation.
- Isolated worlds with monotonic IDs.
- Page enable/navigate/reload/lifecycle/history.
- Screenshot and printToPDF actions.
- Preload scripts and new-document execution.

Remote object handles must be page/realm scoped, bounded and released. A new
document invalidates old handles.

### C1.5. Network/Fetch/IO domains

- Navigation, resource, fetch and XHR events share request/loader IDs.
- Response bodies are bounded and streamable through opaque IO handles.
- Fetch interception delegates to N3.
- Abandoned streams are evicted and freed.

### C1.6. DOM/Input/Emulation minimum surface

Port the methods observed in the Playwright handshake and required smoke tests.
Use existing native domain code as the semantic reference. Input events must
enter WASM/page state, not be emulated with ad hoc page-realm JS when a native
DOM path exists.

### C1.7. ABI and tests

- Exact `cdpAbiVersion` and fail-closed capability probe.
- Command/input and event/output byte/count limits.
- Malformed JSON and unknown method return CDP-shaped errors.
- Stale action completion, session and page IDs are rejected.
- Connection close frees targets/actions/streams.
- Parity tests feed the same request corpus into native and portable cores and
  compare normalized responses/events.

## Acceptance criteria

- WASM is the source of truth for CDP state and event order.
- Node can remain a thin JSON/bytes/action transport.
- The recorded Playwright handshake completes without hardcoded one-off
  responses.

## Copyable LLM prompt

```text
Implement one C1 task from .agent/files/remaining/08-portable-cdp-core.md.
Read AGENTS.md and the existing obscura-cdp domain implementation first. Do
not attempt to compile the Tokio server to WASM. Extract target-neutral state
and express async work as opaque host actions. Preserve strict CDP fields,
session routing and lifecycle ordering. Add native/portable parity tests. Do
not touch docs/README/.agent/index.js or commit unless told.
```
