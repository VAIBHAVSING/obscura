import { createServer } from "node:http";
import { dirname, resolve } from "node:path";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { WasmV8Worker } from "./runtime/client.mjs";
import {
  MAX_WS_MESSAGE_BYTES,
  WebSocketPeer,
  websocketAccept,
} from "./websocket.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const DEFAULT_HOST = "127.0.0.1";
const MAX_COMMAND_BYTES = 1 * 1024 * 1024;
const MAX_EVENT_QUEUE = 512;
const MAX_REMOTE_OBJECTS = 2_048;

function cdpError(code, message, data = undefined) {
  const error = new Error(message);
  error.code = code;
  if (data !== undefined) error.data = data;
  return error;
}

function jsonBytes(value, label = "CDP message") {
  let text;
  try {
    text = JSON.stringify(value);
  } catch {
    throw cdpError(-32603, `${label} is not JSON serializable`);
  }
  if (Buffer.byteLength(text, "utf8") > MAX_COMMAND_BYTES) {
    throw cdpError(-32000, `${label} exceeds ${MAX_COMMAND_BYTES} bytes`);
  }
  return text;
}

function parseJsonMessage(text) {
  if (typeof text !== "string" || Buffer.byteLength(text, "utf8") > MAX_COMMAND_BYTES) {
    throw cdpError(-32000, `CDP command exceeds ${MAX_COMMAND_BYTES} bytes`);
  }
  let message;
  try {
    message = JSON.parse(text);
  } catch {
    throw cdpError(-32700, "Invalid CDP JSON");
  }
  if (!message || typeof message !== "object" || Array.isArray(message)) {
    throw cdpError(-32600, "CDP command must be an object");
  }
  if (!Number.isSafeInteger(message.id) || message.id < 0) {
    throw cdpError(-32600, "CDP command id must be a non-negative integer");
  }
  if (typeof message.method !== "string" || message.method.length === 0 || message.method.length > 256) {
    throw cdpError(-32600, "CDP command method must be a bounded string");
  }
  if (message.params !== undefined &&
      (message.params === null || typeof message.params !== "object" || Array.isArray(message.params))) {
    throw cdpError(-32602, "CDP params must be an object");
  }
  if (message.sessionId !== undefined &&
      (typeof message.sessionId !== "string" || message.sessionId.length === 0 || message.sessionId.length > 256)) {
    throw cdpError(-32600, "CDP sessionId must be a bounded string");
  }
  return message;
}

function safeUrl(value) {
  try {
    return new URL(value).href;
  } catch {
    return "about:blank";
  }
}

function targetInfo(target) {
  return {
    targetId: target.id,
    type: "page",
    title: target.title || "",
    url: target.url,
    attached: target.sessions.size > 0,
    openerId: undefined,
    canAccessOpener: false,
    browserContextId: target.contextId,
  };
}

function frameTree(target) {
  return {
    frameTree: {
      frame: {
        id: target.frameId,
        loaderId: target.loaderId,
        url: target.url,
        domainAndRegistry: "",
        securityOrigin: (() => {
          try { return new URL(target.url).origin; } catch { return ""; }
        })(),
        mimeType: "text/html",
        name: "",
        parentId: undefined,
        unresponsive: false,
      },
      childFrames: undefined,
    },
  };
}

function normalizeRemote(value, target, returnByValue = false) {
  if (value === undefined) return { type: "undefined" };
  if (value === null) return { type: "object", subtype: "null", value: null };
  if (typeof value === "boolean") return { type: "boolean", value, description: String(value) };
  if (typeof value === "string") return { type: "string", value, description: value };
  if (typeof value === "number") {
    if (Number.isNaN(value)) return { type: "number", unserializableValue: "NaN", description: "NaN" };
    if (value === Infinity) return { type: "number", unserializableValue: "Infinity", description: "Infinity" };
    if (value === -Infinity) return { type: "number", unserializableValue: "-Infinity", description: "-Infinity" };
    if (Object.is(value, -0)) return { type: "number", unserializableValue: "-0", description: "-0" };
    return { type: "number", value, description: String(value) };
  }
  if (typeof value === "bigint") {
    return { type: "bigint", unserializableValue: `${value}n`, description: `${value}n` };
  }
  if (returnByValue) {
    return { type: "object", value };
  }
  const objectId = target.storeRemote(value);
  return {
    type: "object",
    objectId,
    className: Array.isArray(value) ? "Array" : "Object",
    description: Array.isArray(value) ? `Array(${value.length})` : "Object",
    preview: {
      type: "object",
      overflow: Object.keys(value).length > 50,
      properties: Object.keys(value).slice(0, 50).map((name) => ({
        name,
        type: typeof value[name],
        value: typeof value[name] === "string" ? value[name].slice(0, 256) : undefined,
      })),
    },
  };
}

function exceptionDetails(error, expression = "") {
  return {
    exceptionId: 1,
    text: error?.message || "Uncaught",
    lineNumber: 0,
    columnNumber: 0,
    exception: {
      type: "object",
      subtype: "error",
      className: error?.name || "Error",
      description: `${error?.name || "Error"}: ${error?.message || "Evaluation failed"}`,
      value: undefined,
    },
    scriptId: "",
    url: "",
    expression,
  };
}

function findEncodedProperty(value, wanted, seen = new Set()) {
  if (!value || typeof value !== "object" || seen.has(value)) return undefined;
  seen.add(value);
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findEncodedProperty(item, wanted, seen);
      if (found !== undefined) return found;
    }
    return undefined;
  }
  if (Object.hasOwn(value, wanted)) return value[wanted];
  for (const item of Object.values(value)) {
    const found = findEncodedProperty(item, wanted, seen);
    if (found !== undefined) return found;
  }
  return undefined;
}

