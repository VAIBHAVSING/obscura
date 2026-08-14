import vm from "node:vm";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { parentPort, workerData, threadId } from "node:worker_threads";

import {
  BOOTSTRAP_PLATFORM_OP_COMMANDS,
  compileBootstrapRuntime,
} from "./bootstrap-runtime.mjs";
import { loadModule } from "./module-loader.mjs";
import { PortableTaskHost } from "./task-runtime.mjs";
import {
  MAX_DOCUMENT_METADATA_BYTES,
  MAX_DOM_ARGUMENT_BYTES,
  MAX_DOM_BATCH_BYTES,
  MAX_DOM_BATCH_OPERATIONS,
  MAX_DOM_COMMAND_BYTES,
  MAX_HTML_INPUT_BYTES,
  MAX_PLATFORM_COMMAND_BYTES,
  MAX_PLATFORM_BINARY_BYTES,
  MAX_PLATFORM_KDF_OUTPUT_BYTES,
  MAX_PLATFORM_PBKDF2_ITERATIONS,
  MAX_PLATFORM_PBKDF2_WORK_UNITS,
  MAX_PLATFORM_RANDOM_BYTES,
  MAX_PLATFORM_REQUEST_BYTES,
  MAX_PLATFORM_RESPONSE_BYTES,
  MAX_RENDER_RESOURCE_BYTES,
  MAX_RENDER_RESOURCE_REQUESTS_PER_PAGE,
  MAX_RENDER_URL_BYTES,
  MAX_RETURNED_STRING_BYTES,
  MAX_SCREENSHOT_DIMENSION,
  MAX_SCREENSHOT_PIXELS,
  MAX_SELECTOR_BYTES,
  requireBoundedBytes,
  requireBoundedString,
  requireRenderImageRequestProfile,
  requireValidPngBytes,
} from "./limits.mjs";

if (!parentPort) throw new Error("The WASM V8 harness worker must run as a Worker");

const VERSION_NAMES = ["version", "getVersion", "get_version", "obscuraVersion", "obscura_version"];
const ABI_VERSION_NAMES = ["abiVersion", "abi_version"];
const PROBE_NAMES = ["probe", "selfTest", "self_test"];
const FACTORY_NAMES = [
  "createRuntime",
  "create_runtime",
  "createEngine",
  "create_engine",
  "newRuntime",
  "new_runtime",
  "createBrowser",
  "create_browser",
];
const CONSTRUCTOR_NAMES = ["EmbeddedRuntime", "Runtime", "Engine", "ObscuraRuntime", "Browser"];
const EVALUATE_NAMES = ["evaluate", "eval", "evaluateScript", "evaluate_script", "executeScript", "execute_script"];
const DISPOSE_NAMES = ["close", "dispose", "destroy", "free", "drop"];
const QUERY_TEXT_NAMES = ["query_text", "queryText"];
const QUERY_HTML_NAMES = ["query_html", "queryHtml"];
const QUERY_SNAPSHOT_NAMES = ["query_snapshot", "querySnapshot"];
const DOCUMENT_ELEMENT_HTML_NAMES = ["document_element_html", "documentElementHtml"];
const DOM_OP_NAMES = ["dom_op", "domOp"];
const DOM_BATCH_NAMES = ["dom_batch", "domBatch"];
const PAGE_REVISION_NAMES = ["page_revision", "pageRevision"];
const DOCUMENT_HANDLE_NAMES = ["document_handle", "documentHandle"];
const SET_DOCUMENT_METADATA_NAMES = ["set_document_metadata", "setDocumentMetadata"];
const SEED_RENDER_RESOURCE_NAMES = ["seed_render_resource", "seedRenderResource"];
const SEED_MISSING_RENDER_RESOURCE_NAMES = ["seed_missing_render_resource", "seedMissingRenderResource"];
const RENDER_RESOURCE_REQUEST_NAMES = ["render_resource_requests", "renderResourceRequests"];
const SEED_RENDER_IMAGE_RESOURCE_NAMES = ["seed_render_image_resource", "seedRenderImageResource"];
const SEED_MISSING_RENDER_IMAGE_RESOURCE_NAMES = [
  "seed_missing_render_image_resource",
  "seedMissingRenderImageResource",
];
const SCREENSHOT_PNG_NAMES = ["screenshot_png", "screenshotPng"];
const PLATFORM_OP_ABI_VERSION_NAME = "platformOpAbiVersion";
const PLATFORM_OP_NAME = "platformOp";
const REQUIRED_CORE_ABI_VERSION = 1;
const REQUIRED_DOM_OP_ABI_VERSION = 1;
const REQUIRED_DOM_BATCH_ABI_VERSION = 1;
const REQUIRED_DOCUMENT_METADATA_ABI_VERSION = 1;
const REQUIRED_PLATFORM_OP_ABI_VERSION = 1;
const REQUIRED_RENDER_ABI_VERSION = 1;
const REQUIRED_RENDER_RESOURCE_REQUEST_ABI_VERSION = 1;
const PLATFORM_OP_COMMAND_SET = new Set(BOOTSTRAP_PLATFORM_OP_COMMANDS);
const PLATFORM_BYTE_FIELDS = Object.freeze({
  op_text_decode: ["bytes"],
  op_subtle_digest: ["data"],
  op_subtle_hmac: ["key", "data"],
  op_subtle_aes_gcm: ["key", "iv", "aad", "data"],
  op_subtle_aes_cbc: ["key", "iv", "data"],
  op_subtle_aes_ctr: ["key", "counter", "data"],
  op_subtle_pbkdf2: ["password", "salt"],
  op_subtle_hkdf: ["ikm", "salt", "info"],
});
const PLATFORM_BYTE_RESULT_COMMANDS = new Set([
  "op_random_bytes",
  "op_subtle_digest",
  "op_subtle_hmac",
  "op_subtle_aes_gcm",
  "op_subtle_aes_cbc",
  "op_subtle_aes_ctr",
  "op_subtle_pbkdf2",
  "op_subtle_hkdf",
]);
const MAX_VM_TIMEOUT_MS = 4_294_967_295;
const QUERY_BINDING = "__obscuraHostQueryElement__";
const DOCUMENT_HTML_BINDING = "__obscuraHostDocumentHtml__";
const BOUNDED_EVALUATE_BINDING = "__obscuraBoundedEvaluate__";
const MAX_BOUNDED_SERIALIZED_CHARS = 4 * 1024 * 1024;
const MAX_BOUNDED_GRAPH_NODES = 50_000;
const MAX_BOUNDED_GRAPH_PROPERTIES = 100_000;
const MAX_BOUNDED_GRAPH_DEPTH = 256;
const MAX_BOUNDED_ARRAY_LENGTH = 100_000;
const DEFAULT_BOOTSTRAP_PATH = fileURLToPath(
  new URL("../../../crates/obscura-js/js/bootstrap.js", import.meta.url),
);
const installBoundedEvaluateScript = new vm.Script(
  `(() => {
    "use strict";
    const safeApply = Reflect.apply;
    const safeCreate = Object.create;
    const safeKeys = Object.keys;
    const safeGetPrototypeOf = Object.getPrototypeOf;
    const safeObjectIs = Object.is;
    const safeArrayIsArray = Array.isArray;
    const safeNumberIsNaN = Number.isNaN;
    const safeNumberIsSafeInteger = Number.isSafeInteger;
    const safeJsonStringify = JSON.stringify;
    const safeBigIntToString = BigInt.prototype.toString;
    const safeStringSlice = String.prototype.slice;
    const safeWeakMapGet = WeakMap.prototype.get;
    const safeWeakMapSet = WeakMap.prototype.set;
    const SafeWeakMap = WeakMap;
    const ContextTypeError = TypeError;
    const ContextRangeError = RangeError;
    const ContextObjectPrototype = Object.prototype;
    const ContextArrayPrototype = Array.prototype;
    const contextString = String;
    const indirectEval = eval;

    const apply = (callback, receiver, args) => safeApply(callback, receiver, args);
    const record = () => apply(safeCreate, undefined, [null]);
    const tagged = (tag, value) => {
      const result = record();
      result.t = tag;
      if (value !== undefined) result.v = value;
      return result;
    };
    const errorText = (error, property, fallback) => {
      try {
        const value = error?.[property];
        if (typeof value === "string") return value;
        if (value == null) return fallback;
        return apply(contextString, undefined, [value]);
      } catch {
        return fallback;
      }
    };
    const failure = (error) => {
      const envelope = record();
      const detail = record();
      envelope.ok = false;
      detail.name = apply(safeStringSlice, errorText(error, "name", "Error"), [0, 256]);
      detail.message = apply(
        safeStringSlice,
        errorText(error, "message", "Obscura page evaluation failed"),
        [0, 16_384],
      );
      envelope.error = detail;
      return apply(safeJsonStringify, undefined, [envelope]);
    };
    const success = (value) => {
      const seen = new SafeWeakMap();
      const nodes = record();
      let nextId = 1;

      let propertyCount = 0;
      const encode = (input, depth = 0) => {
        if (depth > ${MAX_BOUNDED_GRAPH_DEPTH}) {
          throw new ContextRangeError("Evaluation result exceeds the clone-safe graph depth limit");
        }
        if (input === null) return tagged("null");
        switch (typeof input) {
          case "undefined":
            return tagged("undefined");
          case "boolean":
            return tagged("boolean", input);
          case "string":
            return tagged("string", input);
          case "number":
            if (apply(safeNumberIsNaN, undefined, [input])) return tagged("number-special", "nan");
            if (input === Infinity) return tagged("number-special", "infinity");
            if (input === -Infinity) return tagged("number-special", "negative-infinity");
            if (apply(safeObjectIs, undefined, [input, -0])) return tagged("number-special", "negative-zero");
            return tagged("number", input);
          case "bigint":
            return tagged("bigint", apply(safeBigIntToString, input, []));
          case "symbol":
          case "function":
            throw new ContextTypeError(
              "Evaluation result is not clone-safe: symbol and function values are unsupported",
            );
          case "object":
            break;
          default:
            throw new ContextTypeError("Evaluation result has an unsupported type");
        }

        // Promise and arbitrary thenable access is deliberately inside the VM
        // timeout. Never inspect a page-owned result from the Worker realm.
        if (typeof input.then === "function") {
          throw new ContextTypeError("Evaluation returned a Promise; this operation requires a synchronous result");
        }

        const previous = apply(safeWeakMapGet, seen, [input]);
        if (previous !== undefined) return tagged("reference", previous);

        const id = nextId++;
        if (id > ${MAX_BOUNDED_GRAPH_NODES}) {
          throw new ContextRangeError("Evaluation result exceeds the clone-safe graph node limit");
        }
        apply(safeWeakMapSet, seen, [input, id]);
        const node = record();
        const properties = record();
        const isArray = apply(safeArrayIsArray, undefined, [input]);
        const prototype = apply(safeGetPrototypeOf, undefined, [input]);
        if (isArray) {
          if (prototype !== ContextArrayPrototype) {
            throw new ContextTypeError("Evaluation result is not clone-safe: arrays must use the page Array prototype");
          }
          node.kind = "array";
          node.length = input.length;
          if (!apply(safeNumberIsSafeInteger, undefined, [node.length]) || node.length > ${MAX_BOUNDED_ARRAY_LENGTH}) {
            throw new ContextRangeError("Evaluation result exceeds the clone-safe array length limit");
          }
        } else {
          if (prototype !== null && prototype !== ContextObjectPrototype) {
            throw new ContextTypeError(
              "Evaluation result is not clone-safe: only plain or null-prototype objects are supported",
            );
          }
          node.kind = "object";
        }
        node.properties = properties;
        nodes[id] = node;

        const keys = apply(safeKeys, undefined, [input]);
        propertyCount += keys.length;
        if (propertyCount > ${MAX_BOUNDED_GRAPH_PROPERTIES}) {
          throw new ContextRangeError("Evaluation result exceeds the clone-safe property limit");
        }
        for (let index = 0; index < keys.length; index += 1) {
          const key = keys[index];
          properties[key] = encode(input[key], depth + 1);
        }
        return tagged("reference", id);
      };

      const envelope = record();
      envelope.ok = true;
      envelope.root = encode(value);
      envelope.nodes = nodes;
      const serialized = apply(safeJsonStringify, undefined, [envelope]);
      if (serialized.length > ${MAX_BOUNDED_SERIALIZED_CHARS}) {
        throw new ContextRangeError("Evaluation result exceeds the clone-safe serialized size limit");
      }
      return serialized;
    };

    const boundedEvaluate = function boundedEvaluate(source) {
      try {
        return success(apply(indirectEval, undefined, [source]));
      } catch (error) {
        return failure(error);
      }
    };
    Object.freeze(boundedEvaluate);
    Object.defineProperty(globalThis, "${BOUNDED_EVALUATE_BINDING}", {
      value: boundedEvaluate,
      writable: false,
      configurable: false,
    });
  })()`,
  { filename: "obscura-bounded-evaluate-install.js" },
);
const installDocumentFacadeScript = new vm.Script(
  `(() => {
    "use strict";
    const hostQueryElement = globalThis.${QUERY_BINDING};
    const hostDocumentHtml = globalThis.${DOCUMENT_HTML_BINDING};
    const safeApply = Reflect.apply;
    const ContextError = Error;
    const ContextTypeError = TypeError;
    const ContextRangeError = RangeError;
    const ContextSyntaxError = SyntaxError;
    const ContextReferenceError = ReferenceError;
    const ContextEvalError = EvalError;
    const ContextURIError = URIError;
    const ContextString = String;

    const translatedError = (record) => {
      const name = typeof record?.name === "string" ? record.name : "Error";
      const message = typeof record?.message === "string" ? record.message : "Obscura bridge operation failed";
      const ErrorConstructor =
        name === "TypeError" ? ContextTypeError :
        name === "RangeError" ? ContextRangeError :
        name === "SyntaxError" ? ContextSyntaxError :
        name === "ReferenceError" ? ContextReferenceError :
        name === "EvalError" ? ContextEvalError :
        name === "URIError" ? ContextURIError : ContextError;
      const error = new ErrorConstructor(message);
      if (ErrorConstructor === ContextError && name !== "Error") error.name = name;
      return error;
    };

    const callHost = (callback, args) => {
      let response;
      try {
        response = safeApply(callback, undefined, args);
      } catch {
        throw new ContextError("Obscura bridge callback failed");
      }
      if (response === null || typeof response !== "object") {
        throw new ContextTypeError("Obscura bridge callback returned an invalid response");
      }
      if (response.ok === true) return response.value;
      if (response.ok === false) throw translatedError(response.error);
      throw new ContextTypeError("Obscura bridge callback returned an invalid response");
    };

    const querySelector = function querySelector(selector) {
      // Coercion can invoke page-owned Symbol.toPrimitive/toString hooks. Keep
      // it in the context so the VM timeout covers the complete operation.
      const selectorText = safeApply(ContextString, undefined, [selector]);
      return callHost(hostQueryElement, [selectorText]);
    };
    Object.freeze(querySelector);

    const getDocumentOuterHtml = function getDocumentOuterHtml() {
      return callHost(hostDocumentHtml, []);
    };
    Object.freeze(getDocumentOuterHtml);

    const documentElement = Object.create(null);
    Object.defineProperty(documentElement, "outerHTML", {
      enumerable: true,
      get: getDocumentOuterHtml,
    });
    Object.freeze(documentElement);

    const document = Object.create(null);
    Object.defineProperties(document, {
      querySelector: {
        enumerable: true,
        value: querySelector,
        writable: false,
      },
      documentElement: {
        enumerable: true,
        value: documentElement,
        writable: false,
      },
    });
    Object.freeze(document);
    Object.defineProperty(globalThis, "document", {
      enumerable: true,
      value: document,
      writable: false,
      configurable: false,
    });
  })()`,
  { filename: "obscura-document-facade.js" },
);

