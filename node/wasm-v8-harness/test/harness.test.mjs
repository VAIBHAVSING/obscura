import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { WasmV8Worker } from "../src/client.mjs";
import { loadModule } from "../src/module-loader.mjs";
import { MAX_HTML_INPUT_BYTES, MAX_SELECTOR_BYTES } from "../src/limits.mjs";

const mockModule = fileURLToPath(new URL("./fixtures/mock-wasm-bindgen.cjs", import.meta.url));
const mockNativeAddon = fileURLToPath(new URL("./fixtures/mock-native-addon.cjs", import.meta.url));
const mockObscuraCore = fileURLToPath(new URL("./fixtures/mock-obscura-core.cjs", import.meta.url));
const mockObscuraCoreLegacy = fileURLToPath(new URL("./fixtures/mock-obscura-core-legacy.cjs", import.meta.url));
const mockObscuraCoreMissingAbi = fileURLToPath(
  new URL("./fixtures/mock-obscura-core-missing-abi.cjs", import.meta.url),
);
const mockObscuraCoreMismatchedAbi = fileURLToPath(
  new URL("./fixtures/mock-obscura-core-mismatched-abi.cjs", import.meta.url),
);
const mockLeakyRuntime = fileURLToPath(new URL("./fixtures/mock-leaky-runtime.cjs", import.meta.url));
const harnessCli = fileURLToPath(new URL("../bin/run.mjs", import.meta.url));
const realWasmModule = process.env.OBSCURA_REAL_WASM_MODULE;
const realNativeAddon = process.env.OBSCURA_REAL_NATIVE_ADDON;
const execFileAsync = promisify(execFile);

if (process.env.OBSCURA_REQUIRE_REAL_ARTIFACTS === "1") {
  assert.ok(realWasmModule, "OBSCURA_REAL_WASM_MODULE is required by the artifact test gate");
  assert.ok(realNativeAddon, "OBSCURA_REAL_NATIVE_ADDON is required by the artifact test gate");
}

test("loads a wasm-bindgen-shaped wrapper in a persistent worker", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  try {
    const inspect = await worker.inspect();
    assert.equal(inspect.kind, "wasm-bindgen-wrapper");
    assert.equal(inspect.capabilities.runtimeFactory, "createRuntime");
    assert.equal(inspect.capabilities.abiVersion, "abi_version");
    assert.equal(inspect.runtime.evaluate, "evaluate");
    assert.equal(await worker.request("version"), "mock-wasm-bindgen-1");
    assert.equal(await worker.abiVersion(), 1);
    assert.deepEqual(await worker.request("probe"), { ok: true });
    assert.equal(await worker.hostEvaluate("20 + 22"), 42);
    assert.equal(await worker.hostEvaluate("globalThis.persisted = 41"), 41);
    assert.equal(await worker.hostEvaluate("persisted + 1"), 42);
    assert.equal(
      await worker.hostEvaluate(
        "[typeof process, typeof require, typeof Buffer, typeof global].every((value) => value === 'undefined')",
      ),
      true,
    );
    assert.equal(await worker.moduleEvaluate("6 * 7"), 42);
    assert.equal(await worker.moduleEvaluate("'42'"), "42");
    assert.equal(
      await worker.moduleEvaluate("__timeout__", { evaluateTimeoutMs: 4_321, requestTimeoutMs: 10_000 }),
      4_321,
    );
  } finally {
    await worker.close();
  }
});

test("runs create/evaluate/dispose stress on transient runtimes", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  try {
    const result = await worker.createDrop(250, { source: "40 + 2" });
    assert.equal(result.iterations, 250);
    assert.equal(result.evaluated, 250);
    assert.equal(result.disposed, 250);
    assert.equal(result.lastResult, 42);
    const timeoutResult = await worker.createDrop(2, {
      source: "__timeout__",
      evaluateTimeoutMs: 321,
      requestTimeoutMs: 10_000,
    });
    assert.equal(timeoutResult.lastResult, 321);
  } finally {
    await worker.close();
  }
});

test("rejects runtime factories that cannot dispose deterministically", async () => {
  const worker = await WasmV8Worker.launch(mockLeakyRuntime);
  try {
    await assert.rejects(worker.inspect(), /no deterministic disposal method/);
    await assert.rejects(worker.moduleEvaluate("6 * 7"), /no deterministic disposal method/);
    await assert.rejects(worker.createDrop(10), /no deterministic disposal method/);
  } finally {
    await worker.close();
  }
});

