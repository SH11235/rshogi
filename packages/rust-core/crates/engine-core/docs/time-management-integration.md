# Time Management Integration Guide

This guide explains how to properly integrate the time management system with your Shogi engine GUI or USI adapter.

## ⚠️ IMPORTANT: API Changes in v1.0.0

The `finish_move` method has been **removed** as of version 1.0.0. You must use the `update_after_move` API for all time management updates.

**Breaking change**: If you're upgrading from pre-1.0.0, you must migrate to the new API.

## Critical Concepts

### TimeControl vs. TimeState

- **TimeControl**: Immutable configuration containing initial time settings
- **TimeState**: Current runtime state, especially important for Byoyomi transitions

### Byoyomi Transitions

**CRITICAL**: The engine requires explicit notification of remaining main time to properly transition from main time to byoyomi periods.

## API Usage

### Recommended: Type-Safe API

Use the `update_after_move` method with `TimeState` enum for type safety:

```rust
use engine_core::time_management::{TimeManager, TimeState, SearchLimits, TimeControl};

// After each move, update time based on current state
match current_time_control {
    TimeControl::Byoyomi { .. } => {
        if main_time_remaining > 0 {
            time_manager.update_after_move(
                time_spent_ms,
                TimeState::Main { main_left_ms: main_time_remaining }
            );
        } else {
            time_manager.update_after_move(
                time_spent_ms,
                TimeState::Byoyomi { main_left_ms: 0 }
            );
        }
    }
    _ => {
        time_manager.update_after_move(time_spent_ms, TimeState::NonByoyomi);
    }
}
```

### Only One API

There is now only one API for time updates: `update_after_move`. This ensures type safety and prevents Byoyomi transition bugs.

## Integration Examples

### USI Protocol Integration

```rust
use engine_core::time_management::*;

fn handle_go_command(params: &UsiGoParams) -> SearchLimits {
    // Parse time control from USI parameters
    let time_control = if let Some(byoyomi_ms) = params.byoyomi {
        // Determine current main time based on side to move
        let main_time = if position.side_to_move() == Color::Black {
            params.btime.unwrap_or(0)
        } else {
            params.wtime.unwrap_or(0)
        };
        
        TimeControl::Byoyomi {
            main_time_ms: main_time,  // Current remaining main time
            byoyomi_ms,
            periods: 1,  // USI typically uses 1 period
        }
    } else if let (Some(wtime), Some(btime)) = (params.wtime, params.btime) {
        TimeControl::Fischer {
            white_ms: wtime,
            black_ms: btime,
            increment_ms: params.winc.unwrap_or(0),
        }
    } else {
        TimeControl::Infinite
    };
    
    SearchLimits {
        time_control,
        moves_to_go: params.movestogo,
        depth: params.depth.map(|d| d as u32),
        nodes: params.nodes,
        time_parameters: None,
    }
}

fn report_move_time(engine: &Engine, move_time_ms: u64, usi_params: &UsiGoParams) {
    let time_state = if let Some(byoyomi_ms) = usi_params.byoyomi {
        // Calculate remaining main time after this move
        let side = engine.position().side_to_move();
        let current_main = if side == Color::Black {
            usi_params.btime.unwrap_or(0)
        } else {
            usi_params.wtime.unwrap_or(0)
        };
        
        if current_main > 0 {
            TimeState::Main { main_left_ms: current_main.saturating_sub(move_time_ms) }
        } else {
            TimeState::Byoyomi { main_left_ms: 0 }
        }
    } else {
        TimeState::NonByoyomi
    };
    
    engine.time_manager().update_after_move(move_time_ms, time_state);
}
```

### Web/RPC Integration

```rust
use engine_core::time_management::*;

#[derive(Deserialize)]
struct MoveRequest {
    position: String,
    time_left: TimeInfo,
}

#[derive(Deserialize)]
struct TimeInfo {
    main_time_ms: Option<u64>,
    byoyomi_ms: Option<u64>,
    periods_left: Option<u32>,
    fischer_time_ms: Option<u64>,
    increment_ms: Option<u64>,
}

fn handle_move_request(req: MoveRequest) -> MoveResponse {
    let limits = create_search_limits(&req.time_left);
    let engine = create_engine();
    
    // Perform search
    let result = engine.search(limits);
    let time_spent = result.time_ms;
    
    // Update time state
    let time_state = match &req.time_left {
        TimeInfo { main_time_ms: Some(main), byoyomi_ms: Some(_), .. } if *main > 0 => {
            TimeState::Main { main_left_ms: main.saturating_sub(time_spent) }
        }
        TimeInfo { byoyomi_ms: Some(_), .. } => {
            TimeState::Byoyomi { main_left_ms: 0 }
        }
        _ => TimeState::NonByoyomi,
    };
    
    engine.time_manager().update_after_move(time_spent, time_state);
    
    MoveResponse {
        best_move: result.best_move,
        time_used_ms: time_spent,
    }
}
```

## Common Pitfalls

### ❌ Never Do This

```rust
// WRONG: Using NonByoyomi state with Byoyomi time control
let limits = SearchLimits {
    time_control: TimeControl::Byoyomi {
        main_time_ms: 300000,
        byoyomi_ms: 10000,
        periods: 3,
    },
    ..Default::default()
};

// Later...
time_manager.update_after_move(5000, TimeState::NonByoyomi);  // BUG: No transition will occur!
```

### ✅ Always Do This

