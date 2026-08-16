import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { extname, resolve } from "node:path";
import { defaultWasmModulePath, ObscuraCdpServer } from "./cdp-server.mjs";

function resolveModulePath(value) {
  const candidate = value ?? process.env.OBSCURA_NODE_WASM ?? defaultWasmModulePath();
  if (typeof candidate !== "string" || candidate.length === 0) {
    throw new TypeError("A wasm-bindgen module path is required");
  }
  const path = resolve(candidate);
  if (extname(path).toLowerCase() === ".node") {
    const error = new Error("Native .node modules are not supported by @obscura/browser; provide a wasm-bindgen module");
    error.code = "ERR_OBSCURA_NATIVE_UNSUPPORTED";
    throw error;
  }
  if (!existsSync(path)) {
    const error = new Error(`Obscura WASM module was not found at ${path}; pass modulePath or set OBSCURA_NODE_WASM`);
    error.code = "ERR_OBSCURA_WASM_NOT_FOUND";
    throw error;
  }
  return path;
}

export function version() {
  return "0.1.0-portable";
}

export async function launch(options = {}) {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("launch options must be an object");
  }
  const modulePath = resolveModulePath(options.modulePath);
  const server = new ObscuraCdpServer({
    ...options,
    modulePath,
  });
  await server.start();
  return new ObscuraBrowser(server);
}

export async function connect(endpoint, options = {}) {
  const client = new CdpClient(endpoint, options);
  await client.ready();
  return client;
}

export class ObscuraBrowser {
  #server;

  constructor(server) {
    this.#server = server;
  }

  httpEndpoint() {
    return this.#server.httpEndpoint();
  }

  wsEndpoint() {
    return this.#server.wsEndpoint();
  }

  processInfo() {
    return {
      pid: process.pid,
      host: this.#server.host,
      port: this.#server.addressInfo?.port ?? null,
      native: false,
      wasm: true,
    };
  }

  async close() {
    await this.#server.close();
  }
}

export class CdpClient {
  #endpoint;
  #socket;
  #pending = new Map();
  #nextId = 1;
  #readyPromise;
  #readyResolve;
  #readyReject;
  #closed = false;
  #listeners = new Set();

  constructor(endpoint, { WebSocketClass = globalThis.WebSocket } = {}) {
    if (typeof endpoint !== "string" || endpoint.length === 0) throw new TypeError("CDP endpoint must be a non-empty string");
    if (typeof WebSocketClass !== "function") {
      throw new Error("This Node runtime has no WebSocket client; use Node 22+ or provide WebSocketClass");
    }
    this.#endpoint = endpoint;
    this.#readyPromise = new Promise((resolvePromise, reject) => {
      this.#readyResolve = resolvePromise;
      this.#readyReject = reject;
    });
    this.#socket = new WebSocketClass(endpoint);
    this.#socket.addEventListener?.("open", () => this.#readyResolve(this));
    this.#socket.addEventListener?.("message", (event) => this.#onMessage(event.data));
    this.#socket.addEventListener?.("error", (event) => this.#fail(new Error(event?.message || "CDP WebSocket error")));
    this.#socket.addEventListener?.("close", () => this.#fail(new Error("CDP WebSocket closed")));
    // A small compatibility path for EventEmitter-style WebSocket clients.
    this.#socket.on?.("open", () => this.#readyResolve(this));
    this.#socket.on?.("message", (data) => this.#onMessage(data));
    this.#socket.on?.("error", (error) => this.#fail(error));
    this.#socket.on?.("close", () => this.#fail(new Error("CDP WebSocket closed")));
  }

  async ready() {
    return this.#readyPromise;
  }

  onEvent(listener) {
    if (typeof listener !== "function") throw new TypeError("CDP event listener must be a function");
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  command(method, params = {}, sessionId = undefined) {
    if (this.#closed) return Promise.reject(new Error("CDP client is closed"));
    const id = this.#nextId++;
    const message = { id, method, params, ...(sessionId ? { sessionId } : {}) };
    return new Promise((resolvePromise, reject) => {
      this.#pending.set(id, { resolve: resolvePromise, reject });
      try {
        this.#socket.send(JSON.stringify(message));
      } catch (error) {
        this.#pending.delete(id);
        reject(error);
      }
    });
  }

  async close() {
    if (this.#closed) return;
    this.#closed = true;
    try { this.#socket.close(); } catch {}
    this.#fail(new Error("CDP client closed"));
  }

  #onMessage(value) {
    if (value instanceof ArrayBuffer) value = new TextDecoder().decode(new Uint8Array(value));
    if (ArrayBuffer.isView(value)) value = new TextDecoder().decode(value);
    if (Buffer.isBuffer(value)) value = value.toString("utf8");
    let message;
    try { message = JSON.parse(String(value)); } catch { return; }
    if (Number.isSafeInteger(message.id) && this.#pending.has(message.id)) {
      const pending = this.#pending.get(message.id);
      this.#pending.delete(message.id);
      if (message.error) {
        const error = new Error(message.error.message || "CDP command failed");
        error.code = message.error.code;
        error.data = message.error.data;
        pending.reject(error);
      } else pending.resolve(message.result);
      return;
    }
    for (const listener of this.#listeners) {
      try { listener(message); } catch {}
    }
  }

  #fail(error) {
    if (this.#closed && this.#pending.size === 0) return;
    this.#readyReject(error);
    for (const pending of this.#pending.values()) pending.reject(error);
    this.#pending.clear();
  }
}

export { ObscuraCdpServer, defaultWasmModulePath };
