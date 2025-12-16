//! ベンチマーク設定

use std::path::PathBuf;

/// 制限タイプ
#[derive(Debug, Clone, Copy)]
pub enum LimitType {
    Depth,
    Nodes,
    Movetime,
}

impl LimitType {
    pub fn to_usi_cmd(self) -> &'static str {
        match self {
            LimitType::Depth => "depth",
            LimitType::Nodes => "nodes",
            LimitType::Movetime => "movetime",
        }
    }
}

/// ベンチマーク設定
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    pub threads: Vec<usize>,
    pub tt_mb: u32,
    pub limit_type: LimitType,
    pub limit: u64,
    pub sfens: Option<PathBuf>,
    pub iterations: u32,
    pub verbose: bool,
}
