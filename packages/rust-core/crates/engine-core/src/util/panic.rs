use std::backtrace::Backtrace;
use std::panic::{self, PanicHookInfo};
use std::sync::OnceLock;

static HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

/// Install a panic hook that logs payloadとバックトレース先頭を出力する。
/// 既にセット済みの場合は何もしない。
pub fn install_panic_hook() {
    if HOOK_INSTALLED.get().is_some() {
        return;
    }

    HOOK_INSTALLED.set(()).expect("panic hook OnceLock should only be set once");

    panic::set_hook(Box::new(|info: &PanicHookInfo<'_>| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            format!("non-string panic payload (type_id={:?})", info.payload().type_id())
        };

        let location = info
            .location()
            .map(|loc| format!("{}:{}", loc.file(), loc.line()))
            .unwrap_or_else(|| "<unknown>".to_string());

        let backtrace_str = Backtrace::capture().to_string();
        let lines: Vec<String> = backtrace_str
            .lines()
            .filter(|l| !l.is_empty())
            .take(8)
            .map(|l| l.trim().to_string())
            .collect();
        let summary = if lines.is_empty() {
            "<no backtrace>".to_string()
        } else {
            lines.join(" | ")
        };

        log::error!(
            target: "panic",
            "panic detected: payload='{payload}' location={location} backtrace={summary}"
        );
    }));
}
