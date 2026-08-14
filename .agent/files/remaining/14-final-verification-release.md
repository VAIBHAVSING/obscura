# Packet V: completion audit, verification and release evidence

## Goal

Prove the requested migration against current source and the final packed npm
artifact. Passing narrow unit tests is not enough.

## Dependencies

All implementation packets complete and reviewed.

## Small tasks

### V1. Clean source and dependency audit

- `git status` contains no unintended generated/user files.
- `git diff --check` passes.
- No project README/docs changes from this migration unless the user later
  requests them.
- `cargo tree` for `obscura-wasm --target wasm32-unknown-unknown --features
  render` contains no `deno_core`, `v8`, Tokio network stack, `mio`, `ureq`,
  `ring`, `rustls`, native TLS, native font or platform window dependency.
- Search final npm JS for native CLI/addon spawning/loading and absolute build
  paths.

### V2. Focused release tests

Run release nextest for every affected Rust crate and focused Node suites for
each ABI. Record exact executed/pass/skip/fail counts and run IDs. Never call a
skipped real-artifact test a pass.

### V3. Real WASM build and wrapper

```bash
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release \
  -p obscura-wasm --target wasm32-unknown-unknown --features render
```

Generate the exact wrapper used by the npm package. Record raw/wrapper/WASM
sizes and SHA-256. Inspect WASM imports, exports and memory maximum. Reject any
unexpected host/native import.

### V4. Full repository gates from AGENTS.md

```bash
/workspaces/.obscura-tools/nextest/cargo-nextest nextest run --release \
  --features render --no-fail-fast

CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release \
  -p obscura-cli --bins --features render
```

These are regression gates only. The resulting native CLI must not enter the
npm package or runtime.

If `/workspaces/obscura-benchmark` exists, run the authoritative obstacle
course and require 33/33. If absent, record it as unavailable, never passed.

For render changes, run deterministic fixture and representative top/bottom
captures as instructed by AGENTS.md into a disposable external directory.

### V5. Packed artifact audit

- Build with a reproducible package script.
- `npm pack` and hash the tarball.
- Inspect every entry.
- Install with `--ignore-scripts` in a fresh directory.
- Move/remove the source repository and build outputs from resolution scope.
- Run npm API, npx and Playwright tests.
- Assert no child process executes an Obscura native binary.

### V6. Required product flow

Against the tarball only:

1. launch on port 0;
2. Playwright connect over CDP;
3. create context/page;
4. navigate a deterministic local framework/resource fixture;
5. evaluate DOM-changing JavaScript;
6. use locators and input;
7. verify cookies/fetch/XHR/module behavior;
8. capture and validate nonblank PNG;
9. capture and validate PDF;
10. close and prove no Worker/socket remains.

Repeat under concurrency and lifecycle stress.

### V7. Performance and cold-start evidence

- At least 25 rotated fresh-process samples for package launch/ready and first
  completed page task.
- Warm persistent navigation/evaluate/screenshot/PDF samples.
- RSS/peak memory with clear process topology.
- No outlier removal without disclosure.
- Do not claim a speedup without an identical workload and noise-aware result.

### V8. Evidence commit and release decision

Update `.agent/files/wasm-node-migration-memory.md` only after all output is
final. Include:

- exact implementation commit
- commands and counts
- artifact/tarball hashes and sizes
- Node/Playwright versions
- architecture/native-independence audit
- benchmark method/distributions
- unavailable or intentionally out-of-scope items

Commit evidence separately and push. Mark the migration complete only if every
completion item in `00-master-plan.md` has direct current evidence.

## Copyable LLM prompt

```text
Perform one read-only V task from
.agent/files/remaining/14-final-verification-release.md against the current
HEAD and final npm tarball. Do not rely on prior memory or stale artifacts.
Capture exact commands, versions, hashes, counts and skips. Do not edit source,
docs, README, .agent or root index.js. Treat missing/indirect evidence as not
proved. Report a requirement-by-requirement verdict and exact remaining gates.
```
