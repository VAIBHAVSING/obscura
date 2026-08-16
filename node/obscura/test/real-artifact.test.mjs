import { strict as assert } from "node:assert";
import { test } from "node:test";
import { ObscuraCdpServer, CdpClient } from "../src/index.mjs";

const modulePath = process.env.OBSCURA_REAL_WASM_MODULE;

test("real WASM artifact is reachable through package CDP", { skip: !modulePath }, async () => {
  const server = new ObscuraCdpServer({ modulePath });
  let client;
  try {
    await server.start();
    client = new CdpClient(server.wsEndpoint());
    await client.ready();
    const { targetId } = await client.command("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await client.command("Target.attachToTarget", { targetId, flatten: true });
    await client.command("Runtime.enable", {}, sessionId);
    await client.command("Page.enable", {}, sessionId);
    await client.command("Page.navigate", {
      url: "data:text/html,<html><body><h1>Portable</h1></body></html>",
    }, sessionId);
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
  } finally {
    await client?.close();
    await server.close();
  }
});
