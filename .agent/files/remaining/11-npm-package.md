# Packet K1: publishable npm API and artifact layout

## Goal

Create the final Node 22+ package, tentatively `@obscura/browser`, containing
only JavaScript, type declarations, package metadata and the compiled WASM
payload.

## Dependencies

- Real CDP/WebSocket smoke passes.
- Stable Worker/WASM/browser lifecycle.

## Small tasks

### K1.1. Freeze public API

Recommended API:

```ts
launch(options?): Promise<ObscuraBrowserServer>
connect(endpoint, options?): Promise<ObscuraConnection>
version(): string
```

`ObscuraBrowserServer` exposes `httpEndpoint()`, `wsEndpoint()`, `processInfo`
without pretending a native child process exists, and idempotent `close()`.
Provide ESM and CJS entrypoints if support can be tested without duplicating
runtime state.

### K1.2. Package layout

```text
package.json
dist/index.js
dist/index.cjs        optional, if tested
dist/index.d.ts
dist/worker/*.js
dist/wasm/obscura_wasm.js
dist/wasm/obscura_wasm_bg.wasm
bin/obscura-browser.js
LICENSE
```

Worker and WASM URLs must resolve using `import.meta.url`/package-local paths,
not cwd or `/workspaces` paths.

### K1.3. Artifact loading

- Prefer a package-local wasm-bindgen wrapper or explicit
  `WebAssembly.instantiate` bootstrap.
- Load once per Worker and reuse compiled module where Node permits.
- Fail with an actionable error on missing/corrupt/incompatible WASM.
- Verify maximum memory and ABI before accepting ready.
- Do not download binaries at install or runtime.

### K1.4. Options and lifecycle

- host/port, timeouts, memory/resource limits, private-network/file access,
  logging and Worker count.
- Secure defaults: loopback bind, private/file blocked.
- Idempotent close and signal handling only when launched through CLI.
- Library import must not install process-wide signal handlers or call
  `process.exit`.

### K1.5. Tarball tests

- `npm pack` into a disposable directory.
- Inspect tar listing and reject `.node`, ELF/Mach-O/PE, native CLI, source
  targets, absolute paths and unexpected large artifacts.
- Install tarball into fresh ESM and CJS fixtures with scripts disabled.
- Launch, use CDP, navigate, evaluate, screenshot, PDF and close.
- Test directory names containing spaces and non-ASCII characters.

## Acceptance criteria

- `npm install` runs no compiler and no postinstall download.
- Package works after the repository and build target are unavailable.
- Public typings match runtime behavior.
- Package contents prove native independence.

## Copyable LLM prompt

```text
Implement one K1 task from .agent/files/remaining/11-npm-package.md. The package
must be relocatable and contain only JS, declarations and WASM. It may not
shell out to Obscura, load a .node addon, download at install, or rely on a
/workspaces path. Test the packed tarball from a fresh temporary project with
scripts disabled. Do not edit project README/docs/.agent/root index.js or
commit unless told.
```
