# Obscura Node/WASM browser migration: remaining master plan

## Mission

Ship a publishable Node 22+ package in which the browser engine is a
`wasm32-unknown-unknown` module. Node is only the host adapter: it supplies its
existing V8 isolate, networking, timers, filesystem access, process limits,
and a WebSocket listener. The npm package must not execute or distribute the
native Obscura CLI, a `.node` addon, `deno_core`, `rusty_v8`, or a platform
specific Obscura library.

The required user experience is:

```js
import { launch } from "@obscura/browser";

const browser = await launch();
const playwrightBrowser = await chromium.connectOverCDP(browser.wsEndpoint());
const page = await playwrightBrowser.newPage();
await page.goto("https://example.com");
await page.screenshot({ path: "page.png" });
await page.pdf({ path: "page.pdf" });
await browser.close();
```

and:

```bash
npx @obscura/browser serve --port 0
npx @obscura/browser screenshot https://example.com page.png
npx @obscura/browser pdf https://example.com page.pdf
```

## Final architecture

```text
Playwright / Puppeteer / npm API / npx CLI
                    |
                    | CDP JSON over RFC 6455
                    v
Node host adapter (JavaScript only)
  - HTTP upgrade and WebSocket framing
  - Node Worker lifecycle and hard termination
  - vm.Context using Node's existing V8
  - fetch/socket/filesystem/timer adapters
  - copies bounded buffers across the WASM boundary
                    |
                    | versioned, bounded, batched ABI
                    v
Obscura WASM
  - browser/context/page state
  - navigation and lifecycle state machines
  - DOM and browser bootstrap contract
  - cookies, redirects, policy, interception semantics
  - style, layout, paint, screenshot and PDF
  - CDP target/session/domain state and event generation
```

Node must not expose host callbacks or Node globals directly to page code.
Page JavaScript runs in a fresh `vm.Context` per committed document. Host
callbacks cross through temporary, non-enumerable bindings and data-only
envelopes, following the existing harness security design.

## Current authoritative baseline

Pushed baseline: `c860841` on `wasm-node-migration`.

Already implemented and pushed:

- WASM parser, DOM, stable node handles, page revision and batched DOM ABI.
- Node Worker plus `vm.Context` page-JavaScript host.
- Production bootstrap DOM adapter and target-neutral platform operations.
- Timer, posted-task and microtask host scheduling.
- WASM layout, CPU paint and PNG screenshot output.
- ABI negotiation, byte limits, timeouts, deterministic release and stale-page
  protection.

Shipped in source commit `f248863` with verification recorded in
`.agent/files/wasm-node-migration-memory.md`:

- Target-neutral CSS resource scanning.
- WASM render-resource discovery and profiled seed/missing APIs.
- Node resource bridge and tests.
- Shared document-base resolution and typed font/image classification.

Additional shipped slices after that checkpoint:

- `5634135`: target-neutral navigation transactions and redirect/history state.
- `e02d105`: bounded classic document-script orchestration.
- `fbf7b34`: bounded Node fetch/XHR bridge for navigation and page realms.
- `0f3b0a8`: static ES modules, import maps and bounded module graph loading.
- `c860841`: target-neutral cookie state with Node request/document adapters.

These slices are real and tested, but they do not yet constitute a complete
CDP browser or a publishable npm package. Dynamic `import()` remains
fail-closed, XHR is a compatibility subset, and interception/cache/full SSRF
policy are not complete.

The user-owned untracked repository-root `index.js` is outside this migration.
Never edit, delete, stage or commit it.

## Completion boundary

The migration is complete only when all of the following are simultaneously
true:

1. A clean npm install contains JS, type declarations and WASM only.
2. No runtime command invokes a native Obscura executable or addon.
3. Node's V8 executes page scripts while the live DOM/render state remains in
   WASM.
4. HTTP navigation, redirects, cookies, CSS, fonts, images, classic scripts,
   module scripts, `fetch`, XHR and lifecycle events work through bounded host
   adapters.
5. Playwright `connectOverCDP`, context/page creation, navigation, evaluation,
   selectors, screenshots and PDFs pass against the packaged artifact.
6. The CDP server is package-owned and uses a JavaScript WebSocket transport.
7. Worker termination bounds hostile synchronous JavaScript and resource work.
8. The full repository gates required by `AGENTS.md` are recorded. The 33/33
   obstacle course may only be called passed when the companion repository is
   actually present and run.

