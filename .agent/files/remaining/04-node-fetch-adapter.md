# Packet N2: Node fetch and byte-stream adapter

## Goal

Implement the only network executor used by the npm runtime. It consumes
validated host actions from the WASM navigation/resource state machine and
returns bounded response events. It must not expose Node's `fetch`, sockets,
headers or response objects to page JavaScript.

## Dependencies

- Packet N1 navigation ABI is frozen.
- Packet R resource request profiles are frozen.

## Small tasks

### N2.1. Define host request/response envelopes

Request fields:

- opaque request/navigation/page IDs
- method and normalized URL
- ordered headers
- bounded body bytes
- redirect mode and maximum redirects
- credentials/referrer/cache/resource type
- deadline and maximum response bytes

Response events:

- headers/status/final URL
- bounded body chunk
- complete
- categorized error or cancellation

Use data-only records. Never pass Node Request/Response/Error instances across
the Worker or VM boundary.

### N2.2. Implement Node executor

- Use Node 22 built-ins only for the first implementation.
- Maintain one `AbortController` per opaque request.
- Apply connect/headers/body/total deadlines.
- Stream and count response bytes instead of calling unbounded `arrayBuffer()`.
- Normalize headers without merging `set-cookie` incorrectly.
- Decompress only according to the chosen host contract; report decoded versus
  wire lengths consistently.
- Cancel everything on page reset, Worker close or hard deadline.

### N2.3. Implement schemes

- `http:` and `https:` through host fetch.
- `data:` in target-neutral code where possible.
- `about:` in the navigation state machine.
- `file:` disabled by default. If supported later, require explicit package
  configuration, normalized paths and a root allowlist.
- Reject unknown schemes deterministically.

### N2.4. Bridge render resources

- Automatically drain Packet R discovery pages.
- Fetch each unique URL once per cache/profile identity.
- Preserve image CORS/credentials profile.
- Seed success or missing outcomes into the exact page identity.
- Bound concurrency and total resource bytes per page.
- Repeat discovery after CSS bytes are parsed and after DOM/style mutations,
  with a finite fixed-point limit.

### N2.5. Cancellation and backpressure tests

- Abort before headers, during body and after superseding navigation.
- Slow headers/body deadlines.
- Oversize declared and chunked bodies.
- Malformed headers and network errors.
- Concurrent resource limit and queued cancellation.
- Worker close while a fetch is active.
- A stale response cannot seed a replacement document.

### N2.6. Deterministic fixture integration

Build a Node-local fixture server outside the page realm with endpoints for
redirects, delayed chunks, compression, cookies, CSS imports, images, fonts,
scripts and errors. Use it for real Worker/WASM tests; no public network should
be needed for acceptance.

## Acceptance criteria

- No page-controlled object reaches Node network code.
- Every request is cancellable and byte/deadline bounded.
- Navigation and render resources work through the same request identity and
  policy pipeline.
- A real local page with CSS/image/font resources renders correctly.

## Copyable LLM prompt

```text
Implement one N2 task from .agent/files/remaining/04-node-fetch-adapter.md.
Read AGENTS.md and the frozen N1/R ABIs first. Node is an untrusted-I/O adapter,
not the browser-state owner. Page objects/functions must never cross into host
fetch. Use Node 22 built-ins, explicit AbortController cleanup, byte limits and
deadlines. Work only in agreed Node adapter/tests. Do not touch docs/README/
.agent/index.js. Run Node syntax and focused tests; do not commit unless told.
```
