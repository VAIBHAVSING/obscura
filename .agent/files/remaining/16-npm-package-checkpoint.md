# Portable npm package checkpoint

## Scope

This packet records the first publishable Node-only adapter milestone. The
package is `@obscura/browser` and its runtime accepts a wasm-bindgen wrapper,
not the native CLI or a `.node` addon. Page JavaScript runs in Node's existing
V8 `vm.Context`; browser DOM, navigation state, render, screenshot and PDF
operations cross the bounded WASM bridge.

## Source checkpoint

- Package checkpoint commit: `8538aa0` (`feat: add portable WASM npm browser package`)
- Cookie/storage CDP checkpoint: `a2df759` (`feat: expose portable cookies through package CDP`)
- Branch: `wasm-node-migration`
- Package source: `node/obscura/`
- Pushed to `origin/wasm-node-migration`.

The package contains an RFC 6455 WebSocket server, package-owned CDP subset,
Node Worker lifecycle, ESM API, TypeScript declarations, and the
`obscura-browser` npx bin. Native `.node` paths fail with
`ERR_OBSCURA_NATIVE_UNSUPPORTED`.

## Recorded gates

- Package mock suite: 5 passed, 2 optional artifact skips.
- Real-WASM package suite: 7 passed, 0 skipped, 0 failed.
- Real Playwright `connectOverCDP` against the current WASM wrapper: passed
  context/page creation, `goto(..., waitUntil: "load")`, title and locator
  text, `page.evaluate`, PNG screenshot signature, and PDF signature.
- Direct real-WASM CDP cookie round trip: `Network.setCookies`,
  `Network.getAllCookies`, and `Network.deleteCookies` passed with the cookie
  absent after deletion. Playwright cookie coverage is present and requires a
  supplied `playwright-core` module at test time.
- Fresh tarball install with `npm install --ignore-scripts`: passed.
- Fresh tarball `npx --no-install obscura-browser`: version, eval, screenshot,
  and PDF passed.
- Tarball scan: 18 files, no `.node`, ELF/Mach-O/PE binary, native executable,
  Cargo target, or absolute workspace path.

## Known boundary

This is a usable portable package milestone, not proof of full Chromium or
full Playwright parity. The CDP state/session implementation is currently in
the Node adapter, the WASM ABI still needs to own the complete CDP domain
state, and broader browser behavior remains: full network interception/cache
 policy, dynamic import, complete XHR/events, input/actions, storage/cookies
 beyond the current basic CDP round trip, iframe/worker targets, and complete
 option/domain matrices.

The companion `obscura-benchmark` obstacle course is unavailable in this
workspace and must not be reported as 33/33. Final repository render gates,
security/performance review, and release evidence remain separate work.

## Reproduction

Use a current wasm-bindgen output directory outside the repository:

```bash
cd node/obscura
OBSCURA_WASM_BINDGEN_DIR=/path/to/wasm-bindgen-output npm pack
```

Then install the tarball in a clean temporary project with scripts disabled
and invoke `npx --no-install obscura-browser ...`. Do not stage generated
`node/obscura/src/runtime/`, `node/obscura/bootstrap.js`, or
`node/obscura/wasm/`; they are prepared for the tarball and ignored.
