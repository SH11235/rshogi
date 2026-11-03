//! `tournament.rs --sprt` が jsonl の meta 行へ書き出し、
//! `analyze_selfplay --sprt` がラベル/パラメータ自動推定に利用する共有スキーマ。

use serde::{Deserialize, Serialize};

use super::decision::Decision;
use super::penta::Penta;

/// SPRT 実行時の meta 行に埋め込む追加情報。
///
/// `tournament.rs` が書き出し、`analyze_selfplay.rs` が読み取る。
/// 両側でスキーマが一致するように単一定義とする。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct SprtMetaLog {
    pub base_label: String,
    pub test_label: String,
    pub nelo0: f64,
    pub nelo1: f64,
    pub alpha: f64,
    pub beta: f64,
}

/// SPRT が最初に境界へ達した時点の集計。
///
/// `tournament.rs` が meta.json の `sprt_stopped_at` に書き出す。境界到達後も開始済みの
/// ペアを回収するので最終集計とは一致しないことがあり、採否はこちらで決まる。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct SprtStopLog {
    #[serde(flatten)]
    pub hypothesis: SprtMetaLog,
    pub decision: Decision,
    pub pairs: u64,
    pub llr: f64,
    pub penta: Penta,
}
