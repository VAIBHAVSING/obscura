import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { BOOTSTRAP_DOM_OP_COMMANDS } from "../src/bootstrap-runtime.mjs";

const bootstrapUrl = new URL("../../../crates/obscura-js/js/bootstrap.js", import.meta.url);

test("portable DOM command manifest exactly covers the production bootstrap", async () => {
  const source = await readFile(bootstrapUrl, "utf8");
  const literalCommands = Array.from(
    source.matchAll(/\b_dom(?:Parse)?\(\s*"([^"]+)"/g),
    (match) => match[1],
  );
  const reachableCommands = Array.from(new Set([
    ...literalCommands,
    // TreeWalker and NodeIterator select these commands through a local
    // variable, so they cannot be recovered from literal calls alone.
    "next_after_subtree",
    "prev_in_subtree",
  ])).sort();

  assert.equal(BOOTSTRAP_DOM_OP_COMMANDS.length, 63);
  assert.deepEqual(BOOTSTRAP_DOM_OP_COMMANDS, reachableCommands);
  assert.equal(new Set(BOOTSTRAP_DOM_OP_COMMANDS).size, BOOTSTRAP_DOM_OP_COMMANDS.length);
});
