# Build from source

The repository contains the Rust engine crates and the `@obscura/browser`
Node.js package. The standalone Rust CLI has been removed.

## Rust workspace

Requirements: Rust 1.75+, a C compiler, and several GB of disk space for the
first V8 build.

Build the native workspace (the WASM package has a separate shared-library
linking setup):

```bash
cargo build --release --workspace --exclude obscura-wasm
```

Build the embeddable Rust API with rendering:

```bash
cargo build --release -p obscura --features render
```

Build the native browser crate with rendering and stealth support:

```bash
cargo build --release -p obscura-browser --features render,stealth
```

The first build compiles V8 from source. Subsequent builds are incremental.
Stealth builds additionally require CMake, Clang, and libclang/LLVM.

Run Rust tests with `cargo nextest`, which isolates V8-backed tests in separate
processes:

```bash
cargo nextest run --release --features render --no-fail-fast
```

## Node.js package

The package build produces the embedded WASM browser and its Node.js entry
points:

```bash
npm install
npm run build
```

The package README documents the direct API, Puppeteer and Playwright
adapters, persistence, and the `obscura-browser` utility.