```rust
// CORRECT: Provide current main time
let limits = SearchLimits {
    time_control: TimeControl::Byoyomi {
        main_time_ms: current_main_time,  // Current remaining time
        byoyomi_ms: 10000,
        periods: 3,
    },
    ..Default::default()
};

// Later...
time_manager.update_after_move(5000, TimeState::Main { 
    main_left_ms: current_main_time - 5000 
});
```

## WASM Integration

For WASM environments, time tracking works the same but uses `performance.now()`:

```rust
#[wasm_bindgen]
pub struct WasmEngine {
    engine: Engine,
}

#[wasm_bindgen]
impl WasmEngine {
    pub fn update_time_after_move(&mut self, time_spent_ms: u64, main_time_left: Option<u64>) {
        let time_state = if let Some(main) = main_time_left {
            if main > 0 {
                TimeState::Main { main_left_ms: main }
            } else {
                TimeState::Byoyomi { main_left_ms: 0 }
            }
        } else {
            TimeState::NonByoyomi
        };
        
        self.engine.time_manager().update_after_move(time_spent_ms, time_state);
    }
}
```

## Testing Your Integration

Always test these scenarios:

1. **Main Time to Byoyomi Transition**
   ```rust
   // Start with 5s main time
   let limits = create_byoyomi_limits(5000, 1000, 3);
   
   // Use 3s
   tm.update_after_move(3000, TimeState::Main { main_left_ms: 2000 });
   assert!(!tm.get_time_info().byoyomi_info.unwrap().in_byoyomi);
   
   // Use remaining 2s + 0.5s from byoyomi
   tm.update_after_move(2500, TimeState::Main { main_left_ms: 2000 });
   assert!(tm.get_time_info().byoyomi_info.unwrap().in_byoyomi);
   ```

2. **Multiple Period Consumption**
   ```rust
   // In byoyomi with 3 periods of 1s each
   tm.update_after_move(2500, TimeState::Byoyomi { main_left_ms: 0 });
   
   let info = tm.get_time_info().byoyomi_info.unwrap();
   assert_eq!(info.periods_left, 1);  // Consumed 2 periods
   ```

3. **Time Forfeit Detection**
   ```rust
   // Last period
   tm.update_after_move(1500, TimeState::Byoyomi { main_left_ms: 0 });
   assert!(tm.should_stop(0));  // Time forfeit
   ```

## Debug Mode

In debug builds, the engine will assert if you use Byoyomi without providing main time:

```
thread 'main' panicked at 'Byoyomi: main_time_left_ms required when not in byoyomi (initial main_time: 300000ms)'
```

This helps catch integration bugs during development.

## Migration from Pre-1.0.0

### Breaking Changes

The `finish_move` API has been completely removed in v1.0.0. This was necessary because:
- The old API allowed dangerous usage patterns that could cause time forfeits
- The optional parameter made it easy to forget critical information for Byoyomi
- Type safety was not enforced

### Migration Steps

1. **Update all time management calls**
   If you have any code using `finish_move`, it will no longer compile.

2. **Determine the time control type**
   - For Byoyomi: You MUST track whether in main time or byoyomi
   - For other modes: Use `TimeState::NonByoyomi`

3. **Replace with type-safe API**

```rust
// ❌ Old code (NO LONGER AVAILABLE)
// time_manager.finish_move(time_spent, None);  // This API is removed!

// ✅ New code (REQUIRED in v1.0.0)
// For Byoyomi:
let time_state = if main_time_remaining > 0 {
    TimeState::Main { main_left_ms: main_time_remaining }
} else {
    TimeState::Byoyomi { main_left_ms: 0 }
};
time_manager.update_after_move(time_spent, time_state);

// For Fischer/FixedTime/Infinite:
time_manager.update_after_move(time_spent, TimeState::NonByoyomi);
```

### Complete Migration Example

```rust
// Old implementation (pre-1.0.0) - NO LONGER COMPILES
fn handle_move_complete(tm: &TimeManager, result: &SearchResult) {
    let time_spent = result.elapsed_ms;
    // This won't compile in v1.0.0:
    // tm.finish_move(time_spent, Some(remaining_main_time));
}

// New implementation (v1.0.0+)
fn handle_move_complete(tm: &TimeManager, result: &SearchResult) {
    let time_spent = result.elapsed_ms;
    
    let time_state = match tm.time_control() {
        TimeControl::Byoyomi { .. } => {
            // Type system ensures you handle the state
            if self.main_time_remaining > 0 {
                TimeState::Main { main_left_ms: self.main_time_remaining }
            } else {
                TimeState::Byoyomi { main_left_ms: 0 }
            }
        }
        TimeControl::Fischer { .. } => TimeState::NonByoyomi,
        TimeControl::FixedTime { .. } => TimeState::NonByoyomi,
        TimeControl::FixedNodes { .. } => TimeState::NonByoyomi,
        TimeControl::Infinite => TimeState::NonByoyomi,
        TimeControl::Ponder => TimeState::NonByoyomi,
    };
    
    tm.update_after_move(time_spent, time_state);
}
```

### Testing Your Migration

After migration, test these scenarios:

1. **Byoyomi main time to period transition**
2. **Multiple byoyomi period consumption**
3. **Time forfeit detection**
4. **Non-byoyomi time controls still work**

See the test examples earlier in this guide.

## Summary

1. Always track current remaining main time for Byoyomi
2. Use `TimeState` enum for type safety
3. Test transition scenarios thoroughly
4. Watch for debug assertions during development
5. Migrate to new API for better maintainability
