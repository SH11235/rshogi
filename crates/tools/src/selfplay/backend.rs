//! 探索バックエンドの抽象化
//!
//! USI 外部エンジン（`UsiBackend`）と rshogi-core 直接呼び出し（`NativeBackend`）を
//! 統一的に扱うための `SearchBackend` トレイトと `GameEngines` enum を提供する。

use anyhow::Result;
use std::time::Instant;

use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search, SearchInfo};
use rshogi_core::types::{Color, Move};

use super::engine::EngineProcess;
use super::types::{EvalLog, InfoSnapshot, SearchRequest, TimeArgs};

// =============================================================================
// 共通型
// =============================================================================

/// MultiPV 候補
#[derive(Debug, Clone)]
pub struct MultiPvCandidate {
    /// MultiPV 番号（1-indexed）
    pub multipv: u32,
    /// 評価値（centipawns）
    pub score_cp: i32,
    /// 詰みスコア（手数）
    pub score_mate: Option<i32>,
    /// PV の先頭手
    pub first_move: Move,
}

/// バックエンド統一の探索結果
pub struct BackendSearchResult {
    /// USI 形式の最善手文字列（"resign", "win", "none", or USI move）
    pub best_move_usi: Option<String>,
    /// パース済みの Move（合法手の場合のみ Some）
    pub best_move: Option<Move>,
    /// 評価情報（PV1）
    pub eval: Option<EvalLog>,
    /// 経過時間（ミリ秒）
    pub elapsed_ms: u64,
    /// タイムアウトしたか
    pub timed_out: bool,
    /// MultiPV 候補（multi_pv > 1 のときのみ有効）
    pub multipv_candidates: Vec<MultiPvCandidate>,
    /// info 行のバッファ（USI モードでログ出力に使用）
    pub info_lines: Vec<String>,
}

/// 探索パラメータ
pub struct SearchParams {
    /// SFEN 文字列（USI backend が `position sfen ...` で使用）
    pub sfen: String,
    /// 時間制御引数
    pub time_args: TimeArgs,
    /// 思考上限（ミリ秒）— タイムアウト検出用
    pub think_limit_ms: u64,
    /// タイムアウトマージン（ミリ秒）
    pub timeout_margin_ms: u64,
    /// 探索深さ制限
    pub go_depth: Option<u32>,
    /// ノード数制限
    pub go_nodes: Option<u64>,
    /// MultiPV 候補数（1 = 通常探索）
    pub multi_pv: u32,
    /// パス権利（先手, 後手）
    pub pass_rights: Option<(u8, u8)>,
    /// 手番
    pub side: Color,
    /// ゲーム ID（ログ用）
    pub game_id: u32,
    /// 手数（ログ用）
    pub ply: u32,
    /// info 行をバッファに収集するか（ログ出力用）
    pub collect_info_lines: bool,
}

// =============================================================================
// SearchBackend トレイト
// =============================================================================

/// 探索バックエンドの抽象化
pub trait SearchBackend {
    /// 新しい対局の準備
    ///
    /// `keep_tt` が true の場合は置換表を保持する。
    /// false の場合は置換表と履歴をクリアする。
    fn prepare_game(&mut self, keep_tt: bool) -> Result<()>;

    /// 探索を実行して結果を返す
    ///
    /// `pos` は NativeBackend が直接使用する。UsiBackend は `params.sfen` を参照する。
    fn search(&mut self, pos: &Position, params: &SearchParams) -> Result<BackendSearchResult>;
}

// =============================================================================
// NativeBackend — rshogi-core 直接呼び出し
// =============================================================================

/// rshogi-core の Search を直接呼び出すバックエンド（単一プロセス）
pub struct NativeBackend {
    engine: Search,
}

impl NativeBackend {
    /// 新しい NativeBackend を作成
    pub fn new(tt_size_mb: usize, eval_hash_size_mb: usize) -> Self {
        Self {
            engine: Search::new_with_eval_hash(tt_size_mb, eval_hash_size_mb),
        }
    }
}

impl SearchBackend for NativeBackend {
    fn prepare_game(&mut self, keep_tt: bool) -> Result<()> {
        if keep_tt {
            // TT・履歴ともに保持（USI の sync_ready() と同等）
        } else {
            // TT・履歴ともにクリア（USI の usinewgame と同等）
            self.engine.clear_tt();
            self.engine.clear_histories();
        }
        Ok(())
    }

