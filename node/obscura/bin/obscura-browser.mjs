#!/usr/bin/env node
import { writeFile, rename, unlink } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { launch, connect, version } from "../src/index.mjs";

function usage() {
  process.stderr.write([
    "Usage:",
    "  obscura-browser serve [--module path] [--host 127.0.0.1] [--port 0] [--json]",
    "  obscura-browser screenshot <url> <output.png> [--module path]",
    "  obscura-browser pdf <url> <output.pdf> [--module path]",
    "  obscura-browser eval <url> <expression> [--module path]",
    "  obscura-browser version",
    "",
  ].join("\n"));
}

function parse(argv) {
  const [command, ...rest] = argv;
  const positional = [];
  const options = {};
  for (let index = 0; index < rest.length; index += 1) {
    const value = rest[index];
    if (!value.startsWith("--")) { positional.push(value); continue; }
    const name = value.slice(2);
    if (name === "json") { options.json = true; continue; }
    const next = rest[++index];
    if (next === undefined || next.startsWith("--")) throw new Error(`missing value for --${name}`);
    options[name] = next;
  }
  return { command, positional, options };
}

async function atomicWrite(path, bytes) {
  const destination = resolve(path);
  const temporary = `${destination}.obscura-${process.pid}-${Date.now().toString(36)}.tmp`;
  try {
    await writeFile(temporary, bytes, { flag: "wx" });
    await rename(temporary, destination);
  } catch (error) {
    try { await unlink(temporary); } catch {}
    throw error;
  }
}

async function withPage(options, callback) {
  const browser = await launch({
    modulePath: options.module,
    host: options.host,
    port: options.port === undefined ? 0 : Number(options.port),
  });
  const client = await connect(browser.wsEndpoint());
  try {
    const { targetId } = await client.command("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await client.command("Target.attachToTarget", { targetId, flatten: true });
    await client.command("Runtime.enable", {}, sessionId);
    await client.command("Page.enable", {}, sessionId);
    await callback(client, sessionId);
  } finally {
    await client.close();
    await browser.close();
  }
}

async function main(argv) {
  const { command, positional, options } = parse(argv);
  if (command === "version") { process.stdout.write(`${version()}\n`); return; }
  if (!command) { usage(); process.exitCode = 2; return; }
  if (command === "serve") {
    const browser = await launch({ modulePath: options.module, host: options.host, port: options.port === undefined ? 0 : Number(options.port) });
    const info = { httpEndpoint: browser.httpEndpoint(), wsEndpoint: browser.wsEndpoint(), ...browser.processInfo() };
    process.stdout.write(options.json ? `${JSON.stringify(info)}\n` : `${info.httpEndpoint}\n`);
    const close = async () => { await browser.close(); process.exit(0); };
    process.once("SIGINT", close);
    process.once("SIGTERM", close);
    return;
  }
  if (command === "screenshot" || command === "pdf") {
    if (positional.length !== 2) throw new Error(`${command} requires <url> <output>`);
    await withPage(options, async (client, sessionId) => {
      await client.command("Page.navigate", { url: positional[0] }, sessionId);
      const result = await client.command(command === "screenshot" ? "Page.captureScreenshot" : "Page.printToPDF", {}, sessionId);
      await atomicWrite(positional[1], Buffer.from(result.data, "base64"));
    });
    return;
  }
  if (command === "eval") {
    if (positional.length !== 2) throw new Error("eval requires <url> <expression>");
    await withPage(options, async (client, sessionId) => {
      await client.command("Page.navigate", { url: positional[0] }, sessionId);
      const result = await client.command("Runtime.evaluate", { expression: positional[1], returnByValue: true }, sessionId);
      process.stdout.write(`${JSON.stringify(result.result?.value ?? null)}\n`);
    });
    return;
  }
  usage();
  process.exitCode = 2;
}

main(process.argv.slice(2)).catch((error) => {
  process.stderr.write(`${error?.message ?? error}\n`);
  process.exitCode = error?.code === "ERR_OBSCURA_WASM_NOT_FOUND" ? 3 : 1;
});
