//! 棋譜プレイヤー TUI が PSV / tournament JSONL を共通に扱うためのデータモデル。

use std::path::PathBuf;

use anyhow::Result;
use rshogi_core::types::{Color, Move};

/// 索引フェーズで構築する、対局1件分のメタ情報（局面・指し手は含まない）。
///
/// `build_index` はこれを対局数ぶんだけ保持する。手の内容は `load_game` を
/// 呼ぶまで読み込まないため、ピークメモリは総対局数に比例し総手数には依存しない。
#[derive(Debug, Clone)]
pub struct GameIndexEntry {
    pub source: GameSourceRef,
    /// 対局の最終結果から導出済みの値（生のスコア符号は保持しない）。
    pub outcome: Option<GameOutcomeView>,
    /// JSONL の `result.error` を伝播。PSV は常に false。
    pub error: bool,
    pub ply_count: u32,
    /// 検索 UI 用（JSONL のみ）。再現性ある対局指定に使う。
    pub pair_index: Option<u32>,
    pub pair_slot: Option<u32>,
    pub startpos_idx: Option<u32>,
}

/// 対局の出典と、その対局を再生するために必要な位置情報。
#[derive(Debug, Clone, Copy)]
pub enum GameSourceRef {
    /// PSV ストリーム中の `[start_record, end_record)` レコード範囲。
    /// `ordinal` は表示用の通し番号（0-indexed）。
    Psv {
        start_record: u64,
        end_record: u64,
        ordinal: u32,
    },
    /// out-dir 横断インデックスの中での位置。
    /// `start_offset`/`end_offset` はペアファイル内のバイト範囲 `[start, end)`
    /// （`meta` 行を含まず、対象対局の `move`/`result` 行のみ）。
    Jsonl {
        file_idx: usize,
        game_id: u32,
        start_offset: u64,
        end_offset: u64,
    },
}

/// 対局の勝者（手番に依存しない固定 POV で表現する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameOutcomeView {
    Win(Color),
    Draw,
}

/// JSONL ペアファイル単位のメタ情報。対局ごとに複製しない
/// （`GameIndexEntry::file_idx` からこちらを引く）。
#[derive(Debug, Clone)]
pub struct PairFileMeta {
    pub path: PathBuf,
    pub black_label: String,
    pub white_label: String,
}

/// 索引全体。`entries` は出典を問わず1つの横断リストとしてフラット化されている。
#[derive(Debug, Clone, Default)]
pub struct GameIndex {
    pub entries: Vec<GameIndexEntry>,
    /// JSONL のみ使用。PSV ソースの場合は空。
    pub pair_files: Vec<PairFileMeta>,
    /// 致命的ではないが利用者に伝えるべき事項（シャッフル済み PSV の疑い、
    /// 対局データを含まないため読み飛ばした JSONL ファイル等）。
    pub warnings: Vec<String>,
}

impl GameIndex {
    pub fn pair_file(&self, file_idx: usize) -> Option<&PairFileMeta> {
        self.pair_files.get(file_idx)
    }
}

impl GameIndexEntry {
    /// `GameSourceRef::Jsonl` のときのみ `Some`。`GameIndex::pair_file` を引くキー。
    pub fn file_idx(&self) -> Option<usize> {
        match self.source {
            GameSourceRef::Jsonl { file_idx, .. } => Some(file_idx),
            GameSourceRef::Psv { .. } => None,
        }
    }
}

/// 1局を開いたときに遅延構築する、再生用の完全な手順。
#[derive(Debug, Clone)]
pub struct GameRecord {
    pub moves: Vec<MoveView>,
}

#[derive(Debug, Clone)]
pub struct MoveView {
    /// 出典上の手数。PSV は `skip_initial_ply`/`skip_in_check` により欠番がありうる
    /// ため、連番である保証はない（欠番はそのまま表示する）。
    pub ply: u32,
    pub side: Color,
    pub sfen_before: String,
    pub mv: Move,
    /// `kif.rs::format_move_label` を再利用した、棋譜風の人間可読ラベル。
    pub kif_label: String,
    pub annotation: MoveAnnotation,
}

/// 手への注釈。全フィールド `Option`。PSV は `score_cp` のみ埋まり、
/// JSONL（tournament 出力）は埋まる分だけ多くなる。
#[derive(Debug, Clone, Default)]
pub struct MoveAnnotation {
    pub score_cp: Option<i32>,
    pub score_mate: Option<i32>,
    pub depth: Option<u32>,
    pub seldepth: Option<u32>,
    pub nodes: Option<u64>,
    pub nps: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub think_limit_ms: Option<u64>,
    pub timed_out: Option<bool>,
    pub engine_label: Option<String>,
}

/// PSV / JSONL の違いを吸収する共通インターフェース。
pub trait GameSource {
    /// 全件を1パスでストリーミングし、対局単位のメタ情報だけを集めた索引を返す。
    fn build_index(&self) -> Result<GameIndex>;

    /// 索引のオフセットへ seek し、その対局の範囲だけを読んで再生用の手順を返す。
    /// `index` は `file_idx` から `PairFileMeta`（出典パス）を引くために使う
    /// （`GameIndexEntry` 自体にはパスを複製しない）。PSV ソースでは未使用。
    fn load_game(&self, index: &GameIndex, entry: &GameIndexEntry) -> Result<GameRecord>;
}

/// 対局一覧に出す表示ラベルを、保持済みの文字列ではなくその場で組み立てる
/// （`display_label: String` を `GameIndexEntry` ごとに複製しない設計上の選択）。
pub fn display_label(index: &GameIndex, entry: &GameIndexEntry) -> String {
    match entry.source {
        GameSourceRef::Psv { ordinal, .. } => format!("psv #{:03}", ordinal + 1),
        GameSourceRef::Jsonl {
            file_idx, game_id, ..
        } => match index.pair_file(file_idx) {
            Some(meta) => format!("{}-vs-{} #{:03}", meta.black_label, meta.white_label, game_id),
            None => format!("?-vs-? #{:03}", game_id),
        },
    }
}