This does not promise Chromium's GPU stack, DRM, extensions, WebRTC or every
web-platform API. Those are explicitly outside this migration unless added to
the acceptance suite later.

## Dependency graph

```text
R: resource discovery checkpoint
 |\
 | +--> P: portable PDF
 |
 +----> N1: navigation state ------> N2: Node fetch adapter
                                      |\
                                      | +--> N3: resources/cookies/policy
                                      |
                                      +----> S1: classic scripts
                                               |
                                               +--> S2: modules/fetch/XHR
                                                        |
                                                        v
                         C1: portable CDP state <------ browser-complete page
                                      |
                         C2: JS WebSocket transport
                                      |
                         C3: Playwright domain parity
                                      |
                         K1: npm API --> K2: npx CLI
                                      |
                         H: hardening/performance
                                      |
                         V: final verification/release
```

## Work packet index

| Packet | File | May start when |
| --- | --- | --- |
| R | `01-resource-discovery-checkpoint.md` | now |
| P | `02-portable-pdf.md` | R source is stable |
| N1 | `03-navigation-state-machine.md` | R source is stable |
| N2 | `04-node-fetch-adapter.md` | N1 ABI is agreed |
| N3 | `05-resource-cookie-policy.md` | N1 and N2 |
| S1 | `06-classic-script-orchestration.md` | N1 and N2 |
| S2 | `07-modules-fetch-xhr.md` | S1 |
| C1 | `08-portable-cdp-core.md` | N1 and stable page ABI |
| C2 | `09-javascript-websocket-server.md` | C1 command envelope |
| C3 | `10-playwright-compatibility.md` | C1 and C2 |
| K1 | `11-npm-package.md` | real packaged CDP smoke works |
| K2 | `12-npx-cli.md` | K1 |
| H | `13-security-performance.md` | continuous; final after C3 |
| V | `14-final-verification-release.md` | all implementation packets |
| A | `15-multi-agent-operating-guide.md` | use for every delegation |

## Repository-wide rules for every packet

- Read `/workspaces/obscura/AGENTS.md` before acting.
- Never edit project README or `docs/**` for this migration.
- Never touch the user-owned root `index.js`.
- Do not use the native CLI or `.node` addon as an npm runtime dependency.
- Do not compile V8 into WASM. Page JS belongs to Node's existing V8.
- Use `cargo nextest`, never `cargo test`.
- Use the custom executable when `cargo nextest` is unavailable:
  `/workspaces/.obscura-tools/nextest/cargo-nextest nextest run ...`.
- Do not bulk-run `cargo fmt`; format only changed snippets if required.
- Generated artifacts, screenshots and reports belong outside the repository.
- Every ABI must advertise an exact version and fail closed on partial or
  mismatched capabilities.
- Validate limits before allocating/copying whenever the host boundary allows.
- No Rust panic may unwind across wasm-bindgen.
- One agent owns a file at a time. Review agents stay read-only.
- Do not commit another agent's incomplete work. Commit by coherent milestone.

## Status notation

- `TODO`: no implementation has been accepted.
- `ACTIVE`: one named owner is editing the packet.
- `REVIEW`: implementation exists and is undergoing independent review.
- `VERIFIED`: focused real-artifact acceptance passed.
- `SHIPPED`: committed and pushed with evidence recorded.

Current packet states:

- R `SHIPPED` (`f248863`), P `SHIPPED` (`ab36f8e`).
- N1 `SHIPPED` (`5634135`), N2 `SHIPPED` (`fbf7b34`).
- N3 `REVIEW`: cookie semantics are shipped (`c860841`), but interception,
  cache, redirect credential policy, recursive resource completion and full
  SSRF/DNS checks remain.
- S1 `SHIPPED` (`e02d105`) for the bounded classic-script subset.
- S2 `REVIEW`: static modules/import maps and basic fetch/XHR are shipped
  (`0f3b0a8`), while dynamic import, complete XHR parity and event parity
  remain.
- C1, C2, C3, K1 and K2 `TODO`: portable CDP state, JavaScript WebSocket
  transport, Playwright compatibility, npm packaging and npx CLI are not
  implemented yet.
- H `ACTIVE` continuously; V `TODO` until C1 through K2 are complete.