let target;
let metadata;
let persistentRuntime;
let hostContext;
let bridgeRealmKind = null;
let bridgeCore;
let bridgeGeneration = 0;
let bridgeDisposed = 0;
let bridgeDocumentHandle = null;
let bridgePageRevision = null;
let compiledBootstrapRuntime;
let bridgeCapabilityProbePromise;
let portablePlatformOp;
let bridgeTaskHost;
let bridgeTaskLastStatus = null;
let enqueueSerializedWork;
let shuttingDown = false;

function propertyNames(value) {
  const names = new Set();
  let cursor = value;
  for (let depth = 0; cursor && cursor !== Object.prototype && depth < 3; depth += 1, cursor = Object.getPrototypeOf(cursor)) {
    for (const name of Object.getOwnPropertyNames(cursor)) {
      if (name !== "constructor") names.add(name);
    }
  }
  return [...names].sort();
}

function member(object, names) {
  for (const name of names) {
    const value = object?.[name];
    if (typeof value === "function") return { name, fn: value.bind(object) };
  }
  return null;
}

function valueMember(object, names) {
  for (const name of names) {
    const value = object?.[name];
    if (value !== undefined && typeof value !== "function") {
      return { name, value };
    }
  }
  return null;
}

function decodeJsonText(value) {
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

function moduleResult(value) {
  // The migration's Node-API fallback deliberately crosses the isolate
  // boundary as JSON text. Ordinary JS/WASM APIs return structured values and
  // must not have legitimate strings such as "42" silently retyped.
  if (metadata?.kind === "native-addon" || target?.__obscuraNativeJsonText === true) {
    return decodeJsonText(value);
  }
  return value;
}

function sourceText(value, label = "source") {
  if (typeof value !== "string") throw new TypeError(`${label} must be a string`);
  return value;
}

function vmTimeout(value, fallback = 1_000) {
  const timeoutMs = value ?? fallback;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_VM_TIMEOUT_MS) {
    throw new RangeError(`timeoutMs must be an integer between 1 and ${MAX_VM_TIMEOUT_MS} milliseconds`);
  }
  return timeoutMs;
}

function optionalTimeout(value) {
  return value === undefined ? undefined : vmTimeout(value);
}

function synchronousResult(value, label) {
  if ((typeof value === "object" && value !== null) || typeof value === "function") {
    if (typeof value.then === "function") {
      throw new TypeError(`${label} returned a Promise; this operation requires a synchronous result`);
    }
  }
  return value;
}

function boundedEvaluateScript(source, filename) {
  const literal = JSON.stringify(sourceText(source));
  return new vm.Script(`globalThis.${BOUNDED_EVALUATE_BINDING}(${literal})`, { filename });
}

