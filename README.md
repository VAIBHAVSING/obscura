# Obscura

Obscura is a lightweight headless browser engine with a real DOM, JavaScript
execution, layout, paint, and Chrome DevTools Protocol support. The repository
contains a Rust engine workspace and an embedded WebAssembly browser for
Node.js.

## Node.js quickstart

```bash
npm install @obscura/browser
```

```ts
import createBrowser from "@obscura/browser";

const browser = await createBrowser();
const page = await browser.newPage();
await page.goto("https://example.com");
console.log(await page.title());
await browser.close();
```

The package also includes Puppeteer and Playwright adapters, profile
persistence, screenshots, PDF output, and a small `obscura-browser` utility:

```bash
npx --package @obscura/browser obscura-browser version
npx --package @obscura/browser obscura-browser eval https://example.com "document.title"
npx --package @obscura/browser obscura-browser screenshot https://example.com page.png
npx --package @obscura/browser obscura-browser pdf https://example.com page.pdf
```

Use `obscura-browser serve --json` when a network CDP endpoint is needed for
Puppeteer or Playwright.

## Rust workspace

The Rust workspace provides the engine layers and the embeddable `obscura` API:

- `obscura-browser`: navigation, pages, lifecycle, and browser state.
- `obscura-cdp`: Chrome DevTools Protocol transport and domain handlers.
- `obscura-js`: V8 runtime and browser JavaScript APIs.
- `obscura-dom`: DOM tree and selectors.
- `obscura-net`: HTTP, cookies, robots, and optional stealth transport.
- `obscura-render`: layout, text shaping, and CPU-backed paint.
- `obscura`: embeddable Rust library API.

Build the workspace or the Rust API:

```bash
cargo build --release --workspace --exclude obscura-wasm
cargo build --release -p obscura --features render
```

The first Rust build compiles V8 from source. Run tests with `cargo nextest`,
not `cargo test`, because V8-backed tests require process isolation:

```bash
cargo nextest run --release --features render --no-fail-fast
```

## Documentation

- [Build from source](docs/Build-from-source.md)
- [Connect Puppeteer or Playwright](docs/Connect-Puppeteer-or-Playwright.md)
- [Use with Puppeteer](docs/Use-with-Puppeteer.md)
- [Use with Playwright](docs/Use-with-Playwright.md)
- [Use as a Rust library](docs/Use-as-a-Rust-library.md)
- [Persist cookies and storage](docs/Persist-cookies-and-storage.md)
- [Intercept and modify requests](docs/Intercept-and-modify-requests.md)
- [Architecture overview](docs/Architecture-overview.md)
- [Testing and debugging](docs/Testing-and-debugging.md)

## Links

- Source: https://github.com/h4ckf0r0day/obscura
- Issues: https://github.com/h4ckf0r0day/obscura/issues

License: Apache-2.0.