test("recognizes the native EmbeddedRuntime API and decodes its JSON text", async () => {
  const worker = await WasmV8Worker.launch(mockNativeAddon);
  try {
    const inspect = await worker.inspect();
    assert.equal(inspect.capabilities.runtimeFactory, "EmbeddedRuntime");
    assert.deepEqual(await worker.request("probe", { source: "6 * 7" }), { ok: true, result: 42 });
    assert.equal(await worker.moduleEvaluate("21 * 2"), 42);
    await assert.rejects(worker.moduleEvaluate("__timeout_error__"), /eval timed out/);
    assert.equal(await worker.moduleEvaluate("6 * 7"), 42);
  } finally {
    await worker.close();
  }
});

test("does not infer the native JSON-text ABI from an EmbeddedRuntime class name", async () => {
  const directory = await mkdtemp(join(tmpdir(), "obscura-structured-runtime-"));
  const modulePath = join(directory, "structured.cjs");
  await writeFile(
    modulePath,
    `module.exports = { EmbeddedRuntime: class EmbeddedRuntime {
      evaluate() { return "42"; }
      close() {}
    } };`,
  );
  const worker = await WasmV8Worker.launch(modulePath);
  try {
    assert.equal(await worker.moduleEvaluate("ignored"), "42");
  } finally {
    await worker.close();
  }
});

test(
  "initializes the real native addon on the main thread and reuses runtimes after V8 timeouts",
  { skip: !realNativeAddon },
  async () => {
    const gateScript = String.raw`
      const assert = require("node:assert/strict");
      const addon = require(process.argv[1]);
      assert.throws(() => new addon.EmbeddedRuntime(), /initializeEmbeddedV8/);
      assert.throws(() => addon.probe(), /initializeEmbeddedV8/);
      const version = addon.initializeEmbeddedV8();
      assert.equal(typeof version, "string");
      assert.ok(version.length > 0);
      assert.equal(addon.initializeEmbeddedV8(), version);
      assert.equal(addon.embeddedV8Version(), version);
      process.stdout.write(version);
    `;
    const { stdout: embeddedVersion } = await execFileAsync(
      process.execPath,
      ["-e", gateScript, realNativeAddon],
      { timeout: 30_000 },
    );
    assert.ok(embeddedVersion.length > 0);

    const worker = await WasmV8Worker.launch(realNativeAddon, { readyTimeoutMs: 30_000 });
    try {
      assert.equal(await worker.moduleEvaluate("6 * 7"), 42);
      assert.deepEqual(
        await worker.moduleEvaluate(
          `(function(){ JSON.stringify = () => '"spoofed"'; return { answer: 42 }; })()`,
        ),
        { answer: 42 },
      );
      await assert.rejects(
        worker.moduleEvaluate("(function(){ while (true) {} })()", {
          evaluateTimeoutMs: 25,
          requestTimeoutMs: 2_000,
        }),
        /eval timed out/,
      );
      assert.equal(await worker.moduleEvaluate("40 + 2"), 42);
      await assert.rejects(
        worker.moduleEvaluate("({ get value(){ while (true) {} } })", {
          evaluateTimeoutMs: 25,
          requestTimeoutMs: 2_000,
        }),
        /eval timed out/,
      );
      assert.equal(await worker.moduleEvaluate("21 * 2"), 42);
    } finally {
      await worker.close();
    }
  },
);

test(
  "runs concurrent and sequential real native addon Worker lifecycles",
  { skip: !realNativeAddon },
  async () => {
    const launches = await Promise.allSettled(
      Array.from({ length: 4 }, () => WasmV8Worker.launch(realNativeAddon, { readyTimeoutMs: 30_000 })),
    );
    const concurrent = launches.flatMap((launch) => (launch.status === "fulfilled" ? [launch.value] : []));
    try {
      const failedLaunch = launches.find((launch) => launch.status === "rejected");
      if (failedLaunch) throw failedLaunch.reason;
      assert.deepEqual(
        await Promise.all(concurrent.map((worker) => worker.moduleEvaluate("6 * 7"))),
        [42, 42, 42, 42],
      );
    } finally {
      await Promise.all(concurrent.map((worker) => worker.close()));
    }

    for (let index = 0; index < 4; index += 1) {
      const worker = await WasmV8Worker.launch(realNativeAddon, { readyTimeoutMs: 30_000 });
      try {
        assert.equal(await worker.moduleEvaluate(`${index} + 42`), index + 42);
      } finally {
        await worker.close();
      }
    }
  },
);

