# Rust Code Quality Guide

This document describes the code quality tools and practices for the rust-core package.

## Pre-commit Hooks

The project uses Husky and lint-staged to automatically run quality checks before commits:

1. **Automatic formatting**: `cargo fmt` runs on all staged Rust files
2. **Additional checks**: When Rust files are modified, the pre-commit hook runs:
   - Format checking
   - Clippy lints
   - Type checking with `cargo check`

## Available Make Commands

```bash
# Core commands
make format         # Format code with rustfmt
make format-check   # Check formatting without modifying files
make lint          # Run Clippy lints
make check         # Fast type checking
make test          # Run unit tests
make test-wasm     # Run WASM tests in Chrome
make check-all     # Run all quality checks

# Additional tools
make audit         # Check for security vulnerabilities
make outdated      # Check for outdated dependencies
make unused-deps   # Find unused dependencies
make install-dev-tools  # Install additional development tools
```

## Configuration Files

### rustfmt.toml
- Line width: 100 characters
- Import grouping: Standard library, external crates, then local
- Format code in doc comments
- Edition: 2021

### clippy.toml
- Cognitive complexity threshold: 30
- Max lines per function: 100
- Max function arguments: 7
- Warnings treated as errors in CI

## Development Tools

### Required Tools
- **cargo fmt**: Code formatting (built-in)
- **cargo clippy**: Linting (built-in)
- **cargo check**: Type checking (built-in)

### Optional Tools (install with `make install-dev-tools`)
- **cargo-audit**: Security vulnerability scanning
- **cargo-outdated**: Check for outdated dependencies
- **cargo-machete**: Find unused dependencies

## CI/CD Pipeline

GitHub Actions runs the following checks on every PR:
1. Format check
2. Clippy lints
3. Unit tests
4. WASM build verification
5. Security audit

## Best Practices

1. **Before committing**: Run `make check-all` to ensure all checks pass
2. **Keep dependencies updated**: Regularly run `make outdated`
3. **Security**: Run `make audit` periodically
4. **Clean dependencies**: Use `make unused-deps` to find and remove unused dependencies

## Troubleshooting

### Pre-commit hook fails
```bash
# Fix formatting issues
make format

# Fix clippy warnings
make lint
# Then review and fix the warnings

# If tests fail
make test
# Fix failing tests
```

### Manual quality check
```bash
# Run all checks manually
cd packages/rust-core
make check-all
```