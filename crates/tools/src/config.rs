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

/// 評価関数設定
#[derive(Debug, Clone)]
pub struct EvalConfig {
    /// NNUEファイルのパス（`None` の場合は Material 評価を使用）
    pub nnue_file: Option<PathBuf>,
    /// Material評価レベル（1, 2, 3, 4, 7, 8, 9）
    pub material_level: u8,
}

impl Default for EvalConfig {
    fn default() -> Self {
        EvalConfig {
            nnue_file: None,
            material_level: 9, // デフォルトはLv9
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
    /// 評価関数設定
    pub eval_config: EvalConfig,
    /// Searchインスタンスを再利用するか（履歴統計の蓄積効果を測定）
    pub reuse_search: bool,
    /// ウォームアップ実行回数（結果に含めないが履歴を蓄積）
    pub warmup: u32,
    /// EvalHashサイズ（メガバイト単位）
    pub eval_hash_mb: u32,
    /// EvalHashを使用するか
    pub use_eval_hash: bool,
}
