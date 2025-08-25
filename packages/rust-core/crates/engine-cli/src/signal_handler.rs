//! Signal handler for debugging support

#[cfg(unix)]
pub mod unix {
    use backtrace::Backtrace;
    use log::{error, info};
    use signal_hook::consts::signal::*;
    use signal_hook::iterator::Signals;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static SIGNAL_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

    /// Install SIGUSR1 handler for stack dump
    pub fn install_signal_handler() {
        if SIGNAL_HANDLER_INSTALLED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            thread::spawn(|| {
                if let Err(e) = signal_handler_thread() {
                    error!("Signal handler thread error: {e}");
                }
            });
            info!("Signal handler installed for SIGUSR1 (stack dump)");
        }
    }

    fn signal_handler_thread() -> Result<(), Box<dyn std::error::Error>> {
        let mut signals = Signals::new([SIGUSR1])?;

        for sig in signals.forever() {
            match sig {
                SIGUSR1 => handle_usr1(),
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    fn handle_usr1() {
        let timestamp =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();

        // TSV format for structured logging
        eprintln!("timestamp={timestamp}\tkind=signal_received\tsignal=SIGUSR1");
        eprintln!("timestamp={timestamp}\tkind=thread_backtrace\tthread=main");

        // Capture and print backtrace
        let bt = Backtrace::new();
        let bt_str = format!("{:?}", bt);

        // Print each frame as separate TSV line for easier parsing
        for (i, frame) in bt_str.lines().enumerate() {
            eprintln!(
                "timestamp={timestamp}\tkind=backtrace_frame\tthread=main\tframe_id={i}\tframe_data={}",
                frame.trim()
            );
        }

        eprintln!("timestamp={timestamp}\tkind=backtrace_complete\tthread=main");

        // Also try to dump thread info
        dump_thread_info(timestamp);
    }

    fn dump_thread_info(timestamp: u128) {
        // Get current thread info
        let current_thread = thread::current();
        let thread_name = current_thread.name().unwrap_or("<unnamed>");
        let thread_id = format!("{:?}", current_thread.id());

        eprintln!(
            "timestamp={timestamp}\tkind=thread_info\tthread_id={thread_id}\tthread_name={thread_name}"
        );

        // Note: Full thread enumeration requires platform-specific code
        // For now, we just dump the current thread
    }
}

#[cfg(not(unix))]
pub mod unix {
    pub fn install_signal_handler() {
        // No-op on non-Unix platforms
        log::info!("Signal handler not available on this platform");
    }
}
