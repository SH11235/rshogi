//! システム情報収集

use serde::{Deserialize, Serialize};
use sysinfo::System;

/// システム情報
///
/// ベンチマーク実行環境の情報を保持します。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    /// ベンチマーク実行日時（RFC 3339 形式）
    pub timestamp: String,
    /// CPU モデル名
    pub cpu_model: String,
    /// CPU コア数（論理コア）
    pub cpu_cores: usize,
    /// OS 名
    pub os: String,
    /// CPU アーキテクチャ（例: `x86_64`, `aarch64`）
    pub arch: String,
}

/// システム情報を収集
///
/// CPU とOS の情報のみを収集します。
/// `System::new_all()` は全プロセス・ネットワーク・ディスク情報を含み重いため、
/// 必要最小限の `System::new()` + `refresh_cpu_all()` を使用しています。
pub fn collect_system_info() -> SystemInfo {
    let mut sys = System::new();
    sys.refresh_cpu_all();

    let cpu_model = sys.cpus().first().map(|cpu| cpu.brand()).unwrap_or("Unknown").to_string();

    SystemInfo {
        timestamp: chrono::Utc::now().to_rfc3339(),
        cpu_model,
        cpu_cores: sys.cpus().len(),
        os: System::name().unwrap_or_else(|| "Unknown".to_string()),
        arch: std::env::consts::ARCH.to_string(),
    }
}
