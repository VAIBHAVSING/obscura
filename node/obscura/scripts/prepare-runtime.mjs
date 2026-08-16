#!/usr/bin/env node
import { copyFile, mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const repositoryRoot = resolve(packageRoot, "../..");
const sourceRoot = resolve(repositoryRoot, "node/wasm-v8-harness/src");
const outputRoot = resolve(packageRoot, "src/runtime");
await mkdir(outputRoot, { recursive: true });
for (const name of [
  "bootstrap-runtime.mjs",
  "client.mjs",
  "limits.mjs",
  "module-loader.mjs",
  "task-runtime.mjs",
  "worker.mjs",
]) {
  await copyFile(resolve(sourceRoot, name), resolve(outputRoot, name));
}
await copyFile(resolve(repositoryRoot, "crates/obscura-js/js/bootstrap.js"), resolve(packageRoot, "bootstrap.js"));
process.stderr.write(`prepared runtime at ${outputRoot}\n`);
