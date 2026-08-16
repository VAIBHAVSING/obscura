import { strict as assert } from "node:assert";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import { launch } from "../src/index.mjs";

const playwrightModule = process.env.OBSCURA_PLAYWRIGHT_CORE_MODULE;
const wasmModule = process.env.OBSCURA_REAL_WASM_MODULE;

test("Playwright connects over CDP to the portable WASM browser", { skip: !playwrightModule || !wasmModule }, async () => {
  const { chromium } = await import(pathToFileURL(playwrightModule).href);
  const browser = await launch({ modulePath: wasmModule });
  const playwright = await chromium.connectOverCDP(browser.httpEndpoint());
  try {
    const page = await playwright.contexts()[0].newPage();
    await page.goto("data:text/html,<html><head><title>Obscura</title></head><body><h1>Portable</h1></body></html>", {
      waitUntil: "commit",
      timeout: 5_000,
    });
    assert.equal(await page.title(), "Obscura");
    assert.equal(await page.locator("h1").textContent(), "Portable");
    assert.equal(await page.evaluate(() => document.querySelector("h1").textContent), "Portable");
    const png = await page.screenshot();
    assert.equal(Buffer.from(png).subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
    const pdf = await page.pdf();
    assert.equal(Buffer.from(pdf).subarray(0, 5).toString(), "%PDF-");
  } finally {
    await playwright.close();
    await browser.close();
  }
});
