# Packet K2: npx command-line interface

## Goal

Provide a package-owned CLI that calls the same npm API and never invokes the
existing native Rust CLI.

## Dependencies

- K1 package API and tarball are stable.

## Command contract

Initial commands:

```text
obscura-browser serve [--host 127.0.0.1] [--port 0] [--json]
obscura-browser screenshot <url> <output.png> [viewport/options]
obscura-browser pdf <url> <output.pdf> [paper/options]
obscura-browser eval <url> <expression> [--json]
obscura-browser version [--json]
```

## Small tasks

### K2.1. Argument parser

- Use a small JS parser or a pure-JS dependency only if justified.
- Reject unknown/duplicate/missing options.
- Strict finite integer/float parsing and path handling.
- Stable exit codes: usage, launch, navigation, evaluation, output and signal.
- Secrets must not be printed in errors/logs.

### K2.2. Serve lifecycle

- Start through K1 `launch()`.
- Port 0 prints the actual endpoints only after ready.
- `--json` emits one machine-readable ready record.
- SIGINT/SIGTERM close once and set a conventional exit status.
- Parent stdin close behavior must be explicit, not accidental.

### K2.3. One-shot commands

- Launch, create page, navigate with deadline, perform operation, atomically
  write output, then close in `finally`.
- Never leave a partial output file after failure.
- Binary output can use stdout only with an explicit option that suppresses
  logs.
- Multi-statement eval behavior must be documented by tests, not inherited
  accidentally from the native CLI.

### K2.4. Local invocation tests

- `npx --yes <packed-tarball> ...` from a clean temporary project.
- Spawn from spaces/non-ASCII cwd.
- Port collision, invalid URL, timeout, unwritable output and signals.
- Screenshot/PDF signatures and eval JSON.
- Confirm no native child executable is spawned.

## Acceptance criteria

- Every command uses the same package server/runtime as K1.
- The CLI works from the packed artifact with the repo unavailable.
- Machine-readable mode has no incidental stdout noise.

## Copyable LLM prompt

```text
Implement one K2 task from .agent/files/remaining/12-npx-cli.md. Use only the
public K1 npm API; never spawn the Rust CLI or load a native addon. Treat stdout
as an API in --json/binary modes. Ensure cleanup and atomic output on every
failure. Test via npx against npm pack output from a clean temporary project.
Do not edit docs/README/.agent/index.js or commit unless told.
```