    fn search(&mut self, pos: &Position, params: &SearchParams) -> Result<BackendSearchResult> {
        let mut limits = LimitsType::default();
        if let Some(depth) = params.go_depth {
            limits.depth = depth as i32;
        }
        if let Some(nodes) = params.go_nodes {
            limits.nodes = nodes;
        }
        limits.multi_pv = params.multi_pv.max(1) as usize;

        // 時間制御が設定されている場合
        let ta = &params.time_args;
        let has_time = ta.byoyomi > 0 || ta.btime > 0 || ta.wtime > 0 || ta.binc > 0 || ta.winc > 0;
        if has_time {
            limits.time[Color::Black.index()] = ta.btime as i64;
            limits.time[Color::White.index()] = ta.wtime as i64;
            limits.byoyomi[Color::Black.index()] = ta.byoyomi as i64;
            limits.byoyomi[Color::White.index()] = ta.byoyomi as i64;
            limits.inc[Color::Black.index()] = ta.binc as i64;
            limits.inc[Color::White.index()] = ta.winc as i64;
        }

        // MultiPV 候補の収集
        let mut multipv_candidates: Vec<MultiPvCandidate> = Vec::new();
        let collect_multipv = params.multi_pv > 1;

        let mut search_pos = pos.clone();
        let start = Instant::now();

        let result = self.engine.go(
            &mut search_pos,
            limits,
            Some(|info: &SearchInfo| {
                if collect_multipv && !info.pv.is_empty() {
                    let mpv = info.multi_pv as u32;
                    // 同一 multipv 番号は上書き（最終 depth の結果を保持）
                    if let Some(existing) = multipv_candidates.iter_mut().find(|c| c.multipv == mpv)
                    {
                        existing.score_cp = info.score.to_cp();
                        existing.score_mate = if info.score.is_mate_score() {
                            Some(info.score.mate_ply())
                        } else {
                            None
                        };
                        existing.first_move = info.pv[0];
                    } else {
                        multipv_candidates.push(MultiPvCandidate {
                            multipv: mpv,
                            score_cp: info.score.to_cp(),
                            score_mate: if info.score.is_mate_score() {
                                Some(info.score.mate_ply())
                            } else {
                                None
                            },
                            first_move: info.pv[0],
                        });
                    }
                }
            }),
        );

        let elapsed_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

        let best_move = if result.best_move != Move::NONE {
            Some(result.best_move)
        } else {
            None
        };

        let best_move_usi = best_move.map(|m| m.to_usi());

        let eval = Some(EvalLog {
            score_cp: if result.score.is_mate_score() {
                None
            } else {
                Some(result.score.to_cp())
            },
            score_mate: if result.score.is_mate_score() {
                let ply = result.score.mate_ply();
                Some(if result.score.is_loss() { -ply } else { ply })
            } else {
                None
            },
            depth: Some(result.depth as u32),
            seldepth: None,
            nodes: Some(result.nodes),
            time_ms: Some(elapsed_ms),
            nps: if elapsed_ms > 0 {
                Some(result.nodes * 1000 / elapsed_ms)
            } else {
                None
            },
            pv: if result.pv.is_empty() {
                None
            } else {
                Some(result.pv.iter().map(|m| m.to_usi()).collect())
            },
        });

        Ok(BackendSearchResult {
            best_move_usi,
            best_move,
            eval,
            elapsed_ms,
            timed_out: false,
            multipv_candidates,
            info_lines: Vec::new(),
        })
    }
}

// =============================================================================
// UsiBackend — EngineProcess ラッパー
// =============================================================================

/// USI プロトコル経由で外部エンジンを呼び出すバックエンド
pub struct UsiBackend {
    engine: EngineProcess,
}

impl UsiBackend {
    /// 既存の EngineProcess からバックエンドを作成
    pub fn new(engine: EngineProcess) -> Self {
        Self { engine }
    }

    /// 内部の EngineProcess への参照を返す
    pub fn engine(&self) -> &EngineProcess {
        &self.engine
    }

    /// 内部の EngineProcess への可変参照を返す
    pub fn engine_mut(&mut self) -> &mut EngineProcess {
        &mut self.engine
    }
}

impl SearchBackend for UsiBackend {
    fn prepare_game(&mut self, keep_tt: bool) -> Result<()> {
        if keep_tt {
            self.engine.sync_ready()
        } else {
            self.engine.new_game()
        }
    }

