//! ベンチマーク設定

use std::path::PathBuf;

/// 探索制限のタイプ
#[derive(Debug, Clone, Copy)]
pub enum LimitType {
    /// 深さ制限（例: depth 20 で深さ20まで探索）
    Depth,
    /// ノード数制限（例: nodes 1000000 で100万ノードまで探索）
    Nodes,
    /// 時間制限（例: movetime 5000 で5秒間探索）
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
///
/// 将棋エンジンのベンチマーク実行に必要なパラメータを保持します。
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    /// 測定するスレッド数のリスト（例: `vec![1, 2, 4, 8]`）
    pub threads: Vec<usize>,
    /// 置換表サイズ（メガバイト単位）
    pub tt_mb: u32,
    /// 探索制限のタイプ（Depth/Nodes/Movetime）
    pub limit_type: LimitType,
    /// 制限値（`limit_type` に応じた単位）
    pub limit: u64,
    /// カスタム局面ファイルパス（`None` の場合はデフォルト局面を使用）
    pub sfens: Option<PathBuf>,
    /// 各局面セットの反復回数
    pub iterations: u32,
    /// 詳細な info 行を出力するか
    pub verbose: bool,
}