function decodePlaywrightValue(value, refs = new Map()) {
  if (value === null || typeof value !== "object") return value;
  if (Object.hasOwn(value, "h")) return { __remoteHandle: value.h };
  if (Object.hasOwn(value, "v") && Object.keys(value).length === 1) {
    if (value.v === "undefined") return undefined;
    if (value.v === "null") return null;
    if (value.v === "NaN") return Number.NaN;
    if (value.v === "Infinity") return Infinity;
    if (value.v === "-Infinity") return -Infinity;
    if (value.v === "-0") return -0;
    return value.v;
  }
  if (Object.hasOwn(value, "ref")) return refs.get(value.ref);
  if (Array.isArray(value)) return value.map((item) => decodePlaywrightValue(item, refs));
  if (Array.isArray(value.a)) {
    const result = [];
    if (Number.isSafeInteger(value.id)) refs.set(value.id, result);
    for (const item of value.a) result.push(decodePlaywrightValue(item, refs));
    return result;
  }
  if (Array.isArray(value.o)) {
    const result = Object.create(null);
    if (Number.isSafeInteger(value.id)) refs.set(value.id, result);
    for (const entry of value.o) {
      if (Array.isArray(entry) && entry.length === 2) result[entry[0]] = decodePlaywrightValue(entry[1], refs);
      else if (entry && typeof entry === "object" && typeof entry.k === "string") result[entry.k] = decodePlaywrightValue(entry.v, refs);
    }
    return result;
  }
  const result = Object.create(null);
  for (const [key, item] of Object.entries(value)) result[key] = decodePlaywrightValue(item, refs);
  return result;
}

class PageTarget {
  constructor(server, id, contextId = "default") {
    this.server = server;
    this.id = id;
    this.contextId = contextId;
    // Playwright treats the initial page target id as the main-frame id while
    // it is assembling a newly attached CRPage. Reuse the opaque target id so
    // frame/session lookup cannot observe a transient detached frame.
    this.frameId = id;
    this.loaderId = `${id}-loader-0`;
    this.url = "about:blank";
    this.title = "";
    this.worker = null;
    this.sessions = new Set();
    this.remoteObjects = new Map();
    this.nextRemoteId = 1;
    this.viewport = { width: 800, height: 600, deviceScaleFactor: 1 };
    this.closed = false;
  }

  async start() {
    if (this.worker) return;
    this.worker = await this.server.workerFactory(this.server.modulePath, {
      ...this.server.workerOptions,
    });
    await this.worker.ready(this.server.readyTimeoutMs);
    const status = await this.worker.bridgeStatus();
    if (!status.loaded) {
      await this.worker.bootstrapEvaluate("undefined", {
        html: "",
        documentMetadata: { url: this.url, referrer: "", encoding: "UTF-8" },
        timeoutMs: 1_000,
      });
    }
  }

  async close() {
    if (this.closed) return;
    this.closed = true;
    for (const session of this.sessions) session.target = null;
    this.sessions.clear();
    this.remoteObjects.clear();
    if (this.worker) await this.worker.close();
    this.worker = null;
  }

  storeRemote(value) {
    if (this.remoteObjects.size >= MAX_REMOTE_OBJECTS) {
      const oldest = this.remoteObjects.keys().next().value;
      this.remoteObjects.delete(oldest);
    }
    const id = `${this.id}:object-${this.nextRemoteId++}`;
    this.remoteObjects.set(id, value);
    return id;
  }

  getRemote(id) {
    if (typeof id !== "string" || !this.remoteObjects.has(id)) {
      throw cdpError(-32000, "Could not find object with given id");
    }
    return this.remoteObjects.get(id);
  }

  async evaluate(expression, options = {}) {
    await this.start();
    return this.worker.bootstrapEvaluate(expression, {
      timeoutMs: options.timeoutMs ?? this.server.evaluateTimeoutMs,
      requestTimeoutMs: options.requestTimeoutMs ?? this.server.requestTimeoutMs,
      documentMetadata: { url: this.url, referrer: "", encoding: "UTF-8" },
    });
  }

  async navigate(url, params = {}) {
    await this.start();
    const result = await this.worker.navigate(url, {
      executeScripts: params.executeScripts !== false,
      maxRedirects: params.maxRedirects,
      requestTimeoutMs: params.requestTimeoutMs ?? this.server.requestTimeoutMs,
      allowPrivateNetwork: params.allowPrivateNetwork === true,
    });
    this.url = result.url || safeUrl(url);
    this.loaderId = result.loaderId ? String(result.loaderId) : `${this.id}-loader-${Date.now()}`;
    // A navigation replaces the realm. Install the page facade after the
    // commit so Runtime.evaluate and DOM commands see the new document.
    try {
      await this.worker.bootstrapEvaluate("undefined", {
        documentMetadata: { url: this.url, referrer: "", encoding: "UTF-8" },
        timeoutMs: 500,
      });
    } catch {
      // A page with no bootstrap capability can still expose navigation state;
      // later Runtime/DOM commands return a protocol error.
    }
    this.title = "";
    try {
      this.title = await this.evaluate("typeof document.title === 'string' ? document.title : ''", { timeoutMs: 500 });
    } catch {
      this.title = "";
    }
    this.remoteObjects.clear();
    return result;
  }

  async screenshot(params = {}) {
    await this.start();
    const result = await this.worker.screenshotPng({
      width: params.width ?? this.viewport.width,
      height: params.height ?? this.viewport.height,
      scrollX: params.scrollX ?? 0,
      scrollY: params.scrollY ?? 0,
      requestTimeoutMs: this.server.requestTimeoutMs,
    });
    return result.data;
  }

  async pdf(params = {}) {
    await this.start();
    const result = await this.worker.pdf({
      ...params,
      viewportWidth: params.viewportWidth ?? this.viewport.width,
      viewportHeight: params.viewportHeight ?? this.viewport.height,
      requestTimeoutMs: this.server.requestTimeoutMs,
    });
    return result.data;
  }

  async dom(command, arg1 = "", arg2 = "") {
    await this.start();
    return this.worker.bridgeDomOp(command, arg1, arg2, { requestTimeoutMs: this.server.requestTimeoutMs });
  }

  async status() {
    await this.start();
    return this.worker.bridgeStatus();
  }
}

class CdpConnection {
  constructor(server, peer, initialTarget = null) {
    this.server = server;
    this.peer = peer;
    this.id = ++server.connectionCounter;
    this.sessions = new Map();
    this.browserSession = `${server.browserId}-connection-${this.id}`;
    this.closed = false;
    peer.on("message", (text) => void this.#message(text));
    peer.on("close", () => void this.close());
    peer.on("error", (error) => this.server.emit("error", error));
    if (initialTarget) {
      const sessionId = server.allocateSessionId(initialTarget.id);
      this.attach(initialTarget, sessionId);
    }
  }

  attach(target, sessionId = undefined) {
    sessionId ??= this.server.allocateSessionId(target.id);
    this.sessions.set(sessionId, target);
    target.sessions.add(this);
    return sessionId;
  }

  detach(sessionId) {
    const target = this.sessions.get(sessionId);
    if (target) target.sessions.delete(this);
    this.sessions.delete(sessionId);
  }

  send(message) {
    if (this.closed) return;
    const text = jsonBytes(message);
    try {
      this.peer.sendText(text);
    } catch (error) {
      this.server.emit("error", error);
      void this.close();
    }
  }

  event(method, params, sessionId = undefined) {
    this.send({ method, params, ...(sessionId ? { sessionId } : {}) });
  }

  async #message(text) {
    let command;
    try {
      command = parseJsonMessage(text);
    } catch (error) {
      this.send({ id: 0, error: { code: error.code ?? -32600, message: error.message } });
      return;
    }
    try {
      const result = await this.server.dispatch(this, command);
      if (result === undefined) return;
      this.send({ id: command.id, result, ...(command.sessionId ? { sessionId: command.sessionId } : {}) });
    } catch (error) {
      this.send({
        id: command.id,
        ...(command.sessionId ? { sessionId: command.sessionId } : {}),
        error: {
          code: Number.isInteger(error?.code) ? error.code : -32603,
          message: error?.message ?? "CDP command failed",
          ...(error?.data === undefined ? {} : { data: error.data }),
        },
      });
    }
  }

  async close() {
    if (this.closed) return;
    this.closed = true;
    for (const sessionId of this.sessions.keys()) this.detach(sessionId);
    this.server.connections.delete(this);
  }
}

export class ObscuraCdpServer {
  constructor({
    modulePath,
    bootstrapPath,
    host = DEFAULT_HOST,
    port = 0,
    workerOptions = {},
    workerFactory = (path, options) => WasmV8Worker.launch(path, options),
    readyTimeoutMs = 15_000,
    requestTimeoutMs = 30_000,
    evaluateTimeoutMs = 5_000,
    allowPrivateNetwork = false,
  } = {}) {
    this.modulePath = modulePath;
    const localBootstrap = resolve(HERE, "../bootstrap.js");
    this.bootstrapPath = bootstrapPath ?? (existsSync(localBootstrap) ? localBootstrap : undefined);
    this.host = host;
    this.port = port;
    this.workerOptions = { ...workerOptions, ...(this.bootstrapPath ? { bootstrapPath: this.bootstrapPath } : {}) };
    this.workerFactory = workerFactory;
    this.readyTimeoutMs = readyTimeoutMs;
    this.requestTimeoutMs = requestTimeoutMs;
    this.evaluateTimeoutMs = evaluateTimeoutMs;
    this.allowPrivateNetwork = allowPrivateNetwork;
    this.server = null;
    this.addressInfo = null;
    this.connections = new Set();
    this.targets = new Map();
    this.contexts = new Set(["default"]);
    this.connectionCounter = 0;
    this.targetCounter = 0;
    this.sessionCounter = 0;
    this.browserId = `obscura-browser-${process.pid}-${Date.now().toString(36)}`;
    this.listeners = new Map();
    this.streamCounter = 0;
    this.streams = new Map();
  }

  on(event, listener) {
    const listeners = this.listeners.get(event) ?? new Set();
    listeners.add(listener);
    this.listeners.set(event, listeners);
    return this;
  }

  emit(event, value) {
    for (const listener of this.listeners.get(event) ?? []) {
      try { listener(value); } catch {}
    }
  }

  allocateTargetId() {
    this.targetCounter += 1;
    return `page-${this.targetCounter}`;
  }

  allocateSessionId(targetId) {
    this.sessionCounter += 1;
    return `${targetId}-session-${this.sessionCounter}`;
  }

  async createTarget(url = "about:blank", contextId = "default") {
    if (!this.contexts.has(contextId)) throw cdpError(-32000, `Browser context ${contextId} was not found`);
    const target = new PageTarget(this, this.allocateTargetId(), contextId);
    this.targets.set(target.id, target);
    await target.start();
    if (url && url !== "about:blank") await target.navigate(url, { allowPrivateNetwork: this.allowPrivateNetwork });
    return target;
  }