test("rejects ObscuraCore modules with a missing or mismatched ABI before ready", async () => {
  await assert.rejects(WasmV8Worker.launch(mockObscuraCoreMissingAbi), {
    code: "ERR_OBSCURA_WASM_ABI",
    message: /exposes no ABI version/,
  });
  await assert.rejects(WasmV8Worker.launch(mockObscuraCoreMismatchedAbi), {
    code: "ERR_OBSCURA_WASM_ABI",
    message: /requires ABI version 1, but the module exposes 2/,
  });
});

test("bridges ObscuraCore queries into the persistent host V8 document facade", async () => {
  const worker = await WasmV8Worker.launch(mockObscuraCore);
  try {
    const inspect = await worker.inspect();
    assert.equal(inspect.capabilities.obscuraCore, "ObscuraCore");
    assert.equal(await worker.hostEvaluate("globalThis.preBridgeState = 7"), 7);
    const html = "<!doctype html><html><body><main id='app'><h1 class='title'>Obscura</h1></main></body></html>";
    assert.deepEqual(
      await worker.bridgeEvaluate(
        "[document.querySelector('.title').textContent, document.querySelector('#app').outerHTML, document.documentElement.outerHTML, document.querySelector('.missing')]",
        { html },
      ),
      [
        "Obscura",
        "<main id='app'><h1 class='title'>Obscura</h1></main>",
        "<html><body><main id='app'><h1 class='title'>Obscura</h1></main></body></html>",
        null,
      ],
    );
    assert.equal(await worker.bridgeEvaluate("typeof preBridgeState"), "undefined");
    assert.deepEqual(await worker.bridgeStatus(), {
      loaded: true,
      generation: 1,
      disposed: 0,
      api: {
        querySnapshot: "querySnapshot",
        queryText: "queryText",
        queryHtml: "query_html",
        documentElementHtml: "documentElementHtml",
        dispose: "free",
      },
    });
    assert.equal(
      await worker.bridgeEvaluate(
        "(function(){ 'use strict'; globalThis.previousPageState = 42; try { document.querySelector = null; return false; } catch { return true; } })()",
      ),
      true,
    );
    await assert.rejects(
      worker.bridgeEvaluate("document.querySelector('async-result')"),
      /requires a synchronous result/,
    );
    assert.deepEqual(
      await worker.bridgeEvaluate(
        "[document.querySelector('snapshot-only').outerHTML, document.querySelector('snapshot-only').textContent]",
      ),
      ["<snapshot-only>batched</snapshot-only>", "batched"],
    );
    assert.deepEqual(
      await worker.bridgeEvaluate(`(() => {
        try {
          document.querySelector("[");
          return null;
        } catch (error) {
          return [
            error instanceof SyntaxError,
            error.constructor === SyntaxError,
            error.name,
          ];
        }
      })()`),
      [true, true, "SyntaxError"],
    );

    assert.equal(
      await worker.bridgeEvaluate("document.querySelector('h1').textContent", {
        html: "<html><body><h1>Replacement</h1></body></html>",
      }),
      "Replacement",
    );
    assert.equal(await worker.bridgeEvaluate("typeof previousPageState"), "undefined");
    assert.equal((await worker.bridgeStatus()).disposed, 1);
    const released = await worker.releaseBridge();
    assert.equal(released.loaded, false);
    assert.equal(released.disposed, 2);
    assert.equal(await worker.hostEvaluate("typeof document"), "undefined");
    await assert.rejects(worker.bridgeEvaluate("document.documentElement.outerHTML"), /No ObscuraCore is loaded/);
  } finally {
    await worker.close();
  }
});

test("stress executes the ObscuraCore bridge instead of measuring only host V8", async () => {
  const worker = await WasmV8Worker.launch(mockObscuraCore);
  try {
    const result = await worker.bridgeStress(25, {
      html: "<html><body><h1>bridge stress</h1></body></html>",
      source: "document.querySelector('h1').textContent",
    });
    assert.equal(result.iterations, 25);
    assert.equal(result.lastResult, "bridge stress");
    assert.ok(result.operationsPerSecond > 0);
    assert.equal((await worker.bridgeStress(1)).lastResult, "bridge stress");
  } finally {
    await worker.close();
  }
});