    fn search(&mut self, _pos: &Position, params: &SearchParams) -> Result<BackendSearchResult> {
        let engine_label = if params.side == Color::Black {
            "black"
        } else {
            "white"
        };

        let req = SearchRequest {
            sfen: &params.sfen,
            time_args: params.time_args,
            think_limit_ms: params.think_limit_ms,
            timeout_margin_ms: params.timeout_margin_ms,
            game_id: params.game_id,
            ply: params.ply,
            side: params.side,
            engine_label: engine_label.to_string(),
            pass_rights: params.pass_rights,
            go_depth: params.go_depth,
            go_nodes: params.go_nodes,
        };

        // info 行と MultiPV 候補を収集するコールバック
        let collect_info_lines = params.collect_info_lines;
        let mut info_lines: Vec<String> = Vec::new();
        let mut snapshot = InfoSnapshot::default();
        let mut info_cb = |line: &str, _req: &SearchRequest<'_>| {
            if collect_info_lines {
                info_lines.push(line.to_string());
            }
            snapshot.update_from_line(line);
        };

        let outcome = self.engine.search(&req, Some(&mut info_cb))?;

        // bestmove 文字列を Move にパース
        let best_move = outcome.bestmove.as_deref().and_then(|s| match s {
            "resign" | "win" | "none" | "timeout" => None,
            _ => Move::from_usi(s),
        });

        // InfoSnapshot の MultiPV 候補を BackendSearchResult 形式に変換
        let multipv_candidates = snapshot
            .multipv_candidates
            .iter()
            .filter_map(|c| {
                Move::from_usi(&c.first_move_usi).map(|mv| MultiPvCandidate {
                    multipv: c.multipv,
                    score_cp: c.score_cp.unwrap_or(match c.score_mate {
                        Some(m) if m > 0 => 30000,
                        Some(m) if m < 0 => -30000,
                        _ => 0,
                    }),
                    score_mate: c.score_mate,
                    first_move: mv,
                })
            })
            .collect();

        // eval は snapshot から構築（outcome.eval はコールバック有りの場合 None になるため）
        let eval = snapshot.into_eval_log().or(outcome.eval);

        Ok(BackendSearchResult {
            best_move_usi: outcome.bestmove,
            best_move,
            eval,
            elapsed_ms: outcome.elapsed_ms,
            timed_out: outcome.timed_out,
            multipv_candidates,
            info_lines,
        })
    }
}

// =============================================================================
// GameEngines — ゲームループ用の enum ディスパッチ
// =============================================================================

/// 対局ループ用のエンジンラッパー
///
/// NativeBackend は 1 インスタンスで両手番を処理、
/// UsiBackend は先手・後手で別インスタンスを使用。
/// UsiSingle は 1 インスタンスで両手番を処理（gensfen 用）。
pub enum GameEngines {
    /// rshogi-core 直接呼び出し（1 インスタンスで両手番）
    Native(Box<NativeBackend>),
    /// USI プロトコル経由（先手・後手で別インスタンス）
    Usi(Box<UsiEngines>),
    /// USI プロトコル経由（1 インスタンスで両手番、gensfen 用）
    UsiSingle(Box<UsiBackend>),
}

/// USI モードの先手・後手エンジンペア
pub struct UsiEngines {
    pub black: UsiBackend,
    pub white: UsiBackend,
}

impl GameEngines {
    /// 新しい対局の準備
    pub fn prepare_game(&mut self, keep_tt: bool) -> Result<()> {
        match self {
            Self::Native(e) => e.prepare_game(keep_tt),
            Self::Usi(engines) => {
                engines.black.prepare_game(keep_tt)?;
                engines.white.prepare_game(keep_tt)
            }
            Self::UsiSingle(e) => e.prepare_game(keep_tt),
        }
    }

    /// 指定手番側のエンジンで探索を実行
    pub fn search(
        &mut self,
        side: Color,
        pos: &Position,
        params: &SearchParams,
    ) -> Result<BackendSearchResult> {
        match self {
            Self::Native(e) => e.search(pos, params),
            Self::Usi(engines) => match side {
                Color::Black => engines.black.search(pos, params),
                Color::White => engines.white.search(pos, params),
            },
            Self::UsiSingle(e) => e.search(pos, params),
        }
    }
}
