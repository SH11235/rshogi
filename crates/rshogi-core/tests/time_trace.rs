use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use rshogi_core::search::{LimitsType, TimeManagement};
use rshogi_core::types::Color;

struct TraceLogger {
    enabled: AtomicBool,
    records: Mutex<Vec<String>>,
}

static LOGGER: TraceLogger = TraceLogger {
    enabled: AtomicBool::new(false),
    records: Mutex::new(Vec::new()),
};

impl log::Log for TraceLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.enabled.load(Ordering::Relaxed) && metadata.target() == "rshogi_core::search::time"
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.records.lock().unwrap().push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

fn manager(ponder: bool) -> TimeManagement {
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60_000;
    limits.inc[Color::Black.index()] = 2000;
    limits.ponder = ponder;
    let mut manager =
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    manager.init(&limits, Color::Black, 41, 100000);
    manager
}

fn timing_result() -> (i64, i64, i64) {
    let mut manager = manager(false);
    let target = manager.total_time_for_iteration(1.5, 0.9, 2.0, 4);
    manager.apply_iteration_timing(manager.maximum() + 1, target, 0.0, 12);
    (manager.optimum(), manager.maximum(), manager.search_end())
}

#[test]
fn trace_preserves_budgets_and_records_timing_decisions() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let without_trace = timing_result();
    assert!(LOGGER.records.lock().unwrap().is_empty());

    LOGGER.enabled.store(true, Ordering::Relaxed);
    assert_eq!(timing_result(), without_trace);
    let mut manager = manager(false);
    manager.apply_iteration_timing(
        manager.maximum() + 1,
        manager.maximum() as f64 * 2.0,
        100000.0,
        12,
    );
    manager.apply_iteration_timing(1000, 1000.0, 100000.0, 12);

    let mut ponder = self::manager(true);
    ponder.apply_iteration_timing(ponder.maximum() + 1, 1.0, 0.0, 12);
    assert!(ponder.stop_on_ponderhit());
    ponder.on_ponderhit();
    assert!(!ponder.is_pondering());

    let records = LOGGER.records.lock().unwrap();
    for expected in [
        "event=factors falling_eval=1.500000",
        "event=iteration_budget depth=12",
        "event=deadline_request reason=target",
        "event=deadline_request reason=maximum",
        "event=deadline_request reason=nodes_effort",
        "event=deadline reason=rounded",
        "event=ponderhit reason=notification",
        "event=deadline_request reason=ponderhit",
    ] {
        assert!(records.iter().any(|record| record.contains(expected)), "missing {expected}");
    }
}