test("retains the legacy two-call query bridge fallback", async () => {
  const worker = await WasmV8Worker.launch(mockObscuraCoreLegacy);
  try {
    assert.equal(
      await worker.bridgeEvaluate("document.querySelector('h1').textContent", {
        html: "<html><body><h1>legacy</h1></body></html>",
      }),
      "legacy",
    );
    assert.equal((await worker.bridgeStatus()).api.querySnapshot, null);
  } finally {
    await worker.close();
  }
});

test("enforces bridge ABI limits and preserves Worker reuse", async () => {
  const worker = await WasmV8Worker.launch(mockObscuraCore);
  try {
    const html = "<html><body><h1>bounded</h1></body></html>";
    await worker.bridgeEvaluate("document.querySelector('h1').textContent", { html });
    await assert.rejects(
      worker.bridgeEvaluate("1", { html: "x".repeat(MAX_HTML_INPUT_BYTES + 1) }),
      RangeError,
    );
    assert.equal(await worker.bridgeEvaluate("document.querySelector('h1').textContent"), "bounded");

    const oversizedSelector = "x".repeat(MAX_SELECTOR_BYTES + 1);
    assert.deepEqual(
      await worker.bridgeEvaluate(`(() => {
        try {
          document.querySelector(${JSON.stringify(oversizedSelector)});
          return null;
        } catch (error) {
          return [error instanceof RangeError, error.name];
        }
      })()`),
      [true, "RangeError"],
    );
    assert.deepEqual(
      await worker.bridgeEvaluate(`(() => {
        try {
          document.querySelector("oversized-result");
          return null;
        } catch (error) {
          return [error instanceof RangeError, error.name];
        }
      })()`),
      [true, "RangeError"],
    );
    assert.equal(await worker.bridgeEvaluate("document.querySelector('h1').textContent"), "bounded");
  } finally {
    await worker.close();
  }
});

test(
  "real wasm-bindgen core exports ABI 1 and throws SyntaxError for invalid selectors",
  { skip: !realWasmModule },
  async () => {
    const { namespace } = await loadModule(realWasmModule);
    assert.equal(namespace.abi_version(), 1);
    const core = new namespace.ObscuraCore("<!doctype html><html><body><h1>real</h1></body></html>");
    try {
      assert.deepEqual(core.query_snapshot("h1"), ["<h1>real</h1>", "real"]);
      assert.equal(core.query_snapshot(".missing"), undefined);
      assert.throws(() => core.query_html("["), SyntaxError);
      assert.throws(() => core.query_text(":not("), SyntaxError);
      assert.throws(() => core.query_count("["), SyntaxError);
      assert.throws(() => core.query_snapshot("x".repeat(MAX_SELECTOR_BYTES + 1)), RangeError);
    } finally {
      core.free();
    }
  },
);

test("serializes concurrent bridge replacements in request order", async () => {
  const worker = await WasmV8Worker.launch(mockObscuraCore);
  try {
    const first = worker.bridgeEvaluate("document.querySelector('h1').textContent", {
      html: "<html><body><h1>first</h1></body></html>",
    });
    const second = worker.bridgeEvaluate("document.querySelector('h1').textContent", {
      html: "<html><body><h1>second</h1></body></html>",
    });
    assert.deepEqual(await Promise.all([first, second]), ["first", "second"]);
    assert.equal((await worker.bridgeStatus()).disposed, 1);
  } finally {
    await worker.close();
  }
});

