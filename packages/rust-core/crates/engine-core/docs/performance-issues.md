# Performance Issues After Search Engine Unification

This document tracks performance issues introduced during the Search Engine Unification project.

## Issue 1: Depth-Only Search Performance Degradation

### Description
After the unification, depth-limited searches with `TimeControl::Infinite` have become significantly slower.

### Example
- `SearchLimitsBuilder::default().depth(5).build()`
- Before: Completed in < 5 seconds
- After: Takes ~25 seconds

### Root Cause
1. TimeManager is no longer created for `TimeControl::Infinite`
2. Event polling interval defaults to 1024 nodes without TimeManager
3. No time-based optimizations are applied

### Affected Code
- `src/search/unified/mod.rs:133` - TimeManager creation condition
- `src/search/unified/core/mod.rs:46` - Default polling interval

### Workaround
Temporarily reduced test depth from 5 to 4 in `tests/search_smoke_test.rs:108`

### Proposed Solutions
1. Create TimeManager even for Infinite time control to enable optimizations
2. Use smaller polling intervals for depth-only searches
3. Implement depth-specific optimizations independent of TimeManager

### Impact
- Integration tests require modification
- Depth-limited analysis mode may be too slow for practical use