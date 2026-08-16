# Packet P: portable PDF generation

Status: SHIPPED in source commit `ab36f8e`; evidence is recorded in
`.agent/files/wasm-node-migration-memory.md`.

## Goal

Generate bounded PDF bytes inside `obscura-wasm`, using the same retained
print-media layout and resource cache as WASM screenshots. Node may persist or
return the bytes, but must not render the PDF or call a native Obscura binary.

## Dependencies

- Packet R must have a stable resource cache and fresh real-WASM artifact.
- The screenshot render ABI and capture limits are the starting point.

## Design source

Audit and extract target-neutral logic from
`crates/obscura-browser/src/pdf.rs`. Do not duplicate the native algorithm.
Separate page-independent pagination/encoding from the native `Page` wrapper,
then call the same shared implementation from native and WASM paths.

## Small tasks

### P1. Extract target-neutral PDF types and limits

- Move or share `RasterPdfOptions`, page ranges, validation errors, pagination
  planning and raster budgets under a target-neutral render/PDF module.
- Preserve existing defaults and error semantics.
- Keep native `Page::pdf` behavior unchanged through a compatibility wrapper.
- Verify dependencies compile for `wasm32-unknown-unknown` without filesystem,
  sockets or native font libraries.

### P2. Build PDF from retained WASM layout

- Select print media before layout.
- Reuse the current WASM DOM, seeded resources and retained styles.
- Rasterize bounded vertical slices from one immutable document layout.
- Encode selected pages into one PDF byte vector.
- Enforce page, per-page pixel, total pixel and output-byte budgets before
  large allocations.
- Handle backgrounds, landscape, scale, margins, page ranges and empty ranges.

### P3. Add PDF ABI v1

Proposed exports:

```text
pdfAbiVersion() -> 1
pdf(optionsJson, generation, documentHandle, revision) -> Uint8Array
```

- Options JSON must have a strict schema and byte cap.
- Reject unknown fields if doing so will not break the chosen CDP adapter;
  otherwise explicitly document ignored CDP compatibility fields.
- Return copied bytes beginning `%PDF-` and ending with a valid EOF marker.
- Fail closed if only part of the PDF method set exists.
- Dispose intermediate page rasters deterministically.

### P4. Add Node bridge

- Add `pdfAvailable` and `pdfAbiVersion` status.
- Add client/Worker `pdf(options, identity)` calls.
- Validate options on both sides, output length, PDF signature and identity
  before/after the synchronous WASM call.
- Never expose the WASM method directly into the page realm.

### P5. Unit and parity coverage

- Invalid paper sizes, margins and scales.
- Empty, overlapping and out-of-range page ranges.
- Page limit and raster/output budget boundaries.
- Portrait/landscape dimensions.
- `@media print` differs from screen output.
- Background flag behavior.
- Native shared implementation parity.
- Panic/error translation and reuse after failure.

### P6. Real artifact proof

- Generate a multi-page document with deterministic text, colors and an image.
- Seed resource bytes through Packet R.
- Produce PDF through the real Worker/WASM bridge.
- Parse its page count and page sizes using a disposable verifier outside the
  repository; do not make a native browser binary part of the runtime test.
- Assert deterministic bytes or stable structural hashes when metadata makes
  byte-for-byte identity inappropriate.

## Acceptance criteria

- WASM alone owns pagination, rasterization and PDF encoding.
- Node only transports options and bytes.
- Native `Page::pdf` regressions remain green.
- A real generated npm-style wrapper emits a valid multi-page PDF.
- No native-only crate appears in the wasm32 dependency tree.

## Copyable LLM prompt

```text
Implement one numbered task from Packet P in
.agent/files/remaining/02-portable-pdf.md. Read AGENTS.md first. Preserve the
native PDF behavior by extracting shared target-neutral code, not copying a
second algorithm. Node must only transport bytes; PDF creation belongs to
WASM. State the exact files you need before editing and do not overlap another
owner. Do not touch README/docs/.agent/index.js. Use focused release nextest
and a wasm32 release check. Do not commit unless told. Return evidence and any
semantic differences from native PDF.
```
