use serde::{Deserialize, Serialize};

/// TypeScript 側で扱う駒のJSON表現
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PieceJson {
    /// "sente" | "gote"
    pub owner: String,
    /// "K" | "R" | "B" | "G" | "S" | "N" | "L" | "P"
    #[serde(rename = "type")]
    pub piece_type: String,
    /// 成駒かどうか
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted: Option<bool>,
}

/// 盤面の1マス
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CellJson {
    /// "9a" ~ "1i" 形式
    pub square: String,
    /// 駒（存在しない場合はnull）
    pub piece: Option<PieceJson>,
}

/// 持ち駒
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HandJson {
    #[serde(rename = "P", skip_serializing_if = "Option::is_none")]
    pub pawn: Option<u32>,
    #[serde(rename = "L", skip_serializing_if = "Option::is_none")]
    pub lance: Option<u32>,
    #[serde(rename = "N", skip_serializing_if = "Option::is_none")]
    pub knight: Option<u32>,
    #[serde(rename = "S", skip_serializing_if = "Option::is_none")]
    pub silver: Option<u32>,
    #[serde(rename = "G", skip_serializing_if = "Option::is_none")]
    pub gold: Option<u32>,
    #[serde(rename = "B", skip_serializing_if = "Option::is_none")]
    pub bishop: Option<u32>,
    #[serde(rename = "R", skip_serializing_if = "Option::is_none")]
    pub rook: Option<u32>,
}

/// 両者の持ち駒
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandsJson {
    pub sente: HandJson,
    pub gote: HandJson,
}

/// 盤面全体の状態
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardStateJson {
    /// 9x9のセル配列（0:1筋〜8:9筋、0:1段〜8:9段）
    pub cells: Vec<Vec<CellJson>>,
    /// 持ち駒
    pub hands: HandsJson,
    /// 手番: "sente" | "gote"
    pub turn: String,
    /// 手数（省略可）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ply: Option<i32>,
}

/// 棋譜リプレイ結果
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayResultJson {
    pub applied: Vec<String>,
    #[serde(rename = "last_ply")]
    pub last_ply: i32,
    pub board: BoardStateJson,
    pub error: Option<String>,
}