function decodeBoundedEnvelope(serialized, label) {
  if (typeof serialized !== "string") {
    throw new TypeError(`${label} returned an invalid bounded-evaluation envelope`);
  }
  if (serialized.length > MAX_BOUNDED_SERIALIZED_CHARS) {
    throw new RangeError(`${label} exceeded the bounded-evaluation serialized size limit`);
  }

  let envelope;
  try {
    envelope = JSON.parse(serialized);
  } catch {
    throw new TypeError(`${label} returned malformed bounded-evaluation JSON`);
  }
  if (envelope === null || typeof envelope !== "object" || typeof envelope.ok !== "boolean") {
    throw new TypeError(`${label} returned an invalid bounded-evaluation envelope`);
  }
  if (!envelope.ok) {
    const name = typeof envelope.error?.name === "string" ? envelope.error.name : "Error";
    const message =
      typeof envelope.error?.message === "string"
        ? envelope.error.message
        : "Obscura page evaluation failed";
    const error = new Error(message);
    error.name = name;
    throw error;
  }
  if (envelope.nodes === null || typeof envelope.nodes !== "object") {
    throw new TypeError(`${label} returned no bounded-evaluation node table`);
  }
  const nodeKeys = Object.keys(envelope.nodes);
  if (nodeKeys.length > MAX_BOUNDED_GRAPH_NODES) {
    throw new RangeError(`${label} exceeded the bounded-evaluation graph node limit`);
  }

  const decoded = new Map();
  let propertyCount = 0;
  const decode = (encoded, depth = 0) => {
    if (depth > MAX_BOUNDED_GRAPH_DEPTH) {
      throw new RangeError(`${label} exceeded the bounded-evaluation graph depth limit`);
    }
    if (encoded === null || typeof encoded !== "object" || typeof encoded.t !== "string") {
      throw new TypeError(`${label} returned an invalid encoded value`);
    }
    switch (encoded.t) {
      case "null":
        return null;
      case "undefined":
        return undefined;
      case "boolean":
        if (typeof encoded.v !== "boolean") throw new TypeError(`${label} returned an invalid boolean`);
        return encoded.v;
      case "string":
        if (typeof encoded.v !== "string") throw new TypeError(`${label} returned an invalid string`);
        return encoded.v;
      case "number":
        if (typeof encoded.v !== "number" || !Number.isFinite(encoded.v)) {
          throw new TypeError(`${label} returned an invalid number`);
        }
        return encoded.v;
      case "number-special":
        if (encoded.v === "nan") return Number.NaN;
        if (encoded.v === "infinity") return Infinity;
        if (encoded.v === "negative-infinity") return -Infinity;
        if (encoded.v === "negative-zero") return -0;
        throw new TypeError(`${label} returned an invalid special number`);
      case "bigint":
        if (typeof encoded.v !== "string" || !/^-?[0-9]+$/.test(encoded.v)) {
          throw new TypeError(`${label} returned an invalid bigint`);
        }
        return BigInt(encoded.v);
      case "reference":
        break;
      default:
        throw new TypeError(`${label} returned an unknown encoded value type`);
    }

    const id = encoded.v;
    if (!Number.isSafeInteger(id) || id < 1) {
      throw new TypeError(`${label} returned an invalid object reference`);
    }
    if (decoded.has(id)) return decoded.get(id);
    const node = envelope.nodes[id];
    if (node === null || typeof node !== "object" || node.properties === null || typeof node.properties !== "object") {
      throw new TypeError(`${label} returned a missing object node`);
    }

    let value;
    if (node.kind === "array") {
      if (
        !Number.isSafeInteger(node.length) ||
        node.length < 0 ||
        node.length > MAX_BOUNDED_ARRAY_LENGTH
      ) {
        throw new TypeError(`${label} returned an invalid array length`);
      }
      value = [];
      value.length = node.length;
    } else if (node.kind === "object") {
      value = {};
    } else {
      throw new TypeError(`${label} returned an invalid object node kind`);
    }
    decoded.set(id, value);
    const propertyKeys = Object.keys(node.properties);
    propertyCount += propertyKeys.length;
    if (propertyCount > MAX_BOUNDED_GRAPH_PROPERTIES) {
      throw new RangeError(`${label} exceeded the bounded-evaluation property limit`);
    }
    for (const key of propertyKeys) {
      Object.defineProperty(value, key, {
        configurable: true,
        enumerable: true,
        writable: true,
        value: decode(node.properties[key], depth + 1),
      });
    }
    return value;
  };

  return decode(envelope.root);
}

function runBoundedEvaluate(script, context, timeoutMs, label) {
  timeoutMs = vmTimeout(timeoutMs);
  let serialized;
  try {
    serialized = script.runInContext(context, { timeout: timeoutMs });
  } catch {
    // The installed evaluator catches and serializes every page-thrown value.
    // A value that escapes runInContext is a VM timeout/termination or a host
    // failure, so replace it without inspecting page-controlled properties.
    throw new Error(`${label} timed out or was terminated after ${timeoutMs}ms`);
  }
  return decodeBoundedEnvelope(serialized, label);
}

function describeApi() {
  const version = member(target, VERSION_NAMES) ?? valueMember(target, VERSION_NAMES);
  const abiVersion = member(target, ABI_VERSION_NAMES) ?? valueMember(target, ABI_VERSION_NAMES);
  const probe = member(target, PROBE_NAMES);
  const factory = member(target, FACTORY_NAMES);
  const constructor = member(target, CONSTRUCTOR_NAMES);
  const evaluate = member(target, EVALUATE_NAMES);

  return {
    exports: propertyNames(target),
    capabilities: {
      version: version?.name ?? null,
      abiVersion: abiVersion?.name ?? null,
      probe: probe?.name ?? null,
      runtimeFactory: factory?.name ?? constructor?.name ?? null,
      moduleEvaluate: evaluate?.name ?? null,
      obscuraCore: member(target, ["ObscuraCore"])?.name ?? null,
      platformOpAbiVersion: member(target, [PLATFORM_OP_ABI_VERSION_NAME])?.name ?? null,
      platformOp: member(target, [PLATFORM_OP_NAME])?.name ?? null,
    },
  };
}

async function requireCompatibleCoreAbi() {
  if (!member(target, ["ObscuraCore"])) return;

  const abiVersion = member(target, ABI_VERSION_NAMES) ?? valueMember(target, ABI_VERSION_NAMES);
  if (!abiVersion) {
    const error = new Error(
      `ObscuraCore requires ABI version ${REQUIRED_CORE_ABI_VERSION}, but the module exposes no ABI version`,
    );
    error.code = "ERR_OBSCURA_WASM_ABI";
    throw error;
  }

  const actual = abiVersion.fn ? await abiVersion.fn() : abiVersion.value;
  if (!Number.isSafeInteger(actual) || actual !== REQUIRED_CORE_ABI_VERSION) {
    const error = new Error(
      `ObscuraCore requires ABI version ${REQUIRED_CORE_ABI_VERSION}, but the module exposes ${String(actual)}`,
    );
    error.code = "ERR_OBSCURA_WASM_ABI";
    throw error;
  }
}

async function createRuntime() {
  const factory = member(target, FACTORY_NAMES);
  if (factory) {
    const runtime = await factory.fn();
    if ((typeof runtime !== "object" && typeof runtime !== "function") || runtime === null) {
      throw new TypeError(`${factory.name} returned no runtime object`);
    }
    if (!member(runtime, DISPOSE_NAMES)) {
      throw new TypeError(`${factory.name} returned a runtime with no deterministic disposal method`);
    }
    return runtime;
  }

  const constructor = member(target, CONSTRUCTOR_NAMES);
  if (constructor) {
    const runtime = new constructor.fn();
    if (!member(runtime, DISPOSE_NAMES)) {
      throw new TypeError(`${constructor.name} created a runtime with no deterministic disposal method`);
    }
    return runtime;
  }

  return null;
}

async function disposeRuntime(runtime) {
  if (!runtime) return null;
  const dispose = member(runtime, DISPOSE_NAMES);
  if (!dispose) return null;
  await dispose.fn();
  return dispose.name;
}

async function evaluateWithModule(source, timeoutMs) {
  source = sourceText(source);
  timeoutMs = optionalTimeout(timeoutMs);
  const direct = member(target, EVALUATE_NAMES);
  if (direct) return moduleResult(await direct.fn(source, timeoutMs));

  if (!persistentRuntime) persistentRuntime = await createRuntime();
  if (!persistentRuntime) throw new Error("Module has neither an evaluate export nor a runtime factory");

  const evaluate = member(persistentRuntime, EVALUATE_NAMES);
  if (!evaluate) {
    const runtime = persistentRuntime;
    persistentRuntime = null;
    const message = `Created runtime has no evaluate method; found: ${propertyNames(runtime).join(", ")}`;
    try {
      await disposeRuntime(runtime);
    } catch (cleanupError) {
      throw new AggregateError([new Error(message), cleanupError], "Invalid runtime cleanup failed");
    }
    throw new Error(message);
  }
  return moduleResult(await evaluate.fn(source, timeoutMs));
}

function getHostContext() {
  if (hostContext) return hostContext;
  const sandbox = Object.create(null);
  hostContext = vm.createContext(sandbox, {
    name: "obscura-page",
    codeGeneration: { strings: true, wasm: false },
    microtaskMode: "afterEvaluate",
  });
  installBoundedEvaluateScript.runInContext(hostContext, { timeout: 1_000 });
  return hostContext;
}

async function getCompiledBootstrapRuntime() {
  if (compiledBootstrapRuntime) return compiledBootstrapRuntime;
  const bootstrapPath = workerData.bootstrapPath ?? DEFAULT_BOOTSTRAP_PATH;
  let source;
  try {
    source = await readFile(bootstrapPath, "utf8");
  } catch (cause) {
    const origin = workerData.bootstrapPath ? "configured bootstrapPath" : "checkout-default bootstrap path";
    const error = new Error(
      `Unable to load Obscura bootstrap source from ${origin} ${bootstrapPath}; pass bootstrapPath when the harness is packaged outside the repository checkout`,
      { cause },
    );
    error.code = cause?.code ?? "ERR_OBSCURA_BOOTSTRAP_SOURCE";
    throw error;
  }
  compiledBootstrapRuntime = compileBootstrapRuntime(source, { filename: bootstrapPath });
  return compiledBootstrapRuntime;
}

function hostEvaluate(source, timeoutMs = 1_000) {
  const script = boundedEvaluateScript(source, "obscura-evaluate.js");
  return runBoundedEvaluate(script, getHostContext(), timeoutMs, "hostEvaluate");
}

function syncCall(callable, ...args) {
  const value = callable.fn(...args);
  return synchronousResult(value, callable.name);
}

function bridgeApi(core = bridgeCore) {
  return {
    querySnapshot: member(core, QUERY_SNAPSHOT_NAMES),
    queryText: member(core, QUERY_TEXT_NAMES),
    queryHtml: member(core, QUERY_HTML_NAMES),
    documentElementHtml: member(core, DOCUMENT_ELEMENT_HTML_NAMES),
    domOp: member(core, DOM_OP_NAMES),
    domBatch: member(core, DOM_BATCH_NAMES),
    pageRevision: member(core, PAGE_REVISION_NAMES),
    documentHandle: member(core, DOCUMENT_HANDLE_NAMES),
    setDocumentMetadata: member(core, SET_DOCUMENT_METADATA_NAMES),
    seedRenderResource: member(core, SEED_RENDER_RESOURCE_NAMES),
    seedMissingRenderResource: member(core, SEED_MISSING_RENDER_RESOURCE_NAMES),
    renderResourceRequests: member(core, RENDER_RESOURCE_REQUEST_NAMES),
    seedRenderImageResource: member(core, SEED_RENDER_IMAGE_RESOURCE_NAMES),
    seedMissingRenderImageResource: member(core, SEED_MISSING_RENDER_IMAGE_RESOURCE_NAMES),
    screenshotPng: member(core, SCREENSHOT_PNG_NAMES),
    dispose: member(core, DISPOSE_NAMES),
  };
}

