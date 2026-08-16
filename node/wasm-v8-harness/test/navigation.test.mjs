import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";

import { WasmV8Worker } from "../src/client.mjs";

const realWasmModule = process.env.OBSCURA_REAL_WASM_MODULE;

test("portable navigation owns redirects, document identity, history, and HTML commit", {
  skip: !realWasmModule,
}, async () => {
  const server = http.createServer((request, response) => {
    if (request.url === "/redirect") {
      response.writeHead(302, { Location: "/page" });
      response.end();
      return;
    }
    if (request.url === "/page") {
      response.writeHead(200, { "content-type": "text/html; charset=UTF-8" });
      response.end("<!doctype html><html><head><title>portable navigation</title></head><body><h1>Node host only</h1><script>document.body.setAttribute('data-inline', 'yes')</script><script src='/script.js'></script></body></html>");
      return;
    }
    if (request.url === "/script.js") {
      response.writeHead(200, { "content-type": "text/javascript" });
      response.end("document.querySelector('h1').textContent = document.body.getAttribute('data-inline') === 'yes' ? 'script order ok' : 'script order bad';");
      return;
    }
    if (request.url === "/api") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end('{"answer":42}');
      return;
    }
    response.writeHead(404, { "content-type": "text/html" });
    response.end("<h1>missing</h1>");
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const worker = await WasmV8Worker.launch(realWasmModule);
  try {
    await worker.bridgeDomBatch([], { html: "" });
    const initial = await worker.bridgeStatus();
    assert.equal(initial.navigation.available, true);
    assert.equal(initial.navigation.abiVersion, 1);

    const blank = await worker.navigate("about:blank");
    assert.equal(blank.url, "about:blank");
    const data = await worker.navigate("data:text/html,%3Cmain%3Einline%3C%2Fmain%3E");
    assert.match(data.url, /^data:/);
    const page = await worker.navigate(`http://127.0.0.1:${port}/redirect`, {
      allowPrivateNetwork: true,
    });
    assert.equal(page.status, 200);
    assert.equal(page.navigation.currentUrl, `http://127.0.0.1:${port}/page`);
    assert.equal(page.navigation.pending, null);
    assert.ok(page.navigation.history.length >= 4);
    assert.deepEqual(page.scripts.executed.map(({ nid }) => nid).length, 2);
    assert.equal(await worker.bootstrapEvaluate("document.querySelector('title').textContent"), "portable navigation");
    assert.equal(await worker.bootstrapEvaluate("document.querySelector('h1').textContent"), "script order ok");
    await worker.bootstrapEvaluate("globalThis.__fetchValue = null; fetch('/api').then((response) => response.text()).then((value) => { globalThis.__fetchValue = value; }); undefined");
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.equal(await worker.bootstrapEvaluate("globalThis.__fetchValue"), '{"answer":42}');
    await worker.bootstrapEvaluate("globalThis.__fetchError = null; fetch('http://127.0.0.1:1/unreachable').catch((error) => { globalThis.__fetchError = error.name; }); undefined");
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.equal(await worker.bootstrapEvaluate("globalThis.__fetchError"), "TypeError");

    await assert.rejects(
      worker.navigate("http://127.0.0.1:1/blocked"),
      (error) => error?.code === "ERR_OBSCURA_SSRF",
    );
  } finally {
    await worker.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