test("document facade callbacks cannot expose the Worker realm", async () => {
  const worker = await WasmV8Worker.launch(mockObscuraCore);
  try {
    const html = "<html><body><h1>isolated</h1></body></html>";
    assert.deepEqual(
      await worker.bridgeEvaluate(
        `(() => {
          const globalTypes = (FunctionConstructor) => FunctionConstructor(
            "return [typeof process, typeof require, typeof Buffer, typeof global]"
          )();
          const element = document.querySelector("h1");
          const queryDescriptor = Object.getOwnPropertyDescriptor(document, "querySelector");
          const htmlDescriptor = Object.getOwnPropertyDescriptor(document.documentElement, "outerHTML");
          return {
            ordinary: [typeof process, typeof require, typeof Buffer, typeof global],
            queryCallback: globalTypes(document.querySelector.constructor),
            callbackPrototype: globalTypes(Object.getPrototypeOf(document.querySelector).constructor),
            queryDescriptor: globalTypes(queryDescriptor.value.constructor),
            getterCallback: globalTypes(htmlDescriptor.get.constructor),
            descriptorObject: globalTypes(queryDescriptor.constructor.constructor),
            returnedString: globalTypes(element.outerHTML.constructor.constructor),
            nullPrototypeElement: Object.getPrototypeOf(element) === null,
            noElementConstructor: typeof element.constructor,
            frozenCallback: Object.isFrozen(document.querySelector),
            temporaryBindings: [
              typeof globalThis.__obscuraHostQueryElement__,
              typeof globalThis.__obscuraHostDocumentHtml__,
            ],
          };
        })()`,
        { html },
      ),
      {
        ordinary: ["undefined", "undefined", "undefined", "undefined"],
        queryCallback: ["undefined", "undefined", "undefined", "undefined"],
        callbackPrototype: ["undefined", "undefined", "undefined", "undefined"],
        queryDescriptor: ["undefined", "undefined", "undefined", "undefined"],
        getterCallback: ["undefined", "undefined", "undefined", "undefined"],
        descriptorObject: ["undefined", "undefined", "undefined", "undefined"],
        returnedString: ["undefined", "undefined", "undefined", "undefined"],
        nullPrototypeElement: true,
        noElementConstructor: "undefined",
        frozenCallback: true,
        temporaryBindings: ["undefined", "undefined"],
      },
    );

    assert.deepEqual(
      await worker.bridgeEvaluate(`(() => {
        try {
          document.querySelector("async-result");
          return null;
        } catch (error) {
          return [
            error instanceof TypeError,
            error.constructor === TypeError,
            Object.getPrototypeOf(error) === TypeError.prototype,
            error.constructor.constructor(
              "return [typeof process, typeof require, typeof Buffer, typeof global]"
            )(),
            error.message.constructor.constructor(
              "return [typeof process, typeof require, typeof Buffer, typeof global]"
            )(),
            error.message,
          ];
        }
      })()`),
      [
        true,
        true,
        true,
        ["undefined", "undefined", "undefined", "undefined"],
        ["undefined", "undefined", "undefined", "undefined"],
        "querySnapshot returned a Promise; this operation requires a synchronous result",
      ],
    );

    assert.deepEqual(
      await worker.bridgeEvaluate(
        `(() => {
          try {
            return document.documentElement.outerHTML;
          } catch (error) {
            return [
              error instanceof RangeError,
              error.constructor === RangeError,
              error.constructor.constructor(
                "return [typeof process, typeof require, typeof Buffer, typeof global]"
              )(),
              error.message,
            ];
          }
        })()`,
        { html: "<html data-document-error><body></body></html>" },
      ),
      [true, true, ["undefined", "undefined", "undefined", "undefined"], "document serializer failed"],
    );
  } finally {
    await worker.close();
  }
});

test("rejects asynchronous host results instead of escaping the VM deadline", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  try {
    await assert.rejects(
      worker.hostEvaluate("new Promise(() => {})", { timeoutMs: 1_000, requestTimeoutMs: 2_000 }),
      /requires a synchronous result/,
    );
    await assert.rejects(
      worker.hostEvaluate("Promise.resolve().then(() => { while (true) {} })", {
        timeoutMs: 50,
        requestTimeoutMs: 2_000,
      }),
      /timed out/,
    );
    assert.equal(await worker.hostEvaluate("40 + 2"), 42);
  } finally {
    await worker.close();
  }
});

test("keeps result getters, thrown-value getters, and thenables inside the VM deadline", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  try {
    const startedAt = performance.now();
    await assert.rejects(
      worker.hostEvaluate(
        `({
          get delayed() {
            const deadline = Date.now() + 1_000;
            while (Date.now() < deadline) {}
            return 42;
          }
        })`,
        { timeoutMs: 20, requestTimeoutMs: 2_000 },
      ),
      /timed out/,
    );
    assert.ok(performance.now() - startedAt < 750, "finite result getter escaped the VM deadline");

    await assert.rejects(
      worker.hostEvaluate("({ get then() { while (true) {} } })", {
        timeoutMs: 20,
        requestTimeoutMs: 2_000,
      }),
      /timed out/,
    );
    await assert.rejects(
      worker.hostEvaluate("(() => { throw { get message() { while (true) {} } }; })()", {
        timeoutMs: 20,
        requestTimeoutMs: 2_000,
      }),
      /timed out/,
    );
    assert.equal(await worker.hostEvaluate("6 * 7"), 42);
  } finally {
    await worker.close();
  }
});

test("coerces selectors in the page realm and bounds hostile toString hooks", async () => {
  const worker = await WasmV8Worker.launch(mockObscuraCore);
  try {
    const html = "<html><body><h1>bounded</h1></body></html>";
    assert.equal(
      await worker.bridgeEvaluate(
        "document.querySelector({ toString() { return 'h1'; } }).textContent",
        { html, timeoutMs: 100, requestTimeoutMs: 2_000 },
      ),
      "bounded",
    );
    await assert.rejects(
      worker.bridgeEvaluate("document.querySelector({ toString() { while (true) {} } })", {
        timeoutMs: 20,
        requestTimeoutMs: 2_000,
      }),
      /timed out/,
    );
    assert.equal(await worker.bridgeEvaluate("document.querySelector('h1').textContent"), "bounded");
  } finally {
    await worker.close();
  }
});

test("bounds hostStress result inspection inside each VM iteration", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  try {
    await assert.rejects(
      worker.request(
        "hostStress",
        { iterations: 1, source: "({ get then() { while (true) {} } })", timeoutMs: 20 },
        2_000,
      ),
      /timed out/,
    );
    assert.equal(await worker.hostEvaluate("40 + 2"), 42);
  } finally {
    await worker.close();
  }
});

test("outer request deadline terminates a Worker stuck in result inspection", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  await assert.rejects(
    worker.hostEvaluate("({ get then() { while (true) {} } })", {
      timeoutMs: 10_000,
      requestTimeoutMs: 50,
    }),
    /hostEvaluate timed out after 50ms/,
  );
  // A timeout starts termination asynchronously. Explicit termination must
  // join that existing operation rather than returning early because closed
  // was set before Worker.terminate() finished.
  await worker.terminate();
  await assert.rejects(worker.inspect(), /closed/);
});

test("terminates the isolation boundary when an operation deadline expires", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  await assert.rejects(
    worker.moduleEvaluate("__never__", { evaluateTimeoutMs: 1_000, requestTimeoutMs: 100 }),
    /moduleEvaluate timed out after 100ms/,
  );
  await assert.rejects(worker.inspect(), /closed/);
});

test("rejects invalid payloads and uncloneable results without poisoning the worker", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  try {
    await assert.rejects(worker.request("probe", { source: () => 42 }), /clone/i);
    await assert.rejects(worker.hostEvaluate("Symbol('not-cloneable')"), /clone/i);
    await assert.rejects(worker.hostEvaluate("new Date()"), /plain or null-prototype objects/);
    await assert.rejects(worker.hostEvaluate("new Map([['answer', 42]])"), /plain or null-prototype objects/);
    await assert.rejects(worker.hostEvaluate("new Uint8Array([4, 2])"), /plain or null-prototype objects/);
    await assert.rejects(
      worker.hostEvaluate("(() => { const value = []; value.length = 100001; return value; })()"),
      /array length limit/,
    );
    await assert.rejects(worker.hostEvaluate(42), /source must be a string/);
    await assert.rejects(worker.hostEvaluate("1 + 1", { timeoutMs: 1.5 }), /timeoutMs must be an integer/);
    await assert.rejects(worker.hostEvaluate("1 + 1", { requestTimeoutMs: 0 }), /request timeout must be an integer/);
    assert.equal(await worker.hostEvaluate("6 * 7"), 42);
  } finally {
    await worker.close();
  }
});

test("round-trips bounded graph values without prototype mutation", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  try {
    const result = await worker.hostEvaluate(`(() => {
      const value = {
        missing: undefined,
        bigint: 12345678901234567890n,
        nan: NaN,
        positiveInfinity: Infinity,
        negativeInfinity: -Infinity,
        negativeZero: -0,
      };
      Object.defineProperty(value, "__proto__", {
        enumerable: true,
        value: "data-only",
      });
      value.self = value;
      return value;
    })()`);
    assert.equal(result.missing, undefined);
    assert.equal(result.bigint, 12345678901234567890n);
    assert.equal(Number.isNaN(result.nan), true);
    assert.equal(result.positiveInfinity, Infinity);
    assert.equal(result.negativeInfinity, -Infinity);
    assert.equal(Object.is(result.negativeZero, -0), true);
    assert.equal(result.self, result);
    assert.equal(Object.getPrototypeOf(result), Object.prototype);
    assert.equal(Object.hasOwn(result, "__proto__"), true);
    assert.equal(result.__proto__, "data-only");
  } finally {
    await worker.close();
  }
});