async function requireRenderResourceCompatibility(api, requiredMethod) {
  const capabilities = await getBridgeCapabilityProbe();
  if (!capabilities) {
    throw renderAbiError("ObscuraCore render resource ABI requires a machine-readable capability probe");
  }
  if (capabilities.renderAbiVersion !== REQUIRED_RENDER_ABI_VERSION) {
    throw renderAbiError(
      `ObscuraCore render requires ABI version ${REQUIRED_RENDER_ABI_VERSION}, but the probe exposes ${String(capabilities.renderAbiVersion)}`,
    );
  }
  if (capabilities.renderResourceRequestAbiVersion !== REQUIRED_RENDER_RESOURCE_REQUEST_ABI_VERSION) {
    throw renderAbiError(
      `ObscuraCore render resource requests require ABI version ${REQUIRED_RENDER_RESOURCE_REQUEST_ABI_VERSION}, but the probe exposes ${String(capabilities.renderResourceRequestAbiVersion)}`,
    );
  }
  if (capabilities.renderResourceRequests !== true) {
    throw renderAbiError("ObscuraCore render resources require renderResourceRequests=true");
  }
  const names = {
    seedRenderResource: "seed_render_resource/seedRenderResource",
    seedMissingRenderResource: "seed_missing_render_resource/seedMissingRenderResource",
    renderResourceRequests: "render_resource_requests/renderResourceRequests",
    seedRenderImageResource: "seed_render_image_resource/seedRenderImageResource",
    seedMissingRenderImageResource:
      "seed_missing_render_image_resource/seedMissingRenderImageResource",
  };
  for (const method of Object.keys(names)) {
    if (!api[method]) {
      throw renderAbiError(`ObscuraCore does not expose ${names[method]}`);
    }
  }
  if (requiredMethod && !Object.hasOwn(names, requiredMethod)) {
    throw new TypeError(`unknown render resource method ${requiredMethod}`);
  }
}

function bridgeAbiError(message) {
  const error = new Error(message);
  error.code = "ERR_OBSCURA_WASM_DOM_ABI";
  return error;
}

function renderAbiError(message) {
  const error = new Error(message);
  error.code = "ERR_OBSCURA_WASM_RENDER_ABI";
  return error;
}

async function requireRenderCompatibility(api, requiredMethod) {
  const capabilities = await getBridgeCapabilityProbe();
  if (!capabilities) {
    throw renderAbiError("ObscuraCore render ABI requires a machine-readable capability probe");
  }
  if (capabilities.renderAbiVersion !== REQUIRED_RENDER_ABI_VERSION) {
    throw renderAbiError(
      `ObscuraCore render requires ABI version ${REQUIRED_RENDER_ABI_VERSION}, but the probe exposes ${String(capabilities.renderAbiVersion)}`,
    );
  }
  if (capabilities.screenshotPng !== true) {
    throw renderAbiError("ObscuraCore render requires screenshotPng=true");
  }
  if (requiredMethod && !api[requiredMethod]) {
    const names =
      requiredMethod === "screenshotPng"
        ? "screenshot_png/screenshotPng"
        : requiredMethod === "seedRenderResource"
          ? "seed_render_resource/seedRenderResource"
          : "seed_missing_render_resource/seedMissingRenderResource";
    throw renderAbiError(`ObscuraCore does not expose ${names}`);
  }
}

function platformAbiError(message) {
  const error = new Error(message);
  error.code = "ERR_OBSCURA_WASM_PLATFORM_ABI";
  return error;
}

function requirePortablePlatformCompatibility() {
  if (portablePlatformOp) return portablePlatformOp;
  const version = member(target, [PLATFORM_OP_ABI_VERSION_NAME]);
  if (!version) {
    throw platformAbiError(
      `Portable bootstrap requires ${PLATFORM_OP_ABI_VERSION_NAME}() ABI version ${REQUIRED_PLATFORM_OP_ABI_VERSION}`,
    );
  }
  const actual = syncCall(version);
  if (!Number.isSafeInteger(actual) || actual !== REQUIRED_PLATFORM_OP_ABI_VERSION) {
    throw platformAbiError(
      `Portable bootstrap platform_op requires ABI version ${REQUIRED_PLATFORM_OP_ABI_VERSION}, but the module exposes ${String(actual)}`,
    );
  }
  const operation = member(target, [PLATFORM_OP_NAME]);
  if (!operation) {
    throw platformAbiError(`Portable bootstrap requires a synchronous ${PLATFORM_OP_NAME}() export`);
  }
  portablePlatformOp = operation;
  return operation;
}

function decodedBase64Length(value, label) {
  if (typeof value !== "string" || value.length % 4 !== 0) {
    throw new TypeError(`${label} must be standard base64`);
  }
  const padding = value.endsWith("==") ? 2 : (value.endsWith("=") ? 1 : 0);
  const bodyLength = value.length - padding;
  for (let index = 0; index < bodyLength; index++) {
    const code = value.charCodeAt(index);
    const valid = (code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a) ||
      (code >= 0x30 && code <= 0x39) || code === 0x2b || code === 0x2f;
    if (!valid) throw new TypeError(`${label} must be standard base64`);
  }
  for (let index = bodyLength; index < value.length; index++) {
    if (value.charCodeAt(index) !== 0x3d) throw new TypeError(`${label} must be standard base64`);
  }
  if ((padding === 1 && bodyLength % 4 !== 3) || (padding === 2 && bodyLength % 4 !== 2)) {
    throw new TypeError(`${label} must be standard base64`);
  }
  return (value.length / 4) * 3 - padding;
}

function validatePlatformRequest(command, requestJson) {
  const byteFields = PLATFORM_BYTE_FIELDS[command] ?? [];
  const needsNumericValidation = command === "op_random_bytes" ||
    command === "op_subtle_pbkdf2" || command === "op_subtle_hkdf";
  // URL/domain/label operations are latency-sensitive and contain no binary
  // allocation controls. Their exact schema is validated once by WASM.
  if (byteFields.length === 0 && !needsNumericValidation) return;
  let request;
  try {
    request = JSON.parse(requestJson);
  } catch {
    throw new TypeError("platform request must be valid JSON");
  }
  if (request === null || typeof request !== "object" || Array.isArray(request)) {
    throw new TypeError("platform request must be an object");
  }
  let aggregateBinaryBytes = 0;
  for (const field of byteFields) {
    aggregateBinaryBytes += decodedBase64Length(request[field], `platform request ${field}`);
    if (aggregateBinaryBytes > MAX_PLATFORM_BINARY_BYTES) {
      throw new RangeError(
        `platform request binary payload exceeds the ${MAX_PLATFORM_BINARY_BYTES}-byte ABI limit`,
      );
    }
  }
  if (command === "op_random_bytes") {
    if (!Number.isSafeInteger(request.length) || request.length < 0 || request.length > MAX_PLATFORM_RANDOM_BYTES) {
      throw new RangeError(
        `platform random length must be an integer between 0 and ${MAX_PLATFORM_RANDOM_BYTES} bytes`,
      );
    }
  }
  if (command === "op_subtle_pbkdf2") {
    if (!Number.isSafeInteger(request.iterations) ||
        request.iterations < 1 || request.iterations > MAX_PLATFORM_PBKDF2_ITERATIONS) {
      throw new RangeError(
        `platform PBKDF2 iterations must be an integer between 1 and ${MAX_PLATFORM_PBKDF2_ITERATIONS}`,
      );
    }
    const digestBytes = request.hash === "SHA-1" ? 20 :
      request.hash === "SHA-256" ? 32 :
      request.hash === "SHA-384" ? 48 :
      request.hash === "SHA-512" ? 64 : 0;
    if (digestBytes > 0 && Number.isSafeInteger(request.length) && request.length >= 0) {
      const blocks = Math.ceil(request.length / digestBytes);
      const work = request.iterations * blocks;
      if (!Number.isSafeInteger(work) || work > MAX_PLATFORM_PBKDF2_WORK_UNITS) {
        throw new RangeError(
          `platform PBKDF2 request exceeds the ${MAX_PLATFORM_PBKDF2_WORK_UNITS}-unit work limit`,
        );
      }
    }
  }
  if (command === "op_subtle_pbkdf2" || command === "op_subtle_hkdf") {
    if (!Number.isSafeInteger(request.length) ||
        request.length < 0 || request.length > MAX_PLATFORM_KDF_OUTPUT_BYTES) {
      throw new RangeError(
        `platform KDF output must be an integer between 0 and ${MAX_PLATFORM_KDF_OUTPUT_BYTES} bytes`,
      );
    }
  }
}

function platformOperation(command, requestJson) {
  requireBoundedString(command, MAX_PLATFORM_COMMAND_BYTES, "platform command");
  if (!PLATFORM_OP_COMMAND_SET.has(command)) {
    throw new TypeError(`Unsupported portable platform command ${JSON.stringify(command)}`);
  }
  requireBoundedString(requestJson, MAX_PLATFORM_REQUEST_BYTES, "platform request");
  validatePlatformRequest(command, requestJson);
  const value = syncCall(requirePortablePlatformCompatibility(), command, requestJson);
  requireBoundedString(value, MAX_PLATFORM_RESPONSE_BYTES, "platform response");
  if (PLATFORM_BYTE_RESULT_COMMANDS.has(command) &&
      decodedBase64Length(value, "platform response") > MAX_PLATFORM_BINARY_BYTES) {
    throw new RangeError(
      `platform response binary payload exceeds the ${MAX_PLATFORM_BINARY_BYTES}-byte ABI limit`,
    );
  }
  return value;
}

async function getBridgeCapabilityProbe() {
  if (bridgeCapabilityProbePromise) return bridgeCapabilityProbePromise;
  bridgeCapabilityProbePromise = (async () => {
    const probe = member(target, PROBE_NAMES);
    if (!probe) return null;
    const serialized = await probe.fn();
    let capabilities = serialized;
    if (typeof serialized === "string") {
      try {
        capabilities = JSON.parse(serialized);
      } catch {
        throw bridgeAbiError("ObscuraCore stateful bridge probe returned malformed JSON");
      }
    }
    if (capabilities === null || typeof capabilities !== "object" || Array.isArray(capabilities)) {
      throw bridgeAbiError("ObscuraCore stateful bridge probe must return an object");
    }
    return capabilities;
  })();
  return bridgeCapabilityProbePromise;
}

async function requireStatefulBridgeCompatibility(api) {
  const capabilities = await getBridgeCapabilityProbe();
  if (!capabilities) {
    throw bridgeAbiError("ObscuraCore stateful bridge requires a machine-readable capability probe");
  }
  if (capabilities.stableNodeHandles !== true) {
    throw bridgeAbiError("ObscuraCore stateful bridge requires stableNodeHandles=true");
  }
  if (api.domOp && capabilities.domOpAbiVersion !== REQUIRED_DOM_OP_ABI_VERSION) {
    throw bridgeAbiError(
      `ObscuraCore dom_op/domOp requires ABI version ${REQUIRED_DOM_OP_ABI_VERSION}, but the probe exposes ${String(capabilities.domOpAbiVersion)}`,
    );
  }
  if (api.domBatch && capabilities.domBatchAbiVersion !== REQUIRED_DOM_BATCH_ABI_VERSION) {
    throw bridgeAbiError(
      `ObscuraCore dom_batch/domBatch requires ABI version ${REQUIRED_DOM_BATCH_ABI_VERSION}, but the probe exposes ${String(capabilities.domBatchAbiVersion)}`,
    );
  }
}

async function requireDocumentMetadataCompatibility(api) {
  const capabilities = await getBridgeCapabilityProbe();
  if (capabilities?.documentMetadataAbiVersion !== REQUIRED_DOCUMENT_METADATA_ABI_VERSION) {
    throw bridgeAbiError(
      `ObscuraCore document metadata requires ABI version ${REQUIRED_DOCUMENT_METADATA_ABI_VERSION}, but the probe exposes ${String(capabilities?.documentMetadataAbiVersion)}`,
    );
  }
  if (!api.setDocumentMetadata) {
    throw bridgeAbiError("ObscuraCore does not expose set_document_metadata/setDocumentMetadata");
  }
}

function requireUnsignedU32(value, label) {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new TypeError(`${label} must return an unsigned 32-bit integer`);
  }
  return value;
}

function statefulBridgeApi(core = requireBridgeCore()) {
  const api = bridgeApi(core);
  if ((!api.domOp && !api.domBatch) || !api.pageRevision || !api.documentHandle) {
    throw new Error(
      "ObscuraCore stateful bridge requires dom_op/domOp or dom_batch/domBatch, plus page_revision/pageRevision and document_handle/documentHandle",
    );
  }
  return api;
}

function readBridgeIdentity(core = requireBridgeCore(), api = statefulBridgeApi(core)) {
  const revision = requireUnsignedU32(syncCall(api.pageRevision), "ObscuraCore page revision");
  const documentHandle = requireUnsignedU32(syncCall(api.documentHandle), "ObscuraCore document handle");
  if (documentHandle === 0) throw new TypeError("ObscuraCore document handle must be non-zero");
  return { revision, documentHandle };
}

function synchronizeBridgeIdentity(expectedGeneration = bridgeGeneration, expectedDocumentHandle = bridgeDocumentHandle) {
  if (!bridgeCore || expectedGeneration !== bridgeGeneration) {
    throw new Error("Stale Obscura document bridge");
  }
  const identity = readBridgeIdentity();
  if (expectedDocumentHandle !== null && identity.documentHandle !== expectedDocumentHandle) {
    throw new Error("Stale Obscura document handle");
  }
  bridgePageRevision = identity.revision;
  bridgeDocumentHandle = identity.documentHandle;
  return identity;
}

function stalePageError(message) {
  const error = new Error(message);
  error.code = "ERR_OBSCURA_STALE_PAGE";
  return error;
}

function requireExpectedPage(value) {
  if (value === undefined) return;
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("expectedPage must be an object");
  }
  const { generation, documentHandle, revision } = value;
  if (generation !== undefined && (!Number.isSafeInteger(generation) || generation < 0)) {
    throw new TypeError("expectedPage generation must be a non-negative safe integer");
  }
  if (documentHandle !== undefined) {
    requireUnsignedU32(documentHandle, "expectedPage document handle");
    if (documentHandle === 0) throw new TypeError("expectedPage document handle must be non-zero");
  }
  if (revision !== undefined) requireUnsignedU32(revision, "expectedPage revision");
  if (generation !== undefined && generation !== bridgeGeneration) {
    throw stalePageError(`Expected Obscura bridge generation ${generation}, but the current generation is ${bridgeGeneration}`);
  }
  const identity = synchronizeBridgeIdentity();
  if (documentHandle !== undefined && documentHandle !== identity.documentHandle) {
    throw stalePageError(
      `Expected Obscura document handle ${documentHandle}, but the current handle is ${identity.documentHandle}`,
    );
  }
  if (revision !== undefined && revision !== identity.revision) {
    throw stalePageError(`Expected Obscura page revision ${revision}, but the current revision is ${identity.revision}`);
  }
}

function requireDomOperation(operation, index) {
  if (!Array.isArray(operation) || operation.length !== 3) {
    throw new TypeError(`DOM batch operation ${index} must be an exact three-string tuple`);
  }
  const [command, arg1, arg2] = operation;
  requireBoundedString(command, MAX_DOM_COMMAND_BYTES, `DOM batch operation ${index} command`);
  requireBoundedString(arg1, MAX_DOM_ARGUMENT_BYTES, `DOM batch operation ${index} arg1`);
  requireBoundedString(arg2, MAX_DOM_ARGUMENT_BYTES, `DOM batch operation ${index} arg2`);
  return [command, arg1, arg2];
}

function encodeDomBatchRequest(operations) {
  if (!Array.isArray(operations)) throw new TypeError("DOM batch operations must be an array");
  if (operations.length > MAX_DOM_BATCH_OPERATIONS) {
    throw new RangeError(`DOM batch exceeds the ${MAX_DOM_BATCH_OPERATIONS}-operation ABI limit`);
  }
  const normalized = operations.map(requireDomOperation);
  const request = JSON.stringify(normalized);
  requireBoundedString(request, MAX_DOM_BATCH_BYTES, "DOM batch request");
  return { normalized, request };
}

function decodeDomBatchResponse(serialized, expectedLength) {
  requireBoundedString(serialized, MAX_RETURNED_STRING_BYTES, "DOM batch response");
  let results;
  try {
    results = JSON.parse(serialized);
  } catch {
    throw new TypeError("ObscuraCore dom_batch/domBatch returned malformed JSON");
  }
  if (!Array.isArray(results) || results.length !== expectedLength) {
    throw new TypeError("ObscuraCore dom_batch/domBatch returned the wrong number of results");
  }
  for (let index = 0; index < results.length; index += 1) {
    requireBoundedString(results[index], MAX_RETURNED_STRING_BYTES, `DOM batch result ${index}`);
  }
  return results;
}

function domBatch(operations, expectedGeneration = bridgeGeneration, expectedDocumentHandle = bridgeDocumentHandle) {
  const { normalized, request } = encodeDomBatchRequest(operations);
  synchronizeBridgeIdentity(expectedGeneration, expectedDocumentHandle);
  const api = statefulBridgeApi();
  let results;
  if (api.domBatch) {
    results = decodeDomBatchResponse(syncCall(api.domBatch, request), normalized.length);
  } else {
    results = normalized.map(([command, arg1, arg2], index) => {
      const value = syncCall(api.domOp, command, arg1, arg2);
      return requireBoundedString(value, MAX_RETURNED_STRING_BYTES, `DOM operation result ${index}`);
    });
  }
  const identity = synchronizeBridgeIdentity(expectedGeneration, expectedDocumentHandle);
  return { results, ...identity };
}

function domOperation(command, arg1, arg2, expectedGeneration = bridgeGeneration, expectedDocumentHandle = bridgeDocumentHandle) {
  [command, arg1, arg2] = requireDomOperation([command, arg1, arg2], 0);
  // Prefer the single-operation entry point for bootstrap's synchronous hot
  // path. A batch-only core remains compatible through an exact one-record
  // request without changing the native op_dom string contract.
  synchronizeBridgeIdentity(expectedGeneration, expectedDocumentHandle);
  const api = statefulBridgeApi();
  let result;
  if (api.domOp) {
    result = requireBoundedString(
      syncCall(api.domOp, command, arg1, arg2),
      MAX_RETURNED_STRING_BYTES,
      "DOM operation result",
    );
  } else {
    [result] = decodeDomBatchResponse(
      syncCall(api.domBatch, encodeDomBatchRequest([[command, arg1, arg2]]).request),
      1,
    );
  }
  synchronizeBridgeIdentity(expectedGeneration, expectedDocumentHandle);
  return result;
}

function requireBridgeCore() {
  if (!bridgeCore) throw new Error("No ObscuraCore is loaded; supply html to bridgeEvaluate first");
  return bridgeCore;
}

function queryElement(selector) {
  requireBridgeCore();
  requireBoundedString(selector, MAX_SELECTOR_BYTES, "selector");
  const api = bridgeApi();
  let outerHTML;
  let textContent;
  if (api.querySnapshot) {
    const snapshot = syncCall(api.querySnapshot, selector);
    if (snapshot == null) return null;
    if (!Array.isArray(snapshot) || snapshot.length !== 2) {
      throw new TypeError("ObscuraCore query_snapshot/querySnapshot must return a two-string array or nullish value");
    }
    [outerHTML, textContent] = snapshot;
  } else {
    if (!api.queryText || !api.queryHtml) {
      throw new Error(
        "ObscuraCore must expose query_snapshot/querySnapshot or both query_text/queryText and query_html/queryHtml",
      );
    }
    outerHTML = syncCall(api.queryHtml, selector);
    if (outerHTML == null) return null;
    textContent = syncCall(api.queryText, selector);
  }
  requireBoundedString(outerHTML, MAX_RETURNED_STRING_BYTES, "query outerHTML");
  requireBoundedString(textContent, MAX_RETURNED_STRING_BYTES, "query textContent");

  const element = Object.create(null);
  Object.defineProperties(element, {
    textContent: { enumerable: true, value: textContent ?? "", writable: false },
    outerHTML: { enumerable: true, value: outerHTML, writable: false },
  });
  return Object.freeze(element);
}

function documentOuterHtml() {
  requireBridgeCore();
  const html = bridgeApi().documentElementHtml;
  if (!html) {
    throw new Error(
      "ObscuraCore must expose document_element_html/documentElementHtml for document.documentElement.outerHTML",
    );
  }
  const value = syncCall(html);
  return requireBoundedString(value, MAX_RETURNED_STRING_BYTES, "serialized document element");
}

function bridgeErrorRecord(error) {
  let name = "Error";
  let message = "Obscura bridge operation failed";
  try {
    if (typeof error?.name === "string") name = error.name;
  } catch {
    // Keep the context-independent fallback.
  }
  try {
    if (typeof error?.message === "string") message = error.message;
    else if (error != null) message = String(error);
  } catch {
    // Keep the context-independent fallback.
  }
  const record = Object.create(null);
  Object.defineProperties(record, {
    name: { enumerable: true, value: name },
    message: { enumerable: true, value: message },
  });
  return Object.freeze(record);
}

