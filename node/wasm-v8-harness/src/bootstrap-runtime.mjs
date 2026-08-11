import vm from "node:vm";

const HOST_DOM_OP_BINDING = "__obscuraHostDomOpBridge__";
const DEFAULT_INSTALL_TIMEOUT_MS = 5_000;
const MAX_VM_TIMEOUT_MS = 4_294_967_295;

// bootstrap.js reaches the two traversal commands through a local `step`
// variable; keep them in this manifest even though a literal-call search does
// not find them. This is the portable DOM contract, not a feature claim for
// networking, rendering, timers, or crypto.
export const BOOTSTRAP_DOM_OP_COMMANDS = Object.freeze([
  "append_child",
  "attribute_names",
  "child_nodes",
  "clone_node",
  "compare_order",
  "contains",
  "create_comment_node",
  "create_document_fragment",
  "create_element",
  "create_element_ns",
  "create_text_node",
  "document_doctype",
  "document_element",
  "document_encoding",
  "document_node_id",
  "document_referrer",
  "document_title",
  "document_url",
  "element_children",
  "first_child",
  "get_attribute",
  "get_attribute_ns",
  "get_element_by_id",
  "has_child_nodes",
  "inner_html",
  "insert_before",
  "is_connected",
  "last_child",
  "local_name",
  "matches_selector",
  "namespace_uri",
  "next_after_subtree",
  "next_in_subtree",
  "next_sibling",
  "node_index",
  "node_name",
  "node_root",
  "node_type",
  "outer_html",
  "parent_node",
  "prev_in_subtree",
  "prev_sibling",
  "query_selector",
  "query_selector_all",
  "query_selector_all_scoped",
  "query_selector_scoped",
  "remove_attribute",
  "remove_attribute_ns",
  "remove_child",
  "set_attribute",
  "set_attribute_ns",
  "set_fragment_html_executable",
  "set_inner_html",
  "set_inner_html_context",
  "set_text_content",
  "tag_name",
  "template_contents",
  "text_content",
]);

const installDomBridgeScript = new vm.Script(
  `(() => {
    "use strict";
    const hostDomOp = globalThis.${HOST_DOM_OP_BINDING};
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
      const message = typeof record?.message === "string"
        ? record.message
        : "Obscura DOM bridge operation failed";
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

    const opDom = function op_dom(command, arg1 = "", arg2 = "") {
      // All page-owned coercion stays inside the context and therefore inside
      // the vm timeout. The hidden host callback receives only primitive
      // strings and returns an inert envelope.
      const response = safeApply(hostDomOp, undefined, [
        safeApply(ContextString, undefined, [command]),
        safeApply(ContextString, undefined, [arg1]),
        safeApply(ContextString, undefined, [arg2]),
      ]);
      if (response === null || typeof response !== "object") {
        throw new ContextTypeError("Obscura DOM bridge returned an invalid response");
      }
      if (response.ok === true) {
        if (typeof response.value !== "string") {
          throw new ContextTypeError("Obscura DOM bridge returned a non-string op_dom value");
        }
        return response.value;
      }
      if (response.ok === false) throw translatedError(response.error);
      throw new ContextTypeError("Obscura DOM bridge returned an invalid response");
    };
    Object.freeze(opDom);

    // The first portable bootstrap slice is deliberately synchronous. False
    // makes bootstrap.js leave timers and posted tasks pending instead of
    // manufacturing microtask semantics for browser tasks.
    const asyncRuntimeAvailable = function op_async_runtime_available() {
      return false;
    };
    Object.freeze(asyncRuntimeAvailable);

    const unavailableTimer = function unavailableTimer() {
      throw new ContextError("Obscura portable browser task scheduling is unavailable");
    };
    Object.freeze(unavailableTimer);

    const ops = Object.create(null);
    Object.defineProperties(ops, {
      op_dom: { value: opDom, enumerable: true },
      op_async_runtime_available: { value: asyncRuntimeAvailable, enumerable: true },
    });
    Object.freeze(ops);

    const core = Object.create(null);
    Object.defineProperties(core, {
      ops: { value: ops, enumerable: true },
      queueUserTimer: { value: unavailableTimer, enumerable: true },
      cancelTimer: { value: unavailableTimer, enumerable: true },
    });
    Object.freeze(core);

    const deno = Object.create(null);
    Object.defineProperty(deno, "core", { value: core, enumerable: true });
    Object.freeze(deno);
    Object.defineProperty(globalThis, "Deno", {
      value: deno,
      writable: false,
      enumerable: false,
      configurable: false,
    });
  })()`,
  { filename: "obscura-portable-dom-bridge.js" },
);

const initializePageScript = new vm.Script(
  `(() => {
    "use strict";
    if (typeof globalThis.__obscura_init !== "function") {
      throw new TypeError("Obscura bootstrap did not install __obscura_init()");
    }
    globalThis.__obscura_init();
  })()`,
  { filename: "obscura-portable-page-init.js" },
);

function vmTimeout(value) {
  const timeoutMs = value ?? DEFAULT_INSTALL_TIMEOUT_MS;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_VM_TIMEOUT_MS) {
    throw new RangeError(
      `bootstrap install timeout must be an integer between 1 and ${MAX_VM_TIMEOUT_MS} milliseconds`,
    );
  }
  return timeoutMs;
}

function safeErrorRecord(error) {
  let name = "Error";
  let message = "Obscura DOM bridge operation failed";
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

function hostResponse(opDom, command, arg1, arg2) {
  const response = Object.create(null);
  try {
    const value = opDom(command, arg1, arg2);
    if (typeof value !== "string") {
      throw new TypeError("Portable dom_op must return a string");
    }
    Object.defineProperties(response, {
      ok: { enumerable: true, value: true },
      value: { enumerable: true, value },
    });
  } catch (error) {
    Object.defineProperties(response, {
      ok: { enumerable: true, value: false },
      error: { enumerable: true, value: safeErrorRecord(error) },
    });
  }
  return Object.freeze(response);
}

export function compileBootstrapRuntime(source, { filename = "<obscura:bootstrap>" } = {}) {
  if (typeof source !== "string" || source.length === 0) {
    throw new TypeError("Obscura bootstrap source must be a non-empty string");
  }
  if (typeof filename !== "string" || filename.length === 0) {
    throw new TypeError("Obscura bootstrap filename must be a non-empty string");
  }

  const bootstrapScript = new vm.Script(source, { filename });
  return Object.freeze({
    install(context, { opDom, timeoutMs } = {}) {
      if (!vm.isContext(context)) {
        throw new TypeError("Obscura bootstrap requires a Node vm context");
      }
      if (typeof opDom !== "function") {
        throw new TypeError("Obscura bootstrap requires a synchronous dom_op callback");
      }
      timeoutMs = vmTimeout(timeoutMs);
      if (Object.hasOwn(context, "Deno") || Object.hasOwn(context, "document")) {
        throw new Error("Obscura bootstrap requires a fresh page realm");
      }

      Object.defineProperty(context, HOST_DOM_OP_BINDING, {
        value: (command, arg1, arg2) => hostResponse(opDom, command, arg1, arg2),
        configurable: true,
      });
      try {
        installDomBridgeScript.runInContext(context, { timeout: timeoutMs });
      } finally {
        Reflect.deleteProperty(context, HOST_DOM_OP_BINDING);
      }

      bootstrapScript.runInContext(context, { timeout: timeoutMs });
      initializePageScript.runInContext(context, { timeout: timeoutMs });
      return context;
    },
  });
}