test("close is idempotent when callers close concurrently", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  await Promise.all([worker.close(), worker.close(), worker.close()]);
  await assert.rejects(worker.inspect(), /closed/);
});

test("termination rejects subsequent requests", async () => {
  const worker = await WasmV8Worker.launch(mockModule);
  const inFlight = worker
    .hostEvaluate("while (true) {}", { timeoutMs: 10_000, requestTimeoutMs: 15_000 })
    .then(
      () => false,
      () => true,
    );
  await worker.terminate();
  assert.equal(await inFlight, true);
  await assert.rejects(worker.inspect(), /closed/);
});

test("a startup timeout terminates the Worker before launch rejects", async () => {
  const directory = await mkdtemp(join(tmpdir(), "obscura-wasm-harness-timeout-"));
  const stalledModule = join(directory, "stalled.mjs");
  await writeFile(stalledModule, "await new Promise(() => setInterval(() => {}, 1_000));\n");

  await assert.rejects(
    WasmV8Worker.launch(stalledModule, { readyTimeoutMs: 50 }),
    /did not become ready within 50ms/,
  );
});

test("loads an import-free raw WASM module", async () => {
  const directory = await mkdtemp(join(tmpdir(), "obscura-wasm-harness-"));
  const wasmPath = join(directory, "answer.wasm");
  const bytes = Buffer.from(
    "0061736d010000000105016000017f03020100070a0106616e7377657200000a06010400412a0b",
    "hex",
  );
  await writeFile(wasmPath, bytes);

  const worker = await WasmV8Worker.launch(wasmPath);
  try {
    const inspect = await worker.inspect();
    assert.equal(inspect.kind, "raw-wasm");
    assert.deepEqual(inspect.wasm.imports, []);
    assert.equal(inspect.wasm.instantiated, true);
    assert.ok(inspect.exports.includes("answer"));
    assert.equal(await worker.hostEvaluate("21 * 2"), 42);
  } finally {
    await worker.close();
  }
});

test("inspects imported raw WASM and directs callers to its JavaScript wrapper", async () => {
  const directory = await mkdtemp(join(tmpdir(), "obscura-wasm-harness-import-"));
  const wasmPath = join(directory, "imported.wasm");
  const bytes = Buffer.from(
    "0061736d010000000105016000017f02090103656e7601610000030201000708010463616c6c00010a0601040010000b",
    "hex",
  );
  await writeFile(wasmPath, bytes);

  const worker = await WasmV8Worker.launch(wasmPath);
  try {
    const inspect = await worker.inspect();
    assert.equal(inspect.wasm.instantiated, false);
    assert.deepEqual(inspect.wasm.imports, [{ module: "env", name: "a", kind: "function" }]);
    assert.match(inspect.wasm.note, /wasm-bindgen Node\.js wrapper/);
  } finally {
    await worker.close();
  }
});

test("CLI rejects inspection-only raw WASM for artifact-facing modes", async () => {
  const directory = await mkdtemp(join(tmpdir(), "obscura-wasm-harness-cli-import-"));
  const wasmPath = join(directory, "imported.wasm");
  const bytes = Buffer.from(
    "0061736d010000000105016000017f02090103656e7601610000030201000708010463616c6c00010a0601040010000b",
    "hex",
  );
  await writeFile(wasmPath, bytes);

  for (const mode of ["smoke", "bridge", "stress", "terminate"]) {
    await assert.rejects(
      execFileAsync(
        process.execPath,
        [
          harnessCli,
          "--module",
          wasmPath,
          "--mode",
          mode,
          "--iterations",
          "1",
          "--timeout-ms",
          "1000",
          "--json",
        ],
        { timeout: 10_000 },
      ),
      (error) => {
        assert.match(error.stderr, /no executable Obscura capability/);
        return true;
      },
    );
  }
});

test("CLI emits valid JSON for circular evaluation results", async () => {
  const { stdout } = await execFileAsync(
    process.execPath,
    [
      harnessCli,
      "--module",
      mockModule,
      "--mode",
      "smoke",
      "--source",
      "(() => { const value = {}; value.self = value; return value; })()",
      "--json",
    ],
    { timeout: 10_000 },
  );
  const report = JSON.parse(stdout);
  assert.equal(report.smoke.hostV8.self, "[Circular]");
});
