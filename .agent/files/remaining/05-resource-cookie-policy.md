# Packet N3: cookies, resource graph, cache, interception and network policy

## Goal

Preserve Obscura's browser semantics and security policy while Node executes
the actual network requests. Policy/state should be target-neutral and owned by
WASM wherever practical.

## Dependencies

- N1 navigation state.
- N2 fetch envelopes and cancellation.
- R resource discovery.

## Small tasks

### N3.1. Extract portable cookie jar semantics

- Audit `obscura-net/src/cookies.rs` and context storage behavior.
- Separate parsing, matching, expiry, domain/path, Secure, HttpOnly, SameSite
  and ordering from filesystem/time/network wrappers.
- Supply current time through a host value, never call unsupported WASM clocks.
- Add import/export APIs needed by CDP `Network.getAllCookies`,
  `setCookie(s)` and `deleteCookies`.

### N3.2. Redirect and rewrite policy

- Validate the initial URL, every redirect and every interception rewrite.
- Preserve SSRF blocking for loopback, RFC1918, link-local and metadata ranges
  unless explicitly enabled.
- Re-evaluate credentials/referrer/cookies after cross-origin redirects.
- Enforce redirect loop and count limits.

### N3.3. Request interception

- Model `Continue`, `Fulfill` and `Fail` in the portable state machine.
- Continue may rewrite URL/method/headers/body only after revalidation.
- Fulfill responses use the same response-size/MIME/encoding path as network
  responses.
- Passive request/response events must not stall the request.
- Add an opaque interception ID and deterministic timeout/default action.

### N3.4. Bounded response/resource cache

- Define keying across URL, method, credentials/CORS and relevant headers.
- Bound entry count and total bytes with deterministic eviction.
- Keep document resources separate from persistent HTTP cache semantics.
- Never cache failed/partial bodies as successful.
- Clear page-owned cache on document reset while preserving context-owned
  cookies/cache according to API.

### N3.5. Recursive stylesheet and font/image graph

- Fetch linked stylesheets and bounded recursive `@import` chains.
- Rebase stylesheet-relative URLs before discovering nested assets.
- Detect cycles and cap depth/count/total bytes.
- Preserve font versus image request identity from Packet R.
- Trigger rerender/lifecycle readiness when late resources arrive.

### N3.6. Tests

- Cookie domain/path/SameSite/Secure/expiry ordering.
- Redirect cookie changes and cross-origin credential stripping.
- SSRF checks on initial, redirect and rewritten URLs.
- Interception continue/fulfill/fail and timeout.
- Cache eviction, aliasing and reset isolation.
- CSS import cycles and resource budgets.

## Acceptance criteria

- Node never decides cookie, redirect, interception or SSRF semantics on its
  own.
- Contexts are isolated and deterministic.
- Resource completion feeds both rendering and network/CDP events.

## Copyable LLM prompt

```text
Implement one N3 task from
.agent/files/remaining/05-resource-cookie-policy.md. Read the existing native
cookie/network/interception code and preserve its observable behavior. Extract
target-neutral semantics; do not add sockets or Tokio to the WASM graph. Node
only executes already-approved requests. Add parity tests for native and WASM
where possible. Do not touch README/docs/.agent/root index.js or commit without
instruction.
```
