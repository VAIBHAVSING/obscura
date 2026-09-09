# Packet S2: modules, dynamic import, fetch and XHR

## Goal

Complete the major JavaScript-driven network surfaces after classic scripts
work. All requests use the same N2/N3 pipeline and emit consistent lifecycle
and CDP Network events.

## Dependencies

- S1 classic script execution.
- N2 fetch and N3 cookies/policy/interception.

## Small tasks

### S2.1. Import maps

- Reuse existing bootstrap/native import-map parsing semantics.
- Register eligible maps before dependent module graphs start.
- Resolve bare, relative and absolute specifiers with the document base URL.
- Bound map bytes, entry count and resolution depth.

### S2.2. Module graph loader

- Build a per-document module map keyed by canonical URL and type.
- Fetch through the common request pipeline.
- Compile/link/evaluate with Node `vm.SourceTextModule` if available under the
  supported Node 22 contract, or a vetted equivalent.
- Handle cycles, single evaluation, parse/link/runtime errors and top-level
  await without leaking host promises into the page realm.
- Cancel the graph on navigation/reset.

### S2.3. Dynamic import

- Install the VM dynamic-import hook when creating the page realm.
- Route it into the same resolver/module map.
- Return a page-realm Promise and namespace object.
- Apply deadlines to fetch/link/evaluate phases.

### S2.4. Window fetch

- Convert bootstrap `op_fetch_url` requests into N2/N3 actions.
- Expose page-realm `Response`, headers, body readers and errors without
  passing Node objects into the realm.
- Preserve cookies, redirect mode, credentials, abort and interception.
- Bound buffered body APIs; stream APIs require explicit backpressure.

### S2.5. XMLHttpRequest

- Route through the same request pipeline.
- Implement ready-state/event order, response types, abort, timeout and header
  behavior required by existing native tests.
- Ensure synchronous XHR is rejected or implemented consistently; never block
  the Worker indefinitely.

### S2.6. Tests

- Static imports, cycles, duplicate imports and errors.
- Import maps and dynamic import.
- Top-level await ordering and navigation cancellation.
- Fetch redirects/cookies/abort/body limits.
- XHR ready states, progress/error/abort/timeout.
- Network event parity across navigation, resource, fetch and XHR requests.

## Acceptance criteria

- A local ESM fixture and a fetch/XHR application run entirely through the
  packaged Node/WASM runtime.
- No native V8/module loader or native network client appears in the package.

## Copyable LLM prompt

```text
Implement one S2 task from .agent/files/remaining/07-modules-fetch-xhr.md.
Audit the current native module loader/ops and preserve behavior. Use Node V8
only through the existing Worker realm and the common bounded network adapter.
Do not pass Node Promise/Response/Error objects into page JS. Provide focused
tests with a deterministic local fixture. Do not edit docs/README/.agent/
index.js or commit without instruction.
```
