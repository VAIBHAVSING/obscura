import { strict as assert } from "node:assert";
import { test } from "node:test";

import createBrowser, { createBrowser as createNamedBrowser } from "../dist/src/index.mjs";

test("default and named createBrowser run in-process without opening a port", async () => {
  assert.equal(createBrowser, createNamedBrowser);
  const browser = await createBrowser();
  try {
    assert.deepEqual(browser.processInfo(), {
      pid: process.pid,
      host: null,
      port: null,
      native: false,
      wasm: true,
    });
    const page = await browser.newPage();
    await page.goto("data:text/html,<title>Embedded</title><h1>Obscura</h1>");
    assert.equal(await page.title(), "Embedded");
    assert.match(await page.content(), /<h1>Obscura<\/h1>/);
    assert.equal(browser.processInfo().port, null);
  } finally {
    await browser.close({ persist: "skip" });
  }
});

test("screenshot works in-process without opening a CDP listener", async () => {
  const browser = await createBrowser();
  try {
    const page = await browser.newPage();
    await page.goto("data:text/html,<style>body{background:%23fff}</style><h1>Screenshot</h1>");
    const screenshot = await page.screenshot();
    assert.deepEqual(Array.from(screenshot.subarray(0, 8)), [137, 80, 78, 71, 13, 10, 26, 10]);
    assert.equal(browser.processInfo().port, null);
  } finally {
    await browser.close({ persist: "skip" });
  }
});

test("in-memory CDP and explicit lazy listening share the embedded browser", async () => {
  const browser = await createBrowser();
  const connection = await browser.cdp().connect();
  try {
    const version = await connection.command("Browser.getVersion");
    assert.equal(version.product, "Obscura/WASM");
    assert.equal(browser.processInfo().port, null);
    await browser.cdp().listen();
    assert.match(browser.httpEndpoint(), /^http:\/\/127\.0\.0\.1:\d+$/u);
    assert.ok((browser.processInfo().port ?? 0) > 0);
  } finally {
    await connection.close();
    await browser.close({ persist: "skip" });
  }
});

test("a profile checkpoints through a caller-provided object store", async () => {
  let saved: Uint8Array | undefined;
  const store = {
    kind: "memory",
    async load() { return null; },
    async save(_key, bytes) {
      saved = new Uint8Array(bytes);
      return { version: "v1" };
    },
  };
  const browser = await createBrowser({ profile: "customer-123", persistence: store, autoBackup: false });
  try {
    const page = await browser.newPage();
    await page.goto("data:text/html,<title>Profile</title>");
    const result = await browser.defaultContext().backup();
    assert.equal(result.saved, true);
    assert.equal(result.profile, "customer-123");
    assert.equal(result.version, "v1");
    assert.ok((saved?.byteLength ?? 0) > 32);
  } finally {
    await browser.close({ persist: "skip" });
  }
});

test("a profile restores cookies into a later browser process", async () => {
  let stored: { bytes: Uint8Array; version: string } | null = null;
  const store = {
    kind: "memory",
    async load() {
      return stored && { bytes: new Uint8Array(stored.bytes), version: stored.version };
    },
    async save(_key, bytes, options = {}) {
      assert.equal(options.expectedVersion, stored?.version ?? null);
      const version = `v${Number(stored?.version.slice(1) ?? 0) + 1}`;
      stored = { bytes: new Uint8Array(bytes), version };
      return { version };
    },
  };

  const first = await createBrowser({ profile: "customer-restore", persistence: store, autoBackup: false });
  try {
    await first.defaultContext().addCookies([{
      name: "session",
      value: "persistent",
      domain: "example.test",
      path: "/",
    }]);
    assert.equal((await first.defaultContext().backup()).saved, true);
  } finally {
    await first.close({ persist: "skip" });
  }

  const second = await createBrowser({ profile: "customer-restore", persistence: store, autoBackup: false });
  try {
    const cookies = await second.defaultContext().cookies();
    assert.equal(cookies.find((cookie) => cookie.name === "session")?.value, "persistent");
  } finally {
    await second.close({ persist: "skip" });
  }
});
