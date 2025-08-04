# CLAUDE.md for Rust Core

This file provides guidance to Claude Code when working with Rust code in this directory.

## Core Development Principles

### Avoid Premature Optimization
- **"Premature optimization is the root of all evil" - Donald Knuth**
- Only implement features that are immediately needed
- Remove speculative code and "future-proofing" that isn't actively used
- Measure performance before optimizing
- Keep code simple and maintainable first
- Don't add fields or methods "for future use" - add them when actually needed

## Rust Code Quality Standards

### Format Strings - Use Modern Syntax

**IMPORTANT**: Always use inline variables in format strings instead of positional arguments.

```rust
// ❌ OLD STYLE - Don't use this
println!("Note: {} positions had errors and were skipped", final_errors);
format!("Move {} is invalid", move_str);
eprintln!("Error at position {}: {}", index, error);

// ✅ MODERN STYLE - Use this instead
println!("Note: {final_errors} positions had errors and were skipped");
format!("Move {move_str} is invalid");
eprintln!("Error at position {index}: {error}");
```

This modern syntax:
- Is more readable and maintainable
- Reduces potential errors from argument mismatches
- Satisfies clippy's `uninlined_format_args` lint
- Works with all format macros: `format!`, `println!`, `eprintln!`, `write!`, etc.

### Required Linting Checks

Before committing any Rust code changes, ALWAYS run:

1. `cargo fmt` - Format code according to Rust style guidelines
2. `cargo clippy -- -D warnings` - Run clippy with warnings as errors
3. `cargo test` - Ensure all tests pass

### Additional Best Practices

- Use `Result` types for error handling instead of panicking
- Document public APIs with doc comments (`///`)
- Keep functions focused and small
- Use descriptive variable names
- Prefer iterators over manual loops where appropriate