function bridgeResponse(generation, call) {
  const response = Object.create(null);
  try {
    if (!bridgeCore || generation !== bridgeGeneration) {
      throw new Error("Stale Obscura document bridge");
    }
    Object.defineProperties(response, {
      ok: { enumerable: true, value: true },
      value: { enumerable: true, value: call() },
    });
  } catch (error) {
    Object.defineProperties(response, {
      ok: { enumerable: true, value: false },
      error: { enumerable: true, value: bridgeErrorRecord(error) },
    });
  }
  return Object.freeze(response);
}

function installDocumentFacade() {
  if (bridgeRealmKind === "bootstrap") {
    throw new Error("The bootstrap page realm cannot install the legacy document facade");
  }
  const context = getHostContext();
  if (Object.hasOwn(context, "document")) {
    bridgeRealmKind = "legacy";
    return;
  }
  const generation = bridgeGeneration;
  Object.defineProperties(context, {
    [QUERY_BINDING]: {
      value: (selector) => bridgeResponse(generation, () => queryElement(selector)),
      configurable: true,
    },
    [DOCUMENT_HTML_BINDING]: {
      value: () => bridgeResponse(generation, documentOuterHtml),
      configurable: true,
    },
  });
  try {
    installDocumentFacadeScript.runInContext(context, { timeout: 1_000 });
  } catch (error) {
    hostContext = null;
    bridgeRealmKind = null;
    throw error;
  } finally {
    Reflect.deleteProperty(context, QUERY_BINDING);
    Reflect.deleteProperty(context, DOCUMENT_HTML_BINDING);
  }
  bridgeRealmKind = "legacy";
}

async function disposeBridgeCore() {
  const core = bridgeCore;
  bridgeCore = null;
  bridgeDocumentHandle = null;
  bridgePageRevision = null;
  if (bridgeTaskHost) {
    bridgeTaskHost.close();
    bridgeTaskLastStatus = bridgeTaskHost.status();
    bridgeTaskHost = null;
  }
  // Loading or releasing a document is a page boundary. Discard the old V8
  // realm so globals and queued microtask state cannot leak into the next page.
  hostContext = null;
  bridgeRealmKind = null;
  if (!core) return null;
  const dispose = member(core, DISPOSE_NAMES);
  if (!dispose) return null;
  await dispose.fn();
  bridgeDisposed += 1;
  return dispose.name;
}

function normalizeDocumentMetadata(value) {
  if (value === undefined) return null;
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("documentMetadata must be an object");
  }
  const metadata = {
    url: value.url ?? "about:blank",
    referrer: value.referrer ?? "",
    encoding: value.encoding ?? "UTF-8",
  };
  requireBoundedString(metadata.url, MAX_DOCUMENT_METADATA_BYTES, "document URL");
  requireBoundedString(metadata.referrer, MAX_DOCUMENT_METADATA_BYTES, "document referrer");
  requireBoundedString(metadata.encoding, MAX_DOCUMENT_METADATA_BYTES, "document encoding");
  return metadata;
}

async function applyDocumentMetadata(core, metadata) {
  metadata = normalizeDocumentMetadata(metadata);
  if (!metadata) return false;
  const api = bridgeApi(core);
  await requireDocumentMetadataCompatibility(api);
  syncCall(api.setDocumentMetadata, metadata.url, metadata.referrer, metadata.encoding);
  return true;
}

async function replaceBridgeCore(html, documentMetadata) {
  const constructor = member(target, ["ObscuraCore"]);
  if (!constructor) {
    throw new Error("Module has no ObscuraCore constructor");
  }
  html = requireBoundedString(html, MAX_HTML_INPUT_BYTES, "HTML input");
  const next = new constructor.fn(html);
  const nextApi = bridgeApi(next);
  const hasQueryApi = nextApi.querySnapshot || (nextApi.queryText && nextApi.queryHtml);
  const hasAnyStatefulApi = Boolean(
    nextApi.domOp || nextApi.domBatch || nextApi.pageRevision || nextApi.documentHandle,
  );
  const hasStatefulApi = Boolean(
    (nextApi.domOp || nextApi.domBatch) && nextApi.pageRevision && nextApi.documentHandle,
  );
  if (
    (!hasQueryApi && !hasStatefulApi) ||
    (hasQueryApi && !nextApi.documentElementHtml) ||
    (hasAnyStatefulApi && !hasStatefulApi) ||
    !nextApi.dispose
  ) {
    await disposeRuntime(next);
    throw new Error(
      "ObscuraCore must expose a complete legacy query bridge or a complete stateful dom_op/dom_batch bridge, plus free/dispose",
    );
  }
  let nextIdentity = null;
  try {
    if (hasStatefulApi) {
      await requireStatefulBridgeCompatibility(nextApi);
    }
    await applyDocumentMetadata(next, documentMetadata);
    if (hasStatefulApi) nextIdentity = readBridgeIdentity(next, nextApi);
  } catch (error) {
    try {
      await disposeRuntime(next);
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], "Invalid document metadata cleanup failed");
    }
    throw error;
  }
  try {
    await disposeBridgeCore();
  } catch (error) {
    try {
      await disposeRuntime(next);
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], "Failed to replace and clean up ObscuraCore");
    }
    throw error;
  }
  bridgeCore = next;
  bridgeGeneration += 1;
  bridgeTaskLastStatus = null;
  if (nextIdentity) {
    bridgeDocumentHandle = nextIdentity.documentHandle;
    bridgePageRevision = nextIdentity.revision;
  }
}

async function bridgeStatus() {
  const api = bridgeCore ? bridgeApi() : {};
  if (bridgeCore && (api.domOp || api.domBatch)) synchronizeBridgeIdentity();
  let capabilities = null;
  if (bridgeCore) {
    try {
      capabilities = await getBridgeCapabilityProbe();
    } catch {
      capabilities = null;
    }
  }
  const hasValidVersion = capabilities?.renderAbiVersion === REQUIRED_RENDER_ABI_VERSION;
  const hasScreenshotFlag = capabilities?.screenshotPng === true;
  const hasRenderAbi = hasValidVersion && hasScreenshotFlag;
  const hasScreenshotPng = hasRenderAbi && Boolean(api.screenshotPng);
  const hasValidResourceRequestVersion =
    capabilities?.renderResourceRequestAbiVersion === REQUIRED_RENDER_RESOURCE_REQUEST_ABI_VERSION;
  const hasResourceRequestFlag = capabilities?.renderResourceRequests === true;
  const hasRenderResourceAbi = hasValidVersion && hasValidResourceRequestVersion && hasResourceRequestFlag;
  const hasCompleteRenderResourceApi =
    hasRenderResourceAbi &&
    Boolean(api.seedRenderResource) &&
    Boolean(api.seedMissingRenderResource) &&
    Boolean(api.renderResourceRequests) &&
    Boolean(api.seedRenderImageResource) &&
    Boolean(api.seedMissingRenderImageResource);
  const hasCompleteRenderApi =
    hasRenderAbi &&
    Boolean(api.seedRenderResource) &&
    Boolean(api.seedMissingRenderResource) &&
    Boolean(api.screenshotPng);
  return {
    loaded: Boolean(bridgeCore),
    generation: bridgeGeneration,
    disposed: bridgeDisposed,
    realm: bridgeRealmKind,
    api: {
      querySnapshot: api.querySnapshot?.name ?? null,
      queryText: api.queryText?.name ?? null,
      queryHtml: api.queryHtml?.name ?? null,
      documentElementHtml: api.documentElementHtml?.name ?? null,
      domOp: api.domOp?.name ?? null,
      domBatch: api.domBatch?.name ?? null,
      pageRevision: api.pageRevision?.name ?? null,
      documentHandle: api.documentHandle?.name ?? null,
      setDocumentMetadata: api.setDocumentMetadata?.name ?? null,
      seedRenderResource: api.seedRenderResource?.name ?? null,
      seedMissingRenderResource: api.seedMissingRenderResource?.name ?? null,
      renderResourceRequests: api.renderResourceRequests?.name ?? null,
      seedRenderImageResource: api.seedRenderImageResource?.name ?? null,
      seedMissingRenderImageResource: api.seedMissingRenderImageResource?.name ?? null,
      screenshotPng: api.screenshotPng?.name ?? null,
      dispose: api.dispose?.name ?? null,
    },
    page: {
      revision: bridgePageRevision,
      documentHandle: bridgeDocumentHandle,
    },
    render: {
      available: hasCompleteRenderApi,
      renderAbiVersion: hasValidVersion ? REQUIRED_RENDER_ABI_VERSION : null,
      resourcesAvailable: hasCompleteRenderResourceApi,
      resourceRequestAbiVersion: hasValidResourceRequestVersion
        ? REQUIRED_RENDER_RESOURCE_REQUEST_ABI_VERSION
        : null,
      screenshotPng: Boolean(hasScreenshotPng),
    },
    bootstrap: {
      host: "node-vm",
      domTransport: "synchronous-op-dom",
      source: workerData.bootstrapPath ? "explicit-bootstrapPath" : "checkout-default",
      compiled: Boolean(compiledBootstrapRuntime),
      fullBrowser: false,
      denoIntegration: false,
      tasks: bridgeTaskHost?.status() ?? bridgeTaskLastStatus ?? {
        available: false,
        pending: 0,
        scheduled: 0,
        delivered: 0,
        running: 0,
        canceled: 0,
        dropped: 0,
        timeouts: 0,
        timeoutMs: vmTimeout(workerData.taskTimeoutMs, 1_000),
      },
    },
  };
}

async function bridgeEvaluate({ html, source, timeoutMs = 1_000, documentMetadata } = {}) {
  if (html !== undefined) await replaceBridgeCore(html, documentMetadata);
  else if (documentMetadata !== undefined) await applyDocumentMetadata(requireBridgeCore(), documentMetadata);
  requireBridgeCore();
  installDocumentFacade();
  return hostEvaluate(source, timeoutMs);
}

async function bridgeDomBatch({ html, operations, documentMetadata, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  if (html !== undefined) await replaceBridgeCore(html, documentMetadata);
  else if (documentMetadata !== undefined) await applyDocumentMetadata(requireBridgeCore(), documentMetadata);
  requireBridgeCore();
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  const result = domBatch(operations, generation, documentHandle);
  return { generation, documentHandle: result.documentHandle, revision: result.revision, results: result.results };
}

async function bridgeDomOperation({ html, command, arg1, arg2, documentMetadata, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  if (html !== undefined) await replaceBridgeCore(html, documentMetadata);
  else if (documentMetadata !== undefined) await applyDocumentMetadata(requireBridgeCore(), documentMetadata);
  requireBridgeCore();
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  const result = domOperation(command, arg1, arg2, generation, documentHandle);
  return {
    generation,
    documentHandle: bridgeDocumentHandle,
    revision: bridgePageRevision,
    result,
  };
}

async function seedRenderResource({ url, bytes, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  requireBridgeCore();
  const api = bridgeApi();
  await requireRenderCompatibility(api, "seedRenderResource");
  requireBoundedString(url, MAX_RENDER_URL_BYTES, "render resource URL");
  bytes = requireBoundedBytes(bytes, MAX_RENDER_RESOURCE_BYTES, "render resource bytes");
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  synchronizeBridgeIdentity(generation, documentHandle);
  syncCall(api.seedRenderResource, url, bytes);
  const identity = synchronizeBridgeIdentity(generation, documentHandle);
  return {
    generation,
    documentHandle: identity.documentHandle,
    revision: identity.revision,
  };
}

async function seedMissingRenderResource({ url, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  requireBridgeCore();
  const api = bridgeApi();
  await requireRenderCompatibility(api, "seedMissingRenderResource");
  requireBoundedString(url, MAX_RENDER_URL_BYTES, "render resource URL");
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  synchronizeBridgeIdentity(generation, documentHandle);
  syncCall(api.seedMissingRenderResource, url);
  const identity = synchronizeBridgeIdentity(generation, documentHandle);
  return {
    generation,
    documentHandle: identity.documentHandle,
    revision: identity.revision,
  };
}

function validateRenderResourceRequestPage(raw, offset, limit) {
  requireBoundedString(raw, MAX_RETURNED_STRING_BYTES, "render resource request page");
  let page;
  try {
    page = JSON.parse(raw);
  } catch {
    throw new TypeError("renderResourceRequests must return a JSON object");
  }
  if (page === null || typeof page !== "object" || Array.isArray(page)) {
    throw new TypeError("renderResourceRequests must return an object");
  }
  if (!Array.isArray(page.requests)) {
    throw new TypeError("renderResourceRequests requests must be an array");
  }
  if (page.requests.length > limit) {
    throw new RangeError(`renderResourceRequests returned more than the requested ${limit} entries`);
  }
  const requests = page.requests.map((request, index) => {
    if (request === null || typeof request !== "object" || Array.isArray(request)) {
      throw new TypeError(`render resource request ${index} must be an object`);
    }
    const url = requireBoundedString(request.url, MAX_RENDER_URL_BYTES, `render resource request ${index} URL`);
    if (request.kind !== "image" && request.kind !== "font") {
      throw new TypeError(`render resource request ${index} kind must be image or font`);
    }
    let profile;
    if (request.profile !== undefined) {
      profile = requireRenderImageRequestProfile(request.profile);
      if (request.kind !== "image") {
        throw new TypeError(`render resource request ${index} profile is only valid for images`);
      }
    }
    return profile === undefined
      ? { url, kind: request.kind }
      : { url, kind: request.kind, profile };
  });
  if (!Number.isSafeInteger(page.nextOffset) || page.nextOffset < 0 || page.nextOffset > 0xffff_ffff) {
    throw new TypeError("renderResourceRequests nextOffset must be an unsigned 32-bit integer");
  }
  if (typeof page.done !== "boolean") {
    throw new TypeError("renderResourceRequests done must be a boolean");
  }
  if (page.nextOffset < offset || page.nextOffset > offset + limit) {
    throw new RangeError("renderResourceRequests nextOffset is outside the requested page");
  }
  const advanced = page.nextOffset - offset;
  if (requests.length > advanced) {
    throw new RangeError("renderResourceRequests returned more entries than its cursor consumed");
  }
  if (!page.done && advanced !== limit) {
    throw new RangeError("renderResourceRequests non-final pages must consume the requested limit");
  }
  return { requests, nextOffset: page.nextOffset, done: page.done };
}

async function renderResourceRequests({ width, height, offset, limit, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  requireBridgeCore();
  const api = bridgeApi();
  await requireRenderResourceCompatibility(api, "renderResourceRequests");
  width = width ?? 800;
  height = height ?? 600;
  offset = offset ?? 0;
  limit = limit ?? MAX_RENDER_RESOURCE_REQUESTS_PER_PAGE;
  if (!Number.isSafeInteger(width) || width < 1 || width > MAX_SCREENSHOT_DIMENSION) {
    throw new RangeError(`render width must be an integer between 1 and ${MAX_SCREENSHOT_DIMENSION}`);
  }
  if (!Number.isSafeInteger(height) || height < 1 || height > MAX_SCREENSHOT_DIMENSION) {
    throw new RangeError(`render height must be an integer between 1 and ${MAX_SCREENSHOT_DIMENSION}`);
  }
  if (width * height > MAX_SCREENSHOT_PIXELS) {
    throw new RangeError(`render pixel count (${width * height}) exceeds the ${MAX_SCREENSHOT_PIXELS} pixel limit`);
  }
  if (!Number.isSafeInteger(offset) || offset < 0 || offset > 0xffff_ffff) {
    throw new RangeError("render resource request offset must be an unsigned 32-bit integer");
  }
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > MAX_RENDER_RESOURCE_REQUESTS_PER_PAGE) {
    throw new RangeError(
      `render resource request limit must be an integer between 1 and ${MAX_RENDER_RESOURCE_REQUESTS_PER_PAGE}`,
    );
  }
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  synchronizeBridgeIdentity(generation, documentHandle);
  const page = validateRenderResourceRequestPage(
    syncCall(api.renderResourceRequests, width, height, offset, limit),
    offset,
    limit,
  );
  const identity = synchronizeBridgeIdentity(generation, documentHandle);
  return {
    generation,
    documentHandle: identity.documentHandle,
    revision: identity.revision,
    ...page,
  };
}

async function seedRenderImageResource({ url, profile, bytes, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  requireBridgeCore();
  const api = bridgeApi();
  await requireRenderResourceCompatibility(api, "seedRenderImageResource");
  requireBoundedString(url, MAX_RENDER_URL_BYTES, "render resource URL");
  profile = requireRenderImageRequestProfile(profile);
  bytes = requireBoundedBytes(bytes, MAX_RENDER_RESOURCE_BYTES, "render resource bytes");
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  synchronizeBridgeIdentity(generation, documentHandle);
  syncCall(api.seedRenderImageResource, url, profile, bytes);
  const identity = synchronizeBridgeIdentity(generation, documentHandle);
  return { generation, documentHandle: identity.documentHandle, revision: identity.revision };
}

async function seedMissingRenderImageResource({ url, profile, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  requireBridgeCore();
  const api = bridgeApi();
  await requireRenderResourceCompatibility(api, "seedMissingRenderImageResource");
  requireBoundedString(url, MAX_RENDER_URL_BYTES, "render resource URL");
  profile = requireRenderImageRequestProfile(profile);
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  synchronizeBridgeIdentity(generation, documentHandle);
  syncCall(api.seedMissingRenderImageResource, url, profile);
  const identity = synchronizeBridgeIdentity(generation, documentHandle);
  return { generation, documentHandle: identity.documentHandle, revision: identity.revision };
}

async function screenshotPng({ width, height, scrollX, scrollY, expectedPage } = {}) {
  requireExpectedPage(expectedPage);
  requireBridgeCore();
  const api = bridgeApi();
  await requireRenderCompatibility(api, "screenshotPng");
  width = width ?? 800;
  height = height ?? 600;
  scrollX = scrollX ?? 0.0;
  scrollY = scrollY ?? 0.0;
  if (!Number.isSafeInteger(width) || width < 1 || width > MAX_SCREENSHOT_DIMENSION) {
    throw new RangeError(`screenshot width must be an integer between 1 and ${MAX_SCREENSHOT_DIMENSION}`);
  }
  if (!Number.isSafeInteger(height) || height < 1 || height > MAX_SCREENSHOT_DIMENSION) {
    throw new RangeError(`screenshot height must be an integer between 1 and ${MAX_SCREENSHOT_DIMENSION}`);
  }
  if (width * height > MAX_SCREENSHOT_PIXELS) {
    throw new RangeError(`screenshot pixel count (${width * height}) exceeds the ${MAX_SCREENSHOT_PIXELS} pixel limit`);
  }
  if (typeof scrollX !== "number" || !Number.isFinite(scrollX) || !Number.isFinite(Math.fround(scrollX))) {
    throw new TypeError("screenshot scrollX must be a finite number");
  }
  if (typeof scrollY !== "number" || !Number.isFinite(scrollY) || !Number.isFinite(Math.fround(scrollY))) {
    throw new TypeError("screenshot scrollY must be a finite number");
  }
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  synchronizeBridgeIdentity(generation, documentHandle);
  const rawBytes = syncCall(api.screenshotPng, width, height, scrollX, scrollY);
  const data = requireValidPngBytes(rawBytes, "screenshot PNG");
  const identity = synchronizeBridgeIdentity(generation, documentHandle);
  return {
    generation,
    documentHandle: identity.documentHandle,
    revision: identity.revision,
    data,
  };
}

async function installBootstrapRealm(timeoutMs = 5_000) {
  requireBridgeCore();
  statefulBridgeApi();
  if (bridgeRealmKind === "bootstrap") return getHostContext();
  // Reject an absent or incompatible portable platform contract before
  // discarding an existing host/legacy realm or installing any page globals.
  requirePortablePlatformCompatibility();

  // Bootstrap defines the browser global surface and therefore owns a fresh
  // realm. Discard an uninitialized host/legacy context instead of merging
  // globals with a page whose native wrappers belong to another lifecycle.
  hostContext = null;
  bridgeRealmKind = null;
  const context = getHostContext();
  const generation = bridgeGeneration;
  const documentHandle = bridgeDocumentHandle;
  const taskHost = new PortableTaskHost({
    enqueue(work) {
      if (typeof enqueueSerializedWork !== "function") {
        throw new Error("Obscura Worker task queue is unavailable");
      }
      return enqueueSerializedWork(work);
    },
    timeoutMs: vmTimeout(workerData.taskTimeoutMs, 1_000),
  });
  try {
    const runtime = await getCompiledBootstrapRuntime();
    const taskDispatcher = runtime.install(context, {
      timeoutMs: vmTimeout(timeoutMs, 5_000),
      opDom: (command, arg1, arg2) =>
        domOperation(command, arg1, arg2, generation, documentHandle),
      opPlatform: platformOperation,
      opTask: (command, argument) => taskHost.operation(command, argument),
    });
    taskHost.attach(taskDispatcher);
    bridgeTaskHost = taskHost;
    bridgeTaskLastStatus = null;
    bridgeRealmKind = "bootstrap";
    return context;
  } catch (error) {
    taskHost.close();
    hostContext = null;
    bridgeRealmKind = null;
    throw error;
  }
}

async function bootstrapEvaluate({
  html,
  source,
  timeoutMs = 1_000,
  bootstrapTimeoutMs = 5_000,
  documentMetadata,
} = {}) {
  // Capability negotiation is independent of page state. Do it before a
  // requested replacement can dispose the current core or its host realm.
  requirePortablePlatformCompatibility();
  if (html !== undefined) await replaceBridgeCore(html, documentMetadata);
  else if (documentMetadata !== undefined) await applyDocumentMetadata(requireBridgeCore(), documentMetadata);
  await installBootstrapRealm(bootstrapTimeoutMs);
  return hostEvaluate(source, timeoutMs);
}

async function inspectRuntime() {
  const runtime = await createRuntime();
  if (!runtime) return null;
  try {
    return {
      members: propertyNames(runtime),
      evaluate: member(runtime, EVALUATE_NAMES)?.name ?? null,
      dispose: member(runtime, DISPOSE_NAMES)?.name ?? null,
    };
  } finally {
    await disposeRuntime(runtime);
  }
}

async function createDrop({ iterations = 1, source = "1 + 1", timeoutMs } = {}) {
  if (!Number.isSafeInteger(iterations) || iterations < 1) {
    throw new RangeError("iterations must be a positive safe integer");
  }

  source = sourceText(source);
  timeoutMs = optionalTimeout(timeoutMs);
  const api = describeApi();
  if (!api.capabilities.runtimeFactory) {
    return { skipped: true, reason: "module has no runtime factory", iterations: 0 };
  }

  const startedAt = performance.now();
  let evaluated = 0;
  let disposed = 0;
  let lastResult;
  for (let index = 0; index < iterations; index += 1) {
    const runtime = await createRuntime();
    if (!runtime) throw new Error("Runtime factory returned no value");
    try {
      const evaluate = member(runtime, EVALUATE_NAMES);
      if (!evaluate) throw new Error("Created runtime has no evaluate method");
      lastResult = moduleResult(await evaluate.fn(source, timeoutMs));
      evaluated += 1;
    } finally {
      if (await disposeRuntime(runtime)) disposed += 1;
    }
  }
  const elapsedMs = performance.now() - startedAt;

  return {
    skipped: false,
    iterations,
    evaluated,
    disposed,
    elapsedMs,
    operationsPerSecond: iterations / (elapsedMs / 1_000),
    lastResult,
  };
}

async function hostStress({ iterations = 10_000, source = "1 + 1", timeoutMs = 1_000 } = {}) {
  if (!Number.isSafeInteger(iterations) || iterations < 1) {
    throw new RangeError("iterations must be a positive safe integer");
  }
  const sandbox = Object.create(null);
  const context = vm.createContext(sandbox, {
    name: "obscura-page-stress",
    codeGeneration: { strings: true, wasm: false },
    microtaskMode: "afterEvaluate",
  });
  installBoundedEvaluateScript.runInContext(context, { timeout: 1_000 });
  const script = boundedEvaluateScript(source, "obscura-stress.js");
  timeoutMs = vmTimeout(timeoutMs);
  const startedAt = performance.now();
  let lastResult;
  for (let index = 0; index < iterations; index += 1) {
    lastResult = runBoundedEvaluate(script, context, timeoutMs, "hostStress evaluation");
  }
  const elapsedMs = performance.now() - startedAt;
  return {
    iterations,
    elapsedMs,
    operationsPerSecond: iterations / (elapsedMs / 1_000),
    lastResult,
  };
}

async function bridgeStress({
  iterations = 10_000,
  html,
  source = "document.querySelector('h1').textContent",
  timeoutMs = 1_000,
} = {}) {
  if (!Number.isSafeInteger(iterations) || iterations < 1) {
    throw new RangeError("iterations must be a positive safe integer");
  }
  if (html !== undefined) await replaceBridgeCore(html);
  requireBridgeCore();
  installDocumentFacade();

  const script = boundedEvaluateScript(source, "obscura-bridge-stress.js");
  const context = getHostContext();
  timeoutMs = vmTimeout(timeoutMs);
  const startedAt = performance.now();
  let lastResult;
  for (let index = 0; index < iterations; index += 1) {
    lastResult = runBoundedEvaluate(script, context, timeoutMs, "bridgeStress evaluation");
  }
  const elapsedMs = performance.now() - startedAt;
  return {
    iterations,
    elapsedMs,
    operationsPerSecond: iterations / (elapsedMs / 1_000),
    lastResult,
  };
}

async function shutdown() {
  shuttingDown = true;
  const errors = [];
  const runtime = persistentRuntime;
  persistentRuntime = null;
  try {
    await disposeRuntime(runtime);
  } catch (error) {
    errors.push(error);
  }
  try {
    await disposeBridgeCore();
  } catch (error) {
    errors.push(error);
  }
  hostContext = null;
  bridgeRealmKind = null;
  if (errors.length > 0) throw new AggregateError(errors, "Harness shutdown cleanup failed");
  return { closed: true };
}

async function dispatch(operation, payload) {
  switch (operation) {
    case "inspect":
      return { ...metadata, ...describeApi(), runtime: await inspectRuntime(), threadId };
    case "version": {
      const version = member(target, VERSION_NAMES);
      if (version) return await version.fn();
      return valueMember(target, VERSION_NAMES)?.value ?? null;
    }
    case "abiVersion": {
      const abiVersion = member(target, ABI_VERSION_NAMES);
      return abiVersion ? await abiVersion.fn() : valueMember(target, ABI_VERSION_NAMES)?.value ?? null;
    }
    case "probe": {
      const probe = member(target, PROBE_NAMES);
      return probe
        ? decodeJsonText(await probe.fn(payload?.source))
        : { available: false, reason: "module has no probe export" };
    }
    case "hostEvaluate":
      return hostEvaluate(payload?.source, payload?.timeoutMs);
    case "bridgeEvaluate":
      return await bridgeEvaluate(payload);
    case "bridgeDomBatch":
      return await bridgeDomBatch(payload);
    case "bridgeDomOperation":
      return await bridgeDomOperation(payload);
    case "seedRenderResource":
      return await seedRenderResource(payload);
    case "seedMissingRenderResource":
      return await seedMissingRenderResource(payload);
    case "renderResourceRequests":
      return await renderResourceRequests(payload);
    case "seedRenderImageResource":
      return await seedRenderImageResource(payload);
    case "seedMissingRenderImageResource":
      return await seedMissingRenderImageResource(payload);
    case "screenshotPng":
      return await screenshotPng(payload);
    case "bootstrapEvaluate":
      return await bootstrapEvaluate(payload);
    case "bridgeStatus":
      return await bridgeStatus();
    case "bridgeRelease":
      return { dispose: await disposeBridgeCore(), ...(await bridgeStatus()) };
    case "moduleEvaluate":
      return await evaluateWithModule(payload?.source, payload?.timeoutMs);
    case "createDrop":
      return await createDrop(payload);
    case "hostStress":
      return hostStress(payload);
    case "bridgeStress":
      return await bridgeStress(payload);
    case "shutdown":
      return await shutdown();
    default:
      throw new Error(`Unknown harness operation: ${operation}`);
  }
}

function serializeError(error) {
  let errorText = "Harness worker failed";
  try {
    if (error != null) errorText = String(error);
  } catch {
    // Keep the stable fallback for hostile thrown values.
  }
  const safeValue = (name, fallback) => {
    try {
      const value = error?.[name];
      return value == null ? fallback : String(value);
    } catch {
      return fallback;
    }
  };
  return {
    name: safeValue("name", "Error"),
    message: safeValue("message", errorText),
    stack: safeValue("stack", undefined),
    code: safeValue("code", undefined),
  };
}

function postError(id, error) {
  parentPort.postMessage({ type: "response", id, error: serializeError(error) });
}

async function handleMessage(message) {
  const { id, operation, payload } = message ?? {};
  if (!Number.isSafeInteger(id) || id < 1 || typeof operation !== "string") return;
  if (shuttingDown) {
    postError(id, new Error("Harness worker is shutting down"));
    return;
  }
  try {
    const result = await dispatch(operation, payload);
    try {
      parentPort.postMessage({ type: "response", id, result });
    } catch (error) {
      postError(id, error);
    }
  } catch (error) {
    postError(id, error);
  } finally {
    if (operation === "shutdown") setImmediate(() => parentPort.close());
  }
}

try {
  ({ namespace: target, metadata } = await loadModule(workerData.modulePath, workerData.cwd));
  await requireCompatibleCoreAbi();
  // Operations mutate persistent runtime and bridge ownership state. Preserve
  // message order instead of allowing async listener invocations to overlap.
  let operationQueue = Promise.resolve();
  const fatalQueueFailure = (error) => {
    // handleMessage and portable task delivery normally contain their own
    // failures. If the response channel or queue itself fails, close rather
    // than running later work against uncertain page state.
    shuttingDown = true;
    try {
      parentPort.postMessage({ type: "fatal", error: serializeError(error) });
    } catch {
      // The port is already unusable.
    }
    parentPort.close();
  };
  enqueueSerializedWork = (work) => {
    operationQueue = operationQueue.then(work).catch(fatalQueueFailure);
    return operationQueue;
  };
  parentPort.on("message", (message) => {
    void enqueueSerializedWork(() => handleMessage(message));
  });
  parentPort.postMessage({
    type: "ready",
    result: { ...metadata, ...describeApi(), threadId },
  });
} catch (error) {
  parentPort.postMessage({ type: "fatal", error: serializeError(error) });
  parentPort.close();
}
