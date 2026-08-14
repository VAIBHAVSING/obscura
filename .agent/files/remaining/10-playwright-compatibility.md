# Packet C3: Playwright compatibility and browser-facing parity

## Goal

Make current Playwright connect through `chromium.connectOverCDP()` and perform
the required browser workflow against the real packaged WASM backend.

## Dependencies

- C1 portable CDP core.
- C2 WebSocket server.
- Navigation/scripts/resources/screenshots/PDF complete.

## Required end-to-end flow

```js
const browser = await chromium.connectOverCDP(httpEndpoint);
const context = browser.contexts()[0] ?? await browser.newContext();
const page = await context.newPage();
await page.goto(fixtureUrl, { waitUntil: "load" });
await page.locator("h1").waitFor();
await page.evaluate(() => document.body.dataset.ready);
await page.screenshot({ path: pngPath, fullPage: true });
await page.pdf({ path: pdfPath, printBackground: true });
await page.close();
await context.close();
await browser.close();
```

## Small tasks

### C3.1. Record and minimize handshake failures

- Run Playwright with protocol debug logs against the package server.
- Record each unsupported command and missing/misordered event.
- Fix semantics in C1, not with client-name-specific transport hacks.
- Add every fixed interaction to a deterministic replay test.

### C3.2. Browser/context/page lifecycle

- Default context discovery.
- Incognito context create/dispose.
- Multiple pages and deterministic target attachment.
- Popup/opener fields if exercised.
- Close and disconnect without leaked Workers.

### C3.3. Runtime and locator flow

- Utility isolated world recreation after navigation.
- Remote object properties, function calls, promises and disposal.
- DOM query/resolve paths used by locators.
- Console and exception events.
- Evaluation timeout and post-timeout recovery.

### C3.4. Navigation/waiters

- `domcontentloaded`, `load` and network-idle policy.
- Redirect response chain.
- Failed/canceled navigation.
- Reload, back, forward and same-document navigation.
- Concurrent waiter registration without missed events.

### C3.5. Input and page actions

- click, fill, type, press, focus and select as exercised by Playwright.
- Bounding boxes/viewport/scroll through WASM layout.
- Form submission and resulting navigation.
- File chooser/upload only if it can be implemented within the configured Node
  file-access policy; otherwise track it explicitly outside the base gate.

### C3.6. Screenshot/PDF API options

- viewport and full-page screenshot.
- clip, scale/device scale and transparent background if supported.
- PDF paper/margins/landscape/scale/page ranges/background.
- Reject unsupported options with stable errors rather than silently corrupting
  output.

### C3.7. Concurrency and isolation

- At least two contexts and four pages.
- One infinite script does not poison peers.
- One slow navigation can be canceled while peers complete.
- Cookie/storage/resource/realm separation.
- Repeated 100 create/navigate/evaluate/close cycles.

## Acceptance criteria

- The required end-to-end flow passes against the final npm tarball, not the
  source tree alone.
- No test sets a native addon or native CLI environment variable.
- Protocol replay covers the accepted Playwright handshake.

## Copyable LLM prompt

```text
Take one C3 task from
.agent/files/remaining/10-playwright-compatibility.md. Run a real current
Playwright client with protocol debugging against the real WASM server. Reduce
each failure to a deterministic fixture and fix the portable CDP/browser
semantics, never a Playwright-name-specific hack. Do not invoke the native CLI
or addon. Add replay and end-to-end tests. Do not edit docs/README/.agent/
index.js or commit unless told.
```
