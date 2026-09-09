# Multi-agent operating guide and prompt template

## Purpose

Use these packets with Codex, agy/Gemini or another coding LLM without creating
overlapping edits or accepting weak evidence.

## Coordinator responsibilities

The coordinating agent must:

1. Inspect live `git status`, running build processes and current owners.
2. Assign one bounded task with an explicit file allowlist.
3. Keep at most one writer per file; other agents are read-only reviewers.
4. Freeze an ABI before assigning its host adapter in parallel.
5. Ask the worker to report its contract before editing.
6. Independently review every external-agent diff and reject unrelated changes.
7. Run authoritative tests itself or verify the exact retained output.
8. Commit coherent milestones, not per-agent fragments.
9. Never let a worker touch root `index.js`, docs, README or migration memory.
10. Update migration memory only after the source commit and evidence stabilize.

## Recommended parallel lanes

Safe concurrent combinations:

- one Rust implementation owner;
- one Node implementation owner only after the Rust ABI is frozen;
- one read-only security/reliability reviewer.

Examples:

- R1/R2 Rust writer + Node resource tests reviewer.
- P1/P2 Rust writer + P3/P4 ABI design reviewer; Node P4 waits for ABI freeze.
- N1 state writer + N2 transport design reviewer; no simultaneous envelope
  edits.
- C1 portable state writer + C2 WebSocket writer after the command envelope is
  frozen.
- K1 package writer + V5 read-only tarball auditor.

Unsafe combinations:

- two agents editing `worker.mjs`;
- two agents editing `obscura-wasm/src/lib.rs`;
- CDP and navigation agents independently inventing page/loader IDs;
- package and CLI agents independently inventing launch/shutdown behavior;
- running multiple heavy Cargo release builds on the two-CPU workspace;
- committing while another writer is still active.

## Generic implementation prompt

```text
Repository: /workspaces/obscura
Branch: wasm-node-migration

Read /workspaces/obscura/AGENTS.md and the assigned packet completely before
acting. You own only these files:
<EXACT FILE ALLOWLIST>

Task:
<ONE NUMBERED TASK FROM A PACKET>

Architecture invariant:
The final npm runtime contains JS + WASM only. Node supplies its existing V8,
network, timers, filesystem and WebSocket transport. Browser/page/DOM/layout/
paint/PDF/CDP state belongs in target-neutral Rust/WASM. Never add a native
Obscura executable, .node addon, deno_core or rusty_v8 runtime dependency.

Before editing, return:
1. the exact contract you will implement;
2. files you will touch;
3. tests and exact acceptance criteria;
4. overlap or dependency risks.

Then implement only the assigned task. Do not edit README, docs, .agent, root
index.js, or files owned by another worker. Do not bulk fmt. Use apply_patch.
Use release nextest, never cargo test. Do not commit or push.

Final report:
- changed files and concise diff summary;
- exact commands/results with pass/fail/skip counts;
- current limitations and unverified claims;
- git diff --check result;
- whether another agent may safely take the dependent task.

The coordinating Codex agent will independently evaluate your output.
```

## Generic read-only review prompt

```text
Review the implementation of <PACKET/TASK> in /workspaces/obscura. Read
AGENTS.md and the packet first. Do not edit files or run heavy Cargo commands.
Inspect correctness, ABI negotiation, identity/lifecycle, limits, cancellation,
panic/trap behavior, Node realm safety, native/WASM parity and test coverage.
Report only concrete findings with severity, file/line, reproduction and a
specific repair. Distinguish blockers from optional improvements. Confirm what
is proved and what remains unverified.
```

## Generic low-cost test delegation prompt for agy/Gemini

```text
Run only the following bounded verification task in /workspaces/obscura:
<EXACT COMMANDS OR SINGLE TEST-FILE TASK>

Do not modify production source. If asked to add tests, you may edit only:
<EXACT TEST FILE>

Read AGENTS.md. Never use cargo test; use the configured cargo-nextest binary.
Do not edit README/docs/.agent/root index.js. Do not commit. Return exact command
lines, exit codes, executed/pass/fail/skip counts, and the smallest failure
excerpt. Do not claim skipped real-artifact coverage passed. Codex will review
the result and diff independently.
```

## Handoff record template

```markdown
### Task ID

- Owner:
- Status: TODO | ACTIVE | REVIEW | VERIFIED | SHIPPED
- Base commit:
- Owned files:
- Contract/version:
- Changes:
- Commands run:
- Results:
- Artifact paths/hashes:
- Known gaps:
- Safe next task:
- Do not overlap with:
```

## Review checklist before accepting delegated code

- Diff contains only allowed files.
- No user-owned/untracked file was altered.
- No native runtime dependency entered the npm/WASM path.
- ABI presence and exact version are both negotiated.
- Host checks inputs before crossing to WASM where possible; WASM rechecks.
- Page generation/handle/revision identity is checked before and after calls.
- Results are copied and bounded; Promise/host-object results are rejected.
- Reset/release/close cancels and disposes the new state.
- Exact boundary and one-over-limit tests exist.
- Real artifact test is not silently skipped.
- Focused tests and `git diff --check` pass.
