# Rust Core for Shogi

This package contains the WebAssembly (WASM) implementation for P2P communication in the Shogi game.

## Prerequisites

- Rust toolchain (install from https://rustup.rs/)
- wasm-pack (`cargo install wasm-pack`)

## Building

### Development Build
```bash
npm run build:wasm:dev
```

### Production Build
```bash
npm run build:wasm
```

## Important Notes

⚠️ **WASM files must be built before running the web application!**

The build process:
1. Compiles Rust code to WebAssembly
2. Generates JavaScript bindings and TypeScript definitions
3. Copies the generated files to `packages/web/src/wasm/`

The generated files in `packages/web/src/wasm/` are:
- Excluded from git (in .gitignore)
- Required for the web application to run
- Must be regenerated when Rust code changes

## Development Workflow

1. Make changes to Rust code in `src/lib.rs`
2. Run `npm run build:wasm` 
3. Test changes in the web application

## Testing

```bash
# Run standard Rust tests
npm run test:rust

# Run WASM tests in browser (requires Chrome)
npm run test:wasm
```

## Code Quality

```bash
# Format code
npm run fmt

# Run linter
npm run lint
```