  async start() {
    if (this.server) return this;
    if (typeof this.modulePath !== "string" || this.modulePath.length === 0) {
      throw new TypeError("ObscuraCdpServer requires a wasm-bindgen modulePath");
    }
    const target = new PageTarget(this, this.allocateTargetId());
    this.targets.set(target.id, target);
    await target.start();
    this.server = createServer((request, response) => this.#http(request, response));
    this.server.on("upgrade", (request, socket, head) => this.#upgrade(request, socket, head));
    await new Promise((resolvePromise, reject) => {
      const onError = (error) => { this.server?.off("error", onError); reject(error); };
      this.server.once("error", onError);
      this.server.listen(this.port, this.host, () => {
        this.server.off("error", onError);
        resolvePromise();
      });
    });
    this.addressInfo = this.server.address();
    return this;
  }

  get defaultTarget() {
    return this.targets.values().next().value ?? null;
  }

  httpEndpoint() {
    if (!this.addressInfo) throw new Error("CDP server is not started");
    return `http://${this.hostForUrl()}:${this.addressInfo.port}`;
  }

  wsEndpoint() {
    if (!this.addressInfo) throw new Error("CDP server is not started");
    return `ws://${this.hostForUrl()}:${this.addressInfo.port}/devtools/browser/${this.browserId}`;
  }

  hostForUrl() {
    return this.host.includes(":") && !this.host.startsWith("[") ? `[${this.host}]` : this.host;
  }

  async close() {
    if (!this.server && this.targets.size === 0) return;
    for (const connection of [...this.connections]) await connection.close();
    for (const target of [...this.targets.values()]) await target.close();
    this.targets.clear();
    this.streams.clear();
    if (this.server) {
      const server = this.server;
      this.server = null;
      await new Promise((resolvePromise) => server.close(() => resolvePromise()));
    }
    this.addressInfo = null;
  }

  #json(response, status, value) {
    const body = jsonBytes(value, "HTTP response");
    response.writeHead(status, {
      "content-type": "application/json; charset=utf-8",
      "content-length": Buffer.byteLength(body),
      "cache-control": "no-store",
      "connection": "close",
    });
    response.end(body);
  }

  #http(request, response) {
    if (request.method !== "GET") {
      response.writeHead(405, { allow: "GET", connection: "close" });
      response.end();
      return;
    }
    let url;
    try { url = new URL(request.url, this.httpEndpoint()); } catch {
      response.writeHead(400, { connection: "close" });
      response.end();
      return;
    }
    const pathname = url.pathname.length > 1 ? url.pathname.replace(/\/+$/u, "") : url.pathname;
    if (pathname === "/json/version") {
      this.#json(response, 200, {
        Browser: "Obscura/WASM",
        "Protocol-Version": "1.3",
        "User-Agent": `Obscura-WASM Node/${process.versions.node}`,
        webSocketDebuggerUrl: this.wsEndpoint(),
      });
      return;
    }
    if (pathname === "/json" || pathname === "/json/list") {
      this.#json(response, 200, [...this.targets.values()].map((target) => ({
        id: target.id,
        type: "page",
        title: target.title,
        url: target.url,
        webSocketDebuggerUrl: `ws://${this.hostForUrl()}:${this.addressInfo.port}/devtools/page/${target.id}`,
        devtoolsFrontendUrl: "",
        webSocketDebuggerUrl: `ws://${this.hostForUrl()}:${this.addressInfo.port}/devtools/page/${target.id}`,
        faviconUrl: "",
      })));
      return;
    }
    if (pathname === "/json/protocol") {
      this.#json(response, 200, { version: { major: "1", minor: "3" }, domains: [] });
      return;
    }
    response.writeHead(404, { connection: "close" });
    response.end();
  }

  #upgrade(request, socket, head) {
    const fail = (status, message) => {
      try {
        socket.write(`HTTP/1.1 ${status}\r\nConnection: close\r\nContent-Length: ${Buffer.byteLength(message)}\r\n\r\n${message}`);
      } finally {
        socket.destroy();
      }
    };
    if (request.method !== "GET" || String(request.headers.upgrade).toLowerCase() !== "websocket" ||
        !String(request.headers.connection).toLowerCase().split(",").map((value) => value.trim()).includes("upgrade")) {
      fail("400 Bad Request", "invalid websocket upgrade");
      return;
    }
    if (request.headers["sec-websocket-version"] !== "13") {
      fail("426 Upgrade Required", "websocket version 13 required");
      return;
    }
    let key;
    try { key = websocketAccept(request.headers["sec-websocket-key"]); } catch {
      fail("400 Bad Request", "invalid websocket key");
      return;
    }
    let pathname;
    try { pathname = new URL(request.url, this.httpEndpoint()).pathname; } catch {
      fail("400 Bad Request", "invalid websocket path");
      return;
    }
    let initialTarget = null;
    if (pathname.startsWith("/devtools/page/")) {
      const targetId = decodeURIComponent(pathname.slice("/devtools/page/".length));
      initialTarget = this.targets.get(targetId);
      if (!initialTarget) { fail("404 Not Found", "unknown target"); return; }
    } else if (!pathname.startsWith("/devtools/browser")) {
      fail("404 Not Found", "unknown websocket path");
      return;
    }
    socket.write([
      "HTTP/1.1 101 Switching Protocols",
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Accept: ${key}`,
      "\r\n",
    ].join("\r\n"));
    const peer = new WebSocketPeer(socket, head);
    const connection = new CdpConnection(this, peer, initialTarget);
    this.connections.add(connection);
  }

  async dispatch(connection, command) {
    const method = command.method;
    const params = command.params ?? {};
    const target = command.sessionId ? connection.sessions.get(command.sessionId) : null;
    if (command.sessionId === connection.browserSession) {
      return this.#dispatchBrowser(connection, command, method, params);
    }
    if (command.sessionId && !target) throw cdpError(-32000, `No target for session ${command.sessionId}`);
    if (target) return this.#dispatchPage(connection, command.sessionId, target, method, params);
    return this.#dispatchBrowser(connection, command, method, params);
  }

  async #dispatchBrowser(connection, command, method, params) {
    switch (method) {
      case "Browser.getVersion":
        return { protocolVersion: "1.3", product: "Obscura/WASM", revision: "portable", userAgent: `Obscura-WASM Node/${process.versions.node}`, jsVersion: process.versions.v8 };
      case "Browser.getBrowserCommandLine":
        return { arguments: [] };
      case "Browser.getWindowForTarget":
        return { windowId: 1, bounds: { left: 0, top: 0, width: 800, height: 600, windowState: "normal" } };
      case "Browser.setWindowBounds":
        return {};
      case "Target.getBrowserContexts":
        return { browserContextIds: [...this.contexts].filter((id) => id !== "default") };
      case "Target.createBrowserContext": {
        const id = `context-${this.contexts.size}`;
        this.contexts.add(id);
        return { browserContextId: id };
      }
      case "Target.disposeBrowserContext": {
        const id = params.browserContextId;
        if (id === "default" || !this.contexts.delete(id)) throw cdpError(-32000, "Browser context cannot be disposed");
        for (const target of [...this.targets.values()]) if (target.contextId === id) await this.#closeTarget(target);
        return {};
      }
      case "Target.getTargets":
        return { targetInfos: [...this.targets.values()].map(targetInfo) };
      case "Target.setDiscoverTargets":
        connection.discoverTargets = Boolean(params.discover);
        if (connection.discoverTargets) {
          for (const target of this.targets.values()) connection.event("Target.targetCreated", { targetInfo: targetInfo(target) });
        }
        return {};
      case "Target.setAutoAttach":
        connection.autoAttach = Boolean(params.autoAttach);
        connection.waitForDebuggerOnStart = Boolean(params.waitForDebuggerOnStart);
        if (connection.autoAttach) {
          for (const target of this.targets.values()) {
            if (![...connection.sessions.values()].includes(target)) {
              const sessionId = connection.attach(target);
              connection.event("Target.attachedToTarget", {
                sessionId,
                targetInfo: targetInfo(target),
                waitingForDebugger: connection.waitForDebuggerOnStart,
              });
            }
          }
        }
        return {};
      case "Target.attachToBrowserTarget":
        return { sessionId: connection.browserSession };
      case "Target.attachToTarget": {
        const target = this.targets.get(params.targetId);
        if (!target) throw cdpError(-32602, "targetId is not known");
        const sessionId = connection.attach(target);
        connection.event("Target.attachedToTarget", { sessionId, targetInfo: targetInfo(target), waitingForDebugger: false });
        return { sessionId };
      }
      case "Target.detachFromTarget":
        if (params.sessionId) connection.detach(params.sessionId);
        return {};
      case "Target.activateTarget":
        if (!this.targets.has(params.targetId)) throw cdpError(-32602, "targetId is not known");
        return {};
      case "Target.getTargetInfo": {
        const target = params.targetId ? this.targets.get(params.targetId) : this.defaultTarget;
        if (!target) throw cdpError(-32000, "target is not known");
        return { targetInfo: targetInfo(target) };
      }
      case "Target.createTarget": {
        const target = await this.createTarget(params.url || "about:blank", params.browserContextId || "default");
        if (connection.discoverTargets) connection.event("Target.targetCreated", { targetInfo: targetInfo(target) });
        if (connection.autoAttach) {
          const sessionId = connection.attach(target);
          connection.event("Target.attachedToTarget", {
            sessionId,
            targetInfo: targetInfo(target),
            waitingForDebugger: connection.waitForDebuggerOnStart,
          });
        }
        return { targetId: target.id };
      }
      case "Target.closeTarget": {
        const target = this.targets.get(params.targetId);
        if (!target) return { success: false };
        await this.#closeTarget(target);
        return { success: true };
      }
      case "Target.sendMessageToTarget": {
        const sessionId = params.sessionId;
        const nested = parseJsonMessage(params.message);
        nested.sessionId = sessionId;
        const result = await this.dispatch(connection, nested);
        if (nested.id !== undefined && result !== undefined) connection.send({ id: nested.id, result, sessionId });
        return {};
      }
      case "Schema.getDomains":
        return { domains: [] };
      case "Browser.setDownloadBehavior":
      case "Browser.grantPermissions":
      case "Browser.resetPermissions":
        return {};
      default:
        throw cdpError(-32601, `Method ${method} is not implemented`);
    }
  }

  async #closeTarget(target) {
    if (!this.targets.delete(target.id)) return;
    for (const connection of this.connections) {
      for (const [sessionId, sessionTarget] of connection.sessions) {
        if (sessionTarget === target) {
          connection.event("Target.detachedFromTarget", { sessionId, targetId: target.id });
          connection.detach(sessionId);
        }
      }
      if (connection.discoverTargets) connection.event("Target.targetDestroyed", { targetId: target.id });
    }
    await target.close();
  }

  async #dispatchPage(connection, sessionId, target, method, params) {
    switch (method) {
      case "Runtime.enable":
        connection.event("Runtime.executionContextCreated", {
          context: { id: 1, origin: target.url, name: "", uniqueId: `${target.id}:default`, auxData: { isDefault: true, type: "default", frameId: target.frameId } },
        }, sessionId);
        return {};
      case "Runtime.disable":
      case "Page.enable":
      case "Page.disable":
      case "Network.enable":
      case "Network.disable":
      case "DOM.enable":
      case "DOM.disable":
      case "Log.enable":
      case "Performance.enable":
      case "Page.setLifecycleEventsEnabled":
      case "Runtime.runIfWaitingForDebugger":
      case "Runtime.addBinding":
      case "Runtime.removeBinding":
      case "Target.setAutoAttach":
      case "Browser.getWindowForTarget":
      case "Browser.setWindowBounds":
      case "Page.bringToFront":
      case "Page.stopLoading":
      case "Page.setWebLifecycleState":
      case "Page.addScriptToEvaluateOnNewDocument":
      case "Page.removeScriptToEvaluateOnNewDocument":
      case "Page.setBypassCSP":
      case "Network.setCacheDisabled":
      case "Network.setUserAgentOverride":
      case "Network.setExtraHTTPHeaders":
      case "Emulation.setFocusEmulationEnabled":
      case "Emulation.setEmulatedMedia":
      case "Emulation.setDefaultBackgroundColorOverride":
      case "Security.setIgnoreCertificateErrors":
      case "Console.enable":
      case "Console.disable":
      case "Log.disable":
        return {};
      case "Page.setDocumentContent":
        if (typeof params.html !== "string") throw cdpError(-32602, "Page.setDocumentContent requires html");
        await target.worker.bootstrapEvaluate("undefined", { html: params.html, documentMetadata: { url: target.url, referrer: "", encoding: "UTF-8" } });
        target.remoteObjects.clear();
        return {};
      case "Page.getFrameTree":
        return frameTree(target);
      case "Page.getNavigationHistory":
        return { currentIndex: 0, entries: [{ id: 1, url: target.url, userTypedURL: target.url, title: target.title, transitionType: "typed" }] };
      case "Page.resetNavigationHistory":
        return {};
      case "Page.navigate": {
        const url = params.url;
        if (typeof url !== "string") throw cdpError(-32602, "Page.navigate requires url");
        connection.event("Page.frameStartedLoading", { frameId: target.frameId }, sessionId);
        try {
          const result = await target.navigate(url, { allowPrivateNetwork: this.allowPrivateNetwork, requestTimeoutMs: params.timeout });
          connection.event("Page.frameNavigated", { frame: frameTree(target).frameTree.frame }, sessionId);
          // A committed navigation replaces both the default and utility
          // execution worlds. Playwright waits for these notifications before
          // issuing page.evaluate()/locator calls in the new document.
          connection.event("Runtime.executionContextCreated", {
            context: { id: 1, origin: target.url, name: "", uniqueId: `${target.id}:default:${target.loaderId}`, auxData: { isDefault: true, type: "default", frameId: target.frameId } },
          }, sessionId);
          connection.event("Runtime.executionContextCreated", {
            context: { id: 2, origin: target.url, name: "", uniqueId: `${target.id}:utility:${target.loaderId}`, auxData: { isDefault: false, type: "isolated", frameId: target.frameId } },
          }, sessionId);
          connection.event("Page.lifecycleEvent", { frameId: target.frameId, loaderId: target.loaderId, name: "DOMContentLoaded", timestamp: Date.now() / 1000 }, sessionId);
          connection.event("Page.lifecycleEvent", { frameId: target.frameId, loaderId: target.loaderId, name: "load", timestamp: Date.now() / 1000 }, sessionId);
          connection.event("Page.frameStoppedLoading", { frameId: target.frameId }, sessionId);
          return { frameId: target.frameId, loaderId: target.loaderId, errorText: undefined, isDownload: false, result };
        } catch (error) {
          connection.event("Page.frameStoppedLoading", { frameId: target.frameId }, sessionId);
          return { frameId: target.frameId, errorText: error.message || "Navigation failed" };
        }
      }
      case "Page.reload":
        await target.navigate(target.url, { allowPrivateNetwork: this.allowPrivateNetwork });
        return {};
      case "Page.captureScreenshot": {
        const format = params.format ?? "png";
        if (format !== "png") throw cdpError(-32602, "Only PNG screenshots are supported");
        const data = await target.screenshot({
          width: target.viewport.width,
          height: target.viewport.height,
          clip: params.clip,
        });
        return { data: Buffer.from(data).toString("base64") };
      }
      case "Page.printToPDF": {
        const data = await target.pdf({
          landscape: Boolean(params.landscape),
          printBackground: Boolean(params.printBackground),
          scale: params.scale,
          paperWidth: params.paperWidth,
          paperHeight: params.paperHeight,
          marginTop: params.marginTop,
          marginBottom: params.marginBottom,
          marginLeft: params.marginLeft,
          marginRight: params.marginRight,
        });
        if (params.transferMode === "ReturnAsStream") {
          const handle = `obscura-stream-${++this.streamCounter}`;
          this.streams.set(handle, { data: Buffer.from(data), offset: 0 });
          return { stream: handle };
        }
        return { data: Buffer.from(data).toString("base64") };
      }
      case "IO.read": {
        const stream = this.streams.get(params.handle);
        if (!stream) throw cdpError(-32000, "Invalid stream handle");
        const chunkSize = Math.min(1 * 1024 * 1024, stream.data.length - stream.offset);
        const chunk = stream.data.subarray(stream.offset, stream.offset + Math.max(0, chunkSize));
        stream.offset += chunk.length;
        const eof = stream.offset >= stream.data.length;
        if (eof) this.streams.delete(params.handle);
        return { base64Encoded: true, data: chunk.toString("base64"), eof };
      }
      case "IO.close":
        this.streams.delete(params.handle);
        return {};
      case "Page.createIsolatedWorld": {
        const contextId = 2;
        connection.event("Runtime.executionContextCreated", {
          context: {
            id: contextId,
            origin: target.url,
            name: typeof params.worldName === "string" ? params.worldName : "",
            uniqueId: `${target.id}:utility`,
            auxData: { isDefault: false, type: "isolated", frameId: target.frameId },
          },
        }, sessionId);
        return { executionContextId: contextId };
      }
      case "Page.getLayoutMetrics":
        return {
          layoutViewport: { pageX: 0, pageY: 0, clientWidth: target.viewport.width, clientHeight: target.viewport.height },
          visualViewport: { offsetX: 0, offsetY: 0, pageX: 0, pageY: 0, scale: 1, zoom: 1, clientWidth: target.viewport.width, clientHeight: target.viewport.height },
          contentSize: { x: 0, y: 0, width: target.viewport.width, height: target.viewport.height },
        };
      case "Emulation.setDeviceMetricsOverride":
        if (Number.isSafeInteger(params.width) && params.width > 0) target.viewport.width = Math.min(params.width, 4096);
        if (Number.isSafeInteger(params.height) && params.height > 0) target.viewport.height = Math.min(params.height, 4096);
        target.viewport.deviceScaleFactor = typeof params.deviceScaleFactor === "number" ? params.deviceScaleFactor : 1;
        return {};
      case "Emulation.clearDeviceMetricsOverride":
        target.viewport = { width: 800, height: 600, deviceScaleFactor: 1 };
        return {};
      case "Runtime.evaluate":
        return this.#runtimeEvaluate(target, params);
      case "Runtime.callFunctionOn":
        return this.#runtimeCallFunction(target, params);
      case "Runtime.releaseObject":
        target.remoteObjects.delete(params.objectId);
        return {};
      case "Runtime.releaseObjectGroup":
        target.remoteObjects.clear();
        return {};
      case "Runtime.getIsolateId":
        return { id: `${target.id}-isolate` };
      case "Runtime.getProperties": {
        const value = target.getRemote(params.objectId);
        return { result: Object.keys(value).map((name) => ({ name, enumerable: true, configurable: true, writable: true, value: normalizeRemote(value[name], target, Boolean(params.generatePreview)) })) };
      }
      case "DOM.getDocument": {
        const status = await target.status();
        const documentNodeId = status.page?.documentHandle;
        if (!documentNodeId) throw cdpError(-32000, "WASM document identity is unavailable");
        let htmlNodeId = null;
        try { htmlNodeId = Number(await target.dom("query_selector", "html", "")); } catch {}
        const children = htmlNodeId ? [{ nodeId: htmlNodeId, backendNodeId: htmlNodeId, nodeType: 1, nodeName: "HTML", localName: "html", nodeValue: "", childNodeCount: 0 }] : [];
        return { root: { nodeId: documentNodeId, backendNodeId: documentNodeId, nodeType: 9, nodeName: "#document", localName: "", nodeValue: "", childNodeCount: children.length, children } };
      }
      case "DOM.querySelector": {
        const nodeId = await target.dom("query_selector", params.selector, "");
        return { nodeId: nodeId === "null" || nodeId === "-1" ? 0 : Number(nodeId) };
      }
      case "DOM.querySelectorAll": {
        const raw = await target.dom("query_selector_all", params.selector, "");
        const ids = JSON.parse(raw);
        return { nodeIds: Array.isArray(ids) ? ids.map(Number) : [] };
      }
      case "DOM.getOuterHTML": {
        const raw = await target.dom("outer_html", String(params.nodeId), "");
        return { outerHTML: raw === "null" ? "" : raw };
      }
      case "DOM.getAttributes": {
        const raw = await target.dom("attributes", String(params.nodeId), "");
        const attributes = JSON.parse(raw);
        return { attributes: Array.isArray(attributes) ? attributes.flatMap((entry) => Array.isArray(entry) ? entry : []) : [] };
      }
      case "DOM.describeNode":
        return { node: { nodeId: params.nodeId, backendNodeId: params.nodeId, nodeType: 1, nodeName: "ELEMENT", localName: "", nodeValue: "", childNodeCount: 0 } };
      case "DOM.requestChildNodes":
        return {};
      case "Network.getAllCookies":
        return { cookies: [] };
      case "Network.setCookies":
      case "Network.deleteCookies":
      case "Network.clearBrowserCookies":
      case "Network.clearBrowserCache":
        return {};
      default:
        throw cdpError(-32601, `Method ${method} is not implemented`);
    }
  }

  async #runtimeEvaluate(target, params) {
    const expression = params.expression;
    if (typeof expression !== "string") throw cdpError(-32602, "Runtime.evaluate requires expression");
    // Playwright installs one helper object in the page realm and invokes its
    // methods through Runtime.callFunctionOn. The bounded page bridge only
    // transports clone-safe values, so keep this helper as a protocol-local
    // remote object instead of attempting to serialize its class instance.
    if (!params.returnByValue && /module\.exports\.UtilityScript/.test(expression) && /new\s+\(/.test(expression)) {
      const objectId = target.storeRemote({ __obscuraUtilityScript: true });
      return {
        result: { type: "object", objectId, className: "UtilityScript", description: "UtilityScript" },
        executionContextId: Number.isSafeInteger(params.contextId) ? params.contextId : 1,
      };
    }
    if (!params.returnByValue && /module\.exports\.InjectedScript/.test(expression) && /new\s+\(/.test(expression)) {
      const objectId = target.storeRemote({ __obscuraInjectedScript: true });
      return {
        result: { type: "object", objectId, className: "InjectedScript", description: "InjectedScript" },
        executionContextId: Number.isSafeInteger(params.contextId) ? params.contextId : 1,
      };
    }
    try {
      const value = await target.evaluate(expression, { timeoutMs: params.timeout });
      return {
        result: normalizeRemote(value, target, Boolean(params.returnByValue)),
        executionContextId: Number.isSafeInteger(params.contextId) ? params.contextId : 1,
        exceptionDetails: undefined,
      };
    } catch (error) {
      return {
        result: { type: "undefined" },
        exceptionDetails: exceptionDetails(error, expression),
        executionContextId: Number.isSafeInteger(params.contextId) ? params.contextId : 1,
      };
    }
  }

  async #runtimeCallFunction(target, params) {
    if (typeof params.functionDeclaration !== "string") throw cdpError(-32602, "Runtime.callFunctionOn requires functionDeclaration");
    const args = Array.isArray(params.arguments) ? params.arguments : [];
    const receiver = params.objectId ? target.getRemote(params.objectId) : undefined;
    if (receiver?.__obscuraUtilityScript) {
      // The Playwright utility object is represented by a private marker. Its
      // methods are intentionally narrow: evaluate() executes the supplied
      // page expression, while jsonValue() returns a clone-safe value.
      if (params.functionDeclaration.includes(".evaluate(")) {
        const isFunction = args[1]?.value === true;
        const returnByValue = args[2]?.value === true;
        const functionSource = args[3]?.value;
        const argCount = Number.isSafeInteger(args[4]?.value) ? args[4].value : 0;
        if (typeof functionSource === "string" && functionSource.includes("injected.querySelector")) {
          const encoded = decodePlaywrightValue(args[6]?.value);
          const selector = findEncodedProperty(encoded, "source");
          const callbackText = findEncodedProperty(encoded, "callbackText");
          if (typeof selector === "string" && typeof callbackText === "string") {
            const expression = `(() => { const element = document.querySelector(${JSON.stringify(selector)}); if (!element) return { success: false }; return { success: true, value: (${callbackText})(null, element) }; })()`;
            return this.#runtimeEvaluate(target, {
              expression,
              returnByValue: true,
              timeout: params.timeout,
              contextId: params.executionContextId,
            });
          }
        }
        if (typeof functionSource === "string" && functionSource.includes("new Promise")) {
          // Playwright uses an injected polling promise to obtain viewport
          // metrics before a screenshot. The portable target already owns the
          // metrics, so resolve the protocol shape synchronously and let the
          // following Page.captureScreenshot command perform the real WASM
          // capture.
          return {
            result: {
              type: "object",
              value: {
                result: JSON.stringify({ width: target.viewport.width, height: target.viewport.height }),
                abort: null,
              },
            },
            executionContextId: params.executionContextId ?? 1,
          };
        }
        const callArgs = args.slice(5, 5 + argCount).map((arg) => {
          if (Object.hasOwn(arg ?? {}, "value")) return decodePlaywrightValue(arg.value);
          if (arg?.unserializableValue === "undefined") return undefined;
          if (arg?.unserializableValue === "NaN") return Number.NaN;
          if (arg?.unserializableValue === "Infinity") return Infinity;
          if (arg?.unserializableValue === "-Infinity") return -Infinity;
          return undefined;
        });
        if (typeof functionSource !== "string") throw cdpError(-32602, "UtilityScript.evaluate source is invalid");
        const source = isFunction
          ? `(${functionSource})(...${JSON.stringify(callArgs)})`
          : functionSource;
        return this.#runtimeEvaluate(target, {
          expression: source,
          returnByValue,
          timeout: params.timeout,
          contextId: params.executionContextId,
        });
      }
      if (params.functionDeclaration.includes("this.jsonValue") || params.functionDeclaration.includes("_promiseAwareJsonValueNoThrow")) {
        const value = args[1]?.value ?? args[0]?.value;
        return { result: normalizeRemote(value, target, Boolean(params.returnByValue)), executionContextId: params.executionContextId ?? 1 };
      }
    }
    if (receiver?.__obscuraInjectedScript) {
      // Keep the injected helper as a protocol-local remote object. The
      // concrete DOM operation is handled by the WASM-backed page facade in
      // later revisions; returning a stable remote marker keeps the CDP
      // session alive while callers probe helper capabilities.
      const nestedFunctionSource = args[3]?.value;
      if (typeof nestedFunctionSource === "string" && nestedFunctionSource.includes("injected.querySelector")) {
        const encoded = decodePlaywrightValue(args[6]?.value);
        const selector = findEncodedProperty(encoded, "source");
        const callbackText = findEncodedProperty(encoded, "callbackText");
        if (typeof selector === "string" && typeof callbackText === "string") {
          const expression = `(() => { const element = document.querySelector(${JSON.stringify(selector)}); if (!element) return { success: false }; return { success: true, value: (${callbackText})(null, element) }; })()`;
          return this.#runtimeEvaluate(target, {
            expression,
            returnByValue: true,
            timeout: params.timeout,
            contextId: params.executionContextId,
          });
        }
      }
      const objectId = target.storeRemote({ __obscuraInjectedResult: true });
      return { result: { type: "object", objectId, className: "Object", description: "Object" }, executionContextId: params.executionContextId ?? 1 };
    }
    const encoded = args.map((arg) => {
      if (!arg || Object.hasOwn(arg, "objectId")) {
        if (arg?.objectId) return target.getRemote(arg.objectId);
        return undefined;
      }
      return arg.value;
    });
    const source = `(${params.functionDeclaration}).apply(null, ${JSON.stringify(encoded)})`;
    // Host remote objects cannot cross the page boundary. Utility objects use
    // the private path above; ordinary callFunctionOn values are clone-safe.
    return this.#runtimeEvaluate(target, { ...params, expression: source });
  }
}

export function defaultWasmModulePath() {
  return resolve(HERE, "../wasm/obscura_wasm.cjs");
}

export { targetInfo };
