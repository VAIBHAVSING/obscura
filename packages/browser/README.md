# @obscura/browser

An embedded, WebAssembly-based browser for Node.js. It starts in the current
process, opens no listening port by default, and ships its browser artifact in
the npm package.

```ts
import createBrowser from "@obscura/browser";

const browser = await createBrowser();
const page = await browser.newPage();
await page.goto("https://example.com");
console.log(await page.title());
await browser.close();
```

## Persistent profiles

A context profile is the equivalent of a Chrome user profile: it has a stable
identity and can be restored into a later browser process. The current snapshot
format persists cookies and context options. It is versioned so more durable
browser state can be added without changing the storage API.

Passing only `profile` uses the local profile directory. A final checkpoint is
required when the context or browser closes.

```ts
const browser = await createBrowser({ profile: "customer-123" });
// ...authenticate and browse...
await browser.close();
```

Choose a directory or an S3-compatible object store with the package subpaths:

```ts
import createBrowser from "@obscura/browser";
import { local } from "@obscura/browser/storage";
import s3 from "@obscura/browser/s3";

const localBrowser = await createBrowser({
  profile: "customer-123",
  persistence: local({ directory: "/var/lib/my-app/profiles" }),
  profileEncryptionKey: encryptionKey, // 32-byte Uint8Array
});

const remoteBrowser = await createBrowser({
  profile: "customer-123",
  persistence: s3({
    endpoint: "https://objects.example.com",
    bucket: "browser-profiles",
    region: "us-east-1",
  }),
});
```

Call `context.backup()` for an explicit checkpoint. Automatic debounced
checkpoints are enabled by default. `close({ persist: "skip" })` is available
for a deliberate force-close.

## CDP, Puppeteer, and Playwright

The direct API and Puppeteer adapter use an in-memory CDP transport. A network
endpoint is created only when explicitly requested. Playwright currently uses
a loopback CDP endpoint because its public connection API requires one.

```ts
import createBrowser from "@obscura/browser";
import cdp from "@obscura/browser/cdp";

const embedded = await createBrowser();
const puppeteerBrowser = await embedded.puppeteer(); // install puppeteer-core

const exposed = await createBrowser({
  cdp: cdp({ expose: true, host: "127.0.0.1", port: 0 }),
});
console.log(exposed.wsEndpoint());
```

The `@obscura/browser/transport` subpath contains the bounded, versioned
request/event transport intended for a future remote browser service. Local and
remote placement can therefore share the same public browser API.
