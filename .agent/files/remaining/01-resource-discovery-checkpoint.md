# Packet R: portable resource discovery and seeding checkpoint

## Goal

Complete the live page resource loop needed by WASM paint:

```text
WASM discovers image/font requests
  -> Node fetches with the specified request profile
  -> Node seeds bytes or a missing outcome
  -> WASM layout/paint consumes the cache
  -> screenshot pixels prove that the resource was used
```

This packet is shipped in source commit `f248863`, with verification recorded
in `.agent/files/wasm-node-migration-memory.md`. Preserve that implementation
and continue from the next packet.

## Dependencies

None. This is the next checkpoint and blocks navigation and PDF evidence.

## Exclusive file ownership

Primary Rust owner:

- `crates/obscura-render/src/paint.rs`
- `crates/obscura-render/src/lib.rs`
- `crates/obscura-js/src/lib.rs`
- `crates/obscura-js/src/ops.rs` if shared base resolution is wired there
- `crates/obscura-browser/src/page.rs`
- `crates/obscura-wasm/Cargo.toml`
- `crates/obscura-wasm/src/lib.rs`
- `Cargo.lock`

Primary Node owner, only after Rust ABI is frozen:

- `node/wasm-v8-harness/src/client.mjs`
- `node/wasm-v8-harness/src/limits.mjs`
- `node/wasm-v8-harness/src/worker.mjs`
- `node/wasm-v8-harness/test/fixtures/mock-stateful-core.cjs`
- resource-related Node tests

## Small tasks

### R1. Make the CSS scanner typed and panic-free

- Replace filename-suffix classification with scanner context.
- Emit `CssResourceKind::Font` for every `url(...)` inside `@font-face`, even
  when the URL is extensionless or query-described.
- Emit `Image` outside `@font-face`, even when an image URL ends in `.woff2`.
- Preserve comment, quoted-string, escape and `@import` handling.
- Never slice a UTF-8 string at an unchecked byte boundary.
- Add tests for extensionless fonts, misleading suffixes, nested blocks,
  comments/strings and non-ASCII CSS before `@font-face`.

Acceptance: the scanner's typed results are independent of URL suffixes and no
valid UTF-8 input can panic.

### R2. Share correct document-base resolution

- Use the first `<base href>` only.
- Resolve it against the document URL.
- If it is invalid, fall back to the document URL rather than returning no
  base.
- Ignore later base elements even if the first is invalid.
- Use one target-neutral helper from native browser resource loading, native
  JS ops and the WASM renderer.
- Test relative, absolute, invalid-first, empty and multiple-base cases.

Acceptance: native and WASM resolve identical resource URLs.

### R3. Keep the non-render WASM feature lean

- Make the direct `url` dependency optional.
- Enable it only through the `render` feature.
- Verify the default `obscura-wasm` graph does not gain render dependencies.

### R4. Freeze resource ABI v1

- Discovery returns bounded pages of deterministic, deduplicated requests.
- Each request has exact `url`, `kind` and optional image `profile` fields.
- Supported profiles remain `no-cors-include`, `cors-same-origin`, and
  `cors-include`.
- Cursor progression is stable when requests are seeded between pages.
- `set_html` clears outcomes without recycling page/node identity.
- Seed and missing calls use generation/document/revision preconditions.

### R5. Harden Node page validation

- Require `requests.length <= nextOffset - offset`.
- A non-final page must consume exactly the requested `limit`.
- Reject duplicates, invalid kinds/profiles, oversize URLs, partial
  capabilities, Promise-returning WASM calls and stale identities.
- Copy resource bytes so caller mutation cannot alter in-flight input.

### R6. Prove successful seed-to-paint behavior

- Build the real release WASM module with `--features render`.
- Generate a disposable Node wasm-bindgen wrapper outside the repository.
- Load HTML referencing a known small PNG using a non-default CORS profile.
- Discover the request, seed valid bytes, capture a screenshot and record it.
- Replace the document, seed the same request as missing, capture again.
- Assert both PNGs are structurally valid and their bytes/pixels differ.
- Also cover font discovery and a seeded font affecting measurable render
  output when a deterministic fixture is available.

### R7. Checkpoint and evidence

- Run focused renderer, WASM, browser and Node tests.
- Commit all source/tests as `feat: discover render resources in portable WASM`.
- Push the source commit.
- Update only `.agent/files/wasm-node-migration-memory.md` with the commit,
  commands, counts, artifact sizes/hashes and unavailable gates.
- Commit that file as `chore: record portable resource discovery evidence` and
  push.

Never stage root `index.js`, generated wrappers or screenshots.

## Focused verification

```bash
/workspaces/.obscura-tools/nextest/cargo-nextest nextest run --release \
  -p obscura-render --features native-resource-loader

/workspaces/.obscura-tools/nextest/cargo-nextest nextest run --release \
  -p obscura-wasm --features render

/workspaces/.obscura-tools/nextest/cargo-nextest nextest run --release \
  -p obscura-browser --features render

CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo check --release \
  -p obscura-wasm --target wasm32-unknown-unknown --features render

npm test --prefix node/wasm-v8-harness
```

Run the Node artifact tests with `OBSCURA_REAL_WASM_MODULE` pointing at the
fresh wrapper, not an older package.

## Copyable LLM prompt

```text
Implement Packet R from
.agent/files/remaining/01-resource-discovery-checkpoint.md.
Read AGENTS.md and the current dirty diff first. Preserve the existing partial
implementation. Work only in the packet's explicitly owned files. Do not edit
README/docs/.agent or root index.js. First report the exact semantic contract
and tests you will use. Then implement the smallest listed task assigned to
you. Use nextest, not cargo test. Do not commit unless explicitly told. Return
changed files, exact commands/results, unresolved risks, and a diff review.
```
