use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use tempfile::TempDir;

fn write_non_sfen_lines(path: &std::path::Path, n: usize) {
    let mut f = File::create(path).expect("create file");
    for i in 0..n {
        // intentionally no "sfen " token so generator would not extract positions
        // (this exercises pure streaming and line iteration)
        writeln!(f, "noop line {}\twith\tsome\ttabs", i).unwrap();
    }
}
#[cfg(target_os = "linux")]
fn read_linux_hwm_kb() -> Option<u64> {
    use std::io::BufRead as _;
    let f = File::open("/proc/self/status").ok()?;
    let r = BufReader::new(f);
    for line in r.lines().map_while(Result::ok) {
        if let Some(v) = line.strip_prefix("VmHWM:") {
            if let Some(kb) = v.split_whitespace().next().and_then(|s| s.parse().ok()) {
                return Some(kb);
            }
        }
    }
    None
}

#[test]
fn stream_100k_lines_vm_hwm_stable() {
    // Only meaningful on Linux where /proc/self/status exists
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Skipping VmHWM test on non-Linux platform");
        return;
    }

    #[cfg(target_os = "linux")]
    {
        let tmp = TempDir::new().unwrap();
        let input = tmp.path().join("stream_input.txt");
        let lines = 100_000usize;
        write_non_sfen_lines(&input, lines);

        // Baseline HWM before streaming
        let hwm_before = read_linux_hwm_kb().unwrap_or(0);

        // Stream through the file with a bounded buffer and line reuse
        let f = File::open(&input).unwrap();
        let mut r = BufReader::with_capacity(128 * 1024, f); // 128 KiB buffer
        let mut buf = String::new();
        let mut cnt: usize = 0;
        loop {
            buf.clear();
            let n = r.read_line(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            // simulate light processing
            if !buf.is_empty() {
                cnt += 1;
            }
        }
        assert_eq!(cnt, lines);

        // HWM after streaming
        let hwm_after = read_linux_hwm_kb().unwrap_or(hwm_before);
        // Acceptable delta threshold (in KiB). This should remain small and not scale with input size.
        // 16 MiB headroom to account for allocator/OS variance in CI.
        let delta_kb = hwm_after.saturating_sub(hwm_before);
        assert!(
            delta_kb <= 16 * 1024,
            "VmHWM grew too much: before={}kB after={}kB delta={}kB",
            hwm_before,
            hwm_after,
            delta_kb
        );
    }
}
