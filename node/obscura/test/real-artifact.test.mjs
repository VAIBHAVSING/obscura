import { strict as assert } from "node:assert";
import { createServer as createHttpServer } from "node:http";
import { test } from "node:test";
import { ObscuraCdpServer, CdpClient } from "../src/index.mjs";

const modulePath = process.env.OBSCURA_REAL_WASM_MODULE;

test("real WASM artifact is reachable through package CDP", { skip: !modulePath }, async () => {
  const fixture = createHttpServer((request, response) => {
    response.setHeader("content-type", "text/html; charset=utf-8");
    response.end(`<html><body><h1>${request.headers["x-portable"] ?? "missing"}</h1></body></html>`);
  });
  await new Promise((resolve, reject) => {
    fixture.once("error", reject);
    fixture.listen(0, "127.0.0.1", resolve);
  });
  const fixtureAddress = fixture.address();
  const fixtureUrl = `http://127.0.0.1:${fixtureAddress.port}/headers`;
  const server = new ObscuraCdpServer({ modulePath, allowPrivateNetwork: true });
  let client;
  try {
    await server.start();
    client = new CdpClient(server.wsEndpoint());
    await client.ready();
    const { targetId } = await client.command("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await client.command("Target.attachToTarget", { targetId, flatten: true });
    await client.command("Runtime.enable", {}, sessionId);
    await client.command("Page.enable", {}, sessionId);
    await client.command("Emulation.setDeviceMetricsOverride", {
      width: 640,
      height: 480,
      deviceScaleFactor: 2,
    }, sessionId);
    const metrics = await client.command("Page.getLayoutMetrics", {}, sessionId);
    assert.equal(metrics.layoutViewport.clientWidth, 640);
    assert.equal(metrics.layoutViewport.clientHeight, 480);
    await client.command("Page.navigate", {
      url: "data:text/html,<html><body><h1>Portable</h1></body></html>",
    }, sessionId);
    const history = await client.command("Page.getNavigationHistory", {}, sessionId);
    assert.equal(history.currentIndex, 0);
    assert.equal(history.entries.at(-1).url, "data:text/html,<html><body><h1>Portable</h1></body></html>");
    const evaluated = await client.command("Runtime.evaluate", {
      expression: "document.querySelector('h1').textContent",
      returnByValue: true,
    }, sessionId);
    assert.equal(evaluated.result.value, "Portable");
    await client.command("Network.setCookies", {
      cookies: [{ name: "portable", value: "wasm", domain: "example.test", path: "/", httpOnly: true }],
    }, sessionId);
    const cookieSnapshot = await client.command("Network.getAllCookies", {}, sessionId);
    assert.equal(cookieSnapshot.cookies.find((cookie) => cookie.name === "portable")?.value, "wasm");
    await client.command("Network.deleteCookies", {
      name: "portable",
      domain: "example.test",
      path: "/",
    }, sessionId);
    const deletedCookies = await client.command("Storage.getCookies", {}, sessionId);
    assert.equal(deletedCookies.cookies.some((cookie) => cookie.name === "portable"), false);
    await client.command("Network.setExtraHTTPHeaders", { headers: { "X-Portable": "wasm" } }, sessionId);
    await client.command("Page.navigate", { url: fixtureUrl }, sessionId);
    const headerValue = await client.command("Runtime.evaluate", {
      expression: "document.querySelector('h1').textContent",
      returnByValue: true,
    }, sessionId);
    assert.equal(headerValue.result.value, "wasm");
    await client.command("Page.navigate", {
      url: "data:text/html,<html><body><h1>Portable</h1></body></html>",
    }, sessionId);
    await client.command("Runtime.evaluate", {
      expression: "globalThis.__portableClicks = 0; document.body.addEventListener('click', () => { globalThis.__portableClicks += 1; });",
    }, sessionId);
    await client.command("Input.dispatchMouseEvent", { type: "mousePressed", x: 1, y: 1, button: "left", buttons: 1 }, sessionId);
    await client.command("Input.dispatchMouseEvent", { type: "mouseReleased", x: 1, y: 1, button: "left", buttons: 0 }, sessionId);
    const clickCount = await client.command("Runtime.evaluate", { expression: "globalThis.__portableClicks", returnByValue: true }, sessionId);
    assert.equal(clickCount.result.value, 1);
    await client.command("Runtime.evaluate", {
      expression: "globalThis.__portableKeys = 0; document.body.addEventListener('keydown', () => { globalThis.__portableKeys += 1; });",
    }, sessionId);
    await client.command("Input.dispatchKeyEvent", { type: "keyDown", key: "a", code: "KeyA" }, sessionId);
    const keyCount = await client.command("Runtime.evaluate", { expression: "globalThis.__portableKeys", returnByValue: true }, sessionId);
    assert.equal(keyCount.result.value, 1);
    const document = await client.command("DOM.getDocument", { depth: -1 }, sessionId);
    assert.equal(document.root.nodeType, 9);
    const htmlNode = await client.command("DOM.querySelector", { nodeId: document.root.nodeId, selector: "h1" }, sessionId);
    assert.ok(htmlNode.nodeId > 0);
    const outer = await client.command("DOM.getOuterHTML", { nodeId: htmlNode.nodeId }, sessionId);
    assert.equal(outer.outerHTML, '<h1>Portable</h1>');
    const childEvents = [];
    const unsubscribe = client.onEvent((event) => {
      if (event.method === "DOM.setChildNodes") childEvents.push(event);
    });
    await client.command("DOM.requestChildNodes", { nodeId: htmlNode.nodeId }, sessionId);
    await new Promise((resolve) => setTimeout(resolve, 0));
    unsubscribe();
    assert.equal(childEvents.at(-1)?.params.parentId, htmlNode.nodeId);
    await client.command("Page.setDocumentContent", {
      html: "<html><body><p id=next>Next</p></body></html>",
    }, sessionId);
    const replaced = await client.command("DOM.getDocument", { depth: -1 }, sessionId);
    const paragraph = await client.command("DOM.querySelector", { nodeId: replaced.root.nodeId, selector: "#next" }, sessionId);
    assert.ok(paragraph.nodeId > 0);
    const screenshot = await client.command("Page.captureScreenshot", {}, sessionId);
    assert.deepEqual(Buffer.from(screenshot.data, "base64").subarray(0, 8), Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
    const pdf = await client.command("Page.printToPDF", {}, sessionId);
    assert.match(Buffer.from(pdf.data, "base64").toString("utf8", 0, 5), /^%PDF-/);
    const streamedPdf = await client.command("Page.printToPDF", { transferMode: "ReturnAsStream" }, sessionId);
    assert.equal(typeof streamedPdf.stream, "string");
    const streamChunks = [];
    let streamEof = false;
    let streamBytes = 0;
    while (!streamEof) {
      const chunk = await client.command("IO.read", { handle: streamedPdf.stream, size: 64 * 1024 }, sessionId);
      assert.equal(chunk.base64Encoded, true);
      streamChunks.push(Buffer.from(chunk.data, "base64"));
      streamBytes += streamChunks.at(-1).length;
      assert.ok(streamBytes <= 12 * 1024 * 1024);
      streamEof = chunk.eof === true;
    }
    const streamedBytes = Buffer.concat(streamChunks);
    assert.match(streamedBytes.toString("utf8", 0, 5), /^%PDF-/);
    await client.command("IO.close", { handle: streamedPdf.stream }, sessionId);
  } finally {
    await client?.close();
    await server.close();
    await new Promise((resolve) => fixture.close(resolve));
  }
});
