//! システム情報収集

use serde::{Deserialize, Serialize};
use sysinfo::System;

/// システム情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub timestamp: String,
    pub cpu_model: String,
    pub cpu_cores: usize,
    pub os: String,
    pub arch: String,
}

/// システム情報を収集
pub fn collect_system_info() -> SystemInfo {
    let mut sys = System::new_all();
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
