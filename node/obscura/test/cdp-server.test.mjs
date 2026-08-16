import { strict as assert } from "node:assert";
import { test } from "node:test";
import vm from "node:vm";
import { ObscuraCdpServer, CdpClient, launch } from "../src/index.mjs";

const PNG = Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

class FakeWorker {
  constructor() {
    this.loaded = false;
    this.url = "about:blank";
    this.revision = 0;
    this.documentHandle = 1;
    this.closed = false;
    this.html = "<html><head><title>Fake</title></head><body><h1>Obscura</h1></body></html>";
  }

  async ready() {}

  async bridgeStatus() {
    return {
      loaded: this.loaded,
      page: { revision: this.revision, documentHandle: this.documentHandle },
      navigation: { state: { url: this.url } },
    };
  }

  async bootstrapEvaluate(source, { html, documentMetadata } = {}) {
    if (html !== undefined) this.html = html;
    this.loaded = true;
    if (documentMetadata?.url) this.url = documentMetadata.url;
    if (source === "undefined") return undefined;
    const document = {
      title: "Fake",
      querySelector: (selector) => selector === "h1" ? { textContent: "Obscura", outerHTML: "<h1>Obscura</h1>" } : null,
    };
    return vm.runInNewContext(source, { document, location: { href: this.url } });
  }

  async navigate(url) {
    this.loaded = true;
    this.url = url;
    this.revision += 1;
    this.documentHandle += 1;
    return { url, loaderId: `loader-${this.revision}`, documentHandle: this.documentHandle, revision: this.revision };
  }

  async screenshotPng() { return { data: PNG }; }

  async pdf() { return { data: Buffer.from("%PDF-1.4\n%%EOF\n") }; }

  async bridgeDomOp(command, arg1) {
    if (command === "query_selector") return arg1 === "html" ? "2" : "-1";
    if (command === "outer_html") return "<html></html>";
    if (command === "attributes") return "[]";
    return "null";
  }

  async close() { this.closed = true; }
}

async function makeServer() {
  const workers = [];
  const server = new ObscuraCdpServer({
    modulePath: "fake.wasm.js",
    workerFactory: async () => {
      const worker = new FakeWorker();
      workers.push(worker);
      return worker;
    },
  });
  await server.start();
  return { server, workers };
}

test("package-owned CDP server exposes browser, target, runtime, screenshot and PDF", async () => {
  const { server, workers } = await makeServer();
  const client = new CdpClient(server.wsEndpoint());
  try {
    await client.ready();
    const version = await client.command("Browser.getVersion");
    assert.equal(version.product, "Obscura/WASM");
    const targets = await client.command("Target.getTargets");
    assert.equal(targets.targetInfos.length, 1);
    const { sessionId } = await client.command("Target.attachToTarget", {
      targetId: targets.targetInfos[0].targetId,
      flatten: true,
    });
    await client.command("Runtime.enable", {}, sessionId);
    await client.command("Page.enable", {}, sessionId);
    const navigation = await client.command("Page.navigate", { url: "https://example.test/" }, sessionId);
    assert.equal(navigation.frameId, targets.targetInfos[0].targetId);
    const evaluated = await client.command("Runtime.evaluate", {
      expression: "document.querySelector('h1').textContent",
      returnByValue: true,
    }, sessionId);
    assert.equal(evaluated.result.value, "Obscura");
    const screenshot = await client.command("Page.captureScreenshot", {}, sessionId);
    assert.deepEqual(Buffer.from(screenshot.data, "base64").subarray(0, 8), Buffer.from(PNG));
    const pdf = await client.command("Page.printToPDF", {}, sessionId);
    assert.match(Buffer.from(pdf.data, "base64").toString("utf8"), /^%PDF-/);
    assert.equal(workers.length, 1);
  } finally {
    await client.close();
    await server.close();
  }
  assert.equal(workers[0].closed, true);
});

test("CDP server rejects unknown methods and invalid WebSocket paths", async () => {
  const { server } = await makeServer();
  const client = new CdpClient(server.wsEndpoint());
  try {
    await client.ready();
    await assert.rejects(client.command("Not.ARealMethod"), (error) => error.code === -32601);
  } finally {
    await client.close();
    await server.close();
  }
});

test("launch rejects native addon paths", async () => {
  await assert.rejects(
    launch({ modulePath: "/tmp/obscura-native.node" }),
    (error) => error.code === "ERR_OBSCURA_NATIVE_UNSUPPORTED",
  );
});
