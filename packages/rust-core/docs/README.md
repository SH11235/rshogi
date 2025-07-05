# Opening Book Documentation

This directory contains documentation for the Shogi opening book conversion and integration system.

## Directory Structure

### 📁 tools/
**Tool Usage Guides** - How to use the command-line tools
- `opening-book-tools-guide.md` - Complete guide for convert_opening_book and verify_opening_book tools

### 📁 implementation/
**Implementation Guides** - Step-by-step implementation instructions
- `opening-book-implementation-guide.md` - Complete guide for implementing opening book feature in web app
- `rust-preprocessing-scripts-plan.md` - Architecture and planning for Rust preprocessing scripts

### 📁 reference/
**Technical References** - Format specifications and technical details
- `yaneuraou-sfen-format.md` - YaneuraOu SFEN file format specification

### 📁 development/
**Development Process** - TDD workflows and best practices
- `tdd-complete-guide.md` - Comprehensive TDD guide with workflows, implementation order, and test cases

### 📁 archive/
**Archived Documents** - Original documents that have been consolidated
- Contains original documents for reference before consolidation

## Quick Start

1. **Using the tools**: Start with [tools/opening-book-tools-guide.md](tools/opening-book-tools-guide.md)
2. **Implementing in web app**: Follow [implementation/opening-book-implementation-guide.md](implementation/opening-book-implementation-guide.md)
3. **Understanding the format**: See [reference/yaneuraou-sfen-format.md](reference/yaneuraou-sfen-format.md)
4. **Following TDD practices**: Read [development/tdd-complete-guide.md](development/tdd-complete-guide.md)

## Document Status

| Document | Status | Last Updated |
|----------|---------|--------------|
| opening-book-tools-guide.md | ✅ Active | 2025-01-06 |
| opening-book-implementation-guide.md | ✅ Active | 2025-01-06 |
| yaneuraou-sfen-format.md | ✅ Active | 2025-01-06 |
| tdd-complete-guide.md | ✅ Active | 2025-01-06 |
| rust-preprocessing-scripts-plan.md | 📝 Planning | 2025-01-06 |

## Consolidated Documents

The following documents have been consolidated to reduce duplication:

- **Implementation guides**: `opening-book-integration-plan.md` and `opening-book-web-integration.md` → `opening-book-implementation-guide.md`
- **TDD guides**: `tdd-workflow-implementation-order.md`, `step-by-step-implementation-with-tests.md`, `comprehensive-test-cases.md`, and `incremental-validation-workflow.md` → `tdd-complete-guide.md`
- **Master document**: `opening-book-conversion-master.md` → This README.md

Original documents are preserved in the `archive/` directory for reference.