//! ONNX Runtime のプロセス全体の環境設定。
//!
//! Microsoft が配布する Windows 版 ONNX Runtime は、既定でテレメトリ (モデルのメタデータや
//! 推論回数など) を送る。ort の環境設定はプロセスで一度しか確定できず、最初の
//! `Session::builder()` が既定値 (テレメトリ有効) で確定させてしまう。そのため ONNX の
//! session はすべて [`session_builder`] から作る。

use std::sync::OnceLock;

use anyhow::Result;
use ort::session::Session;
use ort::session::builder::SessionBuilder;

/// テレメトリを無効にした環境を確定させてから session builder を返す。
///
/// 同じプロセスで何度呼んでもよい。他の経路が先に環境を確定させていた場合は、
/// テレメトリが有効なまま model を読まないようにエラーを返す。
pub fn session_builder() -> Result<SessionBuilder> {
    ensure_telemetry_disabled()?;
    Session::builder().map_err(|e| anyhow::anyhow!("ONNX Runtime error: {e}"))
}

fn ensure_telemetry_disabled() -> Result<()> {
    // `commit()` は 2 回目以降に必ず false を返し、自分の設定か既定値かを区別できないので、
    // 初回の結果を覚えて使い回す。
    static COMMITTED: OnceLock<bool> = OnceLock::new();
    let committed = *COMMITTED.get_or_init(|| ort::init().with_telemetry(false).commit());
    check_committed(committed)
}

fn check_committed(committed: bool) -> Result<()> {
    anyhow::ensure!(
        committed,
        "ONNX Runtime environment was configured before telemetry could be disabled; \
         refusing to load a model"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_setting_is_reused_across_calls() {
        ensure_telemetry_disabled().unwrap();
        ensure_telemetry_disabled().unwrap();
    }

    #[test]
    fn rejects_environment_configured_elsewhere() {
        assert!(check_committed(false).is_err());
    }
}
