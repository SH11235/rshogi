//! USIプロトコルエンジン
//!
//! 将棋GUIとの通信を行うUSIプロトコル実装。

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::Result;
use engine_core::eval::{set_material_level, MaterialLevel};
use engine_core::nnue::init_nnue;
use engine_core::position::Position;
use engine_core::search::{LimitsType, Search, SearchInfo, SearchResult};
use engine_core::types::Move;
use serde_json::json;

/// エンジン名
const ENGINE_NAME: &str = "Shogi Engine";
/// エンジンバージョン
const ENGINE_VERSION: &str = "0.1.0";
/// エンジン作者
const ENGINE_AUTHOR: &str = "sh11235";
/// 探索スレッド用のスタックサイズ（SearchWorkerが大きいため増やす）
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

/// USIエンジンの状態
struct UsiEngine {
    /// 探索エンジン
    search: Option<Search>,
    /// 現在の局面
    position: Position,
    /// 置換表サイズ（USI_Hashで変更）
    tt_size_mb: usize,
    /// MultiPV値
    multi_pv: usize,
    /// Skill Level オプション
    skill_options: engine_core::search::SkillOptions,
    /// 探索スレッドのハンドル
    search_thread: Option<thread::JoinHandle<(Search, SearchResult)>>,
    /// 探索停止用のフラグ（探索スレッドと共有）
    stop_flag: Option<Arc<AtomicBool>>,
    /// ponderhit通知フラグ
    ponderhit_flag: Option<Arc<AtomicBool>>,
    /// Large Pages使用メッセージの出力済みフラグ
    large_pages_reported: bool,
}

impl UsiEngine {
    /// 新しいUSIエンジンを作成
    fn new() -> Self {
        let tt_size_mb = 256;

        Self {
            search: Some(Search::new(tt_size_mb)), // デフォルト256MB
            position: Position::new(),
            tt_size_mb,
            multi_pv: 1,
            skill_options: engine_core::search::SkillOptions::default(),
            search_thread: None,
            stop_flag: None,
            ponderhit_flag: None,
            large_pages_reported: false,
        }
    }

    /// USIコマンドを処理
    fn process_command(&mut self, line: &str) -> Result<bool> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() {
            return Ok(true);
        }

        match tokens[0] {
            "usi" => {
                self.cmd_usi();
            }
            "isready" => {
                self.cmd_isready();
            }
            "setoption" => {
                self.cmd_setoption(&tokens);
            }
            "usinewgame" => {
                self.cmd_usinewgame();
            }
            "position" => {
                self.cmd_position(&tokens);
            }
            "go" => {
                self.cmd_go(&tokens);
            }
            "stop" => {
                self.cmd_stop();
            }
            "ponderhit" => {
                self.cmd_ponderhit();
            }
            "quit" => {
                self.cmd_stop();
                return Ok(false);
            }
            "gameover" => {
                self.cmd_stop();
            }
            // デバッグ用コマンド
            "d" | "display" => {
                self.cmd_display();
            }
            _ => {
                // 未知のコマンドは無視
            }
        }

        Ok(true)
    }

    /// usiコマンド: エンジン情報を出力
    fn cmd_usi(&self) {
        println!("id name {ENGINE_NAME} {ENGINE_VERSION}");
        println!("id author {ENGINE_AUTHOR}");
        println!();
        // オプション（将来的に追加）
        println!("option name USI_Hash type spin default 256 min 1 max 4096");
        println!("option name Threads type spin default 1 min 1 max 512");
        println!("option name USI_Ponder type check default false");
        println!("option name Stochastic_Ponder type check default false");
        println!("option name MultiPV type spin default 1 min 1 max 500");
        println!("option name NetworkDelay type spin default 120 min 0 max 10000");
        println!("option name NetworkDelay2 type spin default 1120 min 0 max 10000");
        println!("option name MinimumThinkingTime type spin default 2000 min 1000 max 100000");
        println!("option name SlowMover type spin default 100 min 1 max 1000");
        println!("option name MaxMovesToDraw type spin default 100000 min 0 max 100000");
        println!("option name Skill Level type spin default 20 min 0 max 20");
        println!("option name UCI_LimitStrength type check default false");
        println!("option name UCI_Elo type spin default 0 min 0 max 4000");
        println!("option name MaterialLevel type combo default 9 var 1 var 2 var 3 var 4 var 7 var 8 var 9");
        println!("option name EvalFile type string default <empty>");
        println!("usiok");
    }

    /// isreadyコマンド: 準備完了を通知
    fn cmd_isready(&mut self) {
        // 必要な初期化があればここで行う
        self.maybe_report_large_pages();
        println!("readyok");
    }

    fn maybe_report_large_pages(&mut self) {
        if self.large_pages_reported {
            return;
        }

        let Some(search) = self.search.as_ref() else {
            return;
        };
        if !search.tt_uses_large_pages() {
            return;
        }

        // Windows: VirtualAlloc with MEM_LARGE_PAGES
        // Linux: madvise(MADV_HUGEPAGE) によるhugepageヒント
        let payload = json!({
            "type": "info",
            "message": "Large Pages are used.",
        });
        println!("info string {}", payload);
        self.large_pages_reported = true;
    }

    /// setoptionコマンド: オプション設定
    fn cmd_setoption(&mut self, tokens: &[&str]) {
        // 探索中の設定変更は避ける
        self.wait_for_search();

        // setoption name <name> value <value>
        let mut name = String::new();
        let mut value = String::new();
        let mut parsing_name = false;
        let mut parsing_value = false;

        for token in tokens.iter().skip(1) {
            match *token {
                "name" => {
                    parsing_name = true;
                    parsing_value = false;
                }
                "value" => {
                    parsing_name = false;
                    parsing_value = true;
                }
                _ => {
                    if parsing_name {
                        if !name.is_empty() {
                            name.push(' ');
                        }
                        name.push_str(token);
                    } else if parsing_value {
                        if !value.is_empty() {
                            value.push(' ');
                        }
                        value.push_str(token);
                    }
                }
            }
        }

        // オプションを適用
        match name.as_str() {
            "USI_Hash" => {
                if let Ok(size) = value.parse::<usize>() {
                    if let Some(search) = self.search.as_mut() {
                        search.resize_tt(size);
                        self.tt_size_mb = size;
                    }
                    self.maybe_report_large_pages();
                }
            }
            "Threads" => {
                if let Ok(num) = value.parse::<usize>() {
                    if let Some(search) = self.search.as_mut() {
                        search.set_num_threads(num);
                    }
                }
            }
            "NetworkDelay" => {
                if let Ok(v) = value.parse::<i64>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = search.time_options();
                        opts.network_delay = v;
                        search.set_time_options(opts);
                    }
                }
            }
            "NetworkDelay2" => {
                if let Ok(v) = value.parse::<i64>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = search.time_options();
                        opts.network_delay2 = v;
                        search.set_time_options(opts);
                    }
                }
            }
            "MinimumThinkingTime" => {
                if let Ok(v) = value.parse::<i64>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = search.time_options();
                        opts.minimum_thinking_time = v;
                        search.set_time_options(opts);
                    }
                }
            }
            "SlowMover" => {
                if let Ok(v) = value.parse::<i32>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = search.time_options();
                        opts.slow_mover = v;
                        search.set_time_options(opts);
                    }
                }
            }
            "USI_Ponder" => {
                if let Ok(v) = value.parse::<bool>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = search.time_options();
                        opts.usi_ponder = v;
                        search.set_time_options(opts);
                    }
                }
            }
            "Stochastic_Ponder" => {
                if let Ok(v) = value.parse::<bool>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = search.time_options();
                        opts.stochastic_ponder = v;
                        search.set_time_options(opts);
                    }
                }
            }
            "Skill Level" => {
                if let Ok(v) = value.parse::<i32>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = self.skill_options;
                        opts.skill_level = v.clamp(0, 20);
                        self.skill_options = opts;
                        search.set_skill_options(opts);
                    }
                }
            }
            "UCI_LimitStrength" => {
                if let Ok(v) = value.parse::<bool>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = self.skill_options;
                        opts.uci_limit_strength = v;
                        self.skill_options = opts;
                        search.set_skill_options(opts);
                    }
                }
            }
            "UCI_Elo" => {
                if let Ok(v) = value.parse::<i32>() {
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = self.skill_options;
                        opts.uci_elo = v;
                        self.skill_options = opts;
                        search.set_skill_options(opts);
                    }
                }
            }
            "MaxMovesToDraw" => {
                if let Ok(v) = value.parse::<i32>() {
                    if let Some(search) = self.search.as_mut() {
                        search.set_max_moves_to_draw(v);
                    }
                }
            }
            "MultiPV" => {
                if let Ok(v) = value.parse::<usize>() {
                    self.multi_pv = v;
                }
            }
            "MaterialLevel" => {
                if let Ok(v) = value.parse::<u8>() {
                    if let Some(level) = MaterialLevel::from_value(v) {
                        set_material_level(level);
                    } else {
                        eprintln!("info string Warning: Invalid MaterialLevel value {v}, ignored");
                    }
                } else {
                    eprintln!("info string Warning: MaterialLevel parse error for '{value}'");
                }
            }
            "EvalFile" => {
                if value.is_empty() || value == "<empty>" {
                    // 空の場合は何もしない
                } else {
                    match init_nnue(&value) {
                        Ok(()) => {
                            let payload = json!({
                                "type": "info",
                                "message": format!("NNUE loaded: {value}"),
                            });
                            eprintln!("info string {payload}");
                        }
                        Err(e) => {
                            eprintln!("info string Error loading NNUE file: {e}");
                        }
                    }
                }
            }
            _ => {
                // 未知のオプションは無視
            }
        }
    }

    /// usinewgameコマンド: 新しい対局の開始
    fn cmd_usinewgame(&mut self) {
        self.cmd_stop();

        if let Some(search) = self.search.as_mut() {
            search.clear_tt();
            search.clear_histories(); // YaneuraOu準拠：履歴統計もクリア
        }
        self.position = Position::new();
    }

    /// positionコマンド: 局面設定
    fn cmd_position(&mut self, tokens: &[&str]) {
        // position [sfen <sfen> | startpos] [moves <move1> <move2> ...]
        let mut idx = 1;
        if idx >= tokens.len() {
            return;
        }

        // 局面の設定
        if tokens[idx] == "startpos" {
            self.position.set_hirate();
            idx += 1;
        } else if tokens[idx] == "sfen" {
            idx += 1;
            // SFENを収集（movesの前まで）
            let mut sfen_parts = Vec::new();
            while idx < tokens.len() && tokens[idx] != "moves" {
                sfen_parts.push(tokens[idx]);
                idx += 1;
            }
            let sfen = sfen_parts.join(" ");
            if let Err(e) = self.position.set_sfen(&sfen) {
                eprintln!("info string Error parsing SFEN: {e}");
                return;
            }
        }

        // 指し手の適用
        if idx < tokens.len() && tokens[idx] == "moves" {
            idx += 1;
            while idx < tokens.len() {
                if let Some(mv) = Move::from_usi(tokens[idx]) {
                    let gives_check = self.position.gives_check(mv);
                    self.position.do_move(mv, gives_check);
                } else {
                    eprintln!("info string Error parsing move: {token}", token = tokens[idx]);
                    break;
                }
                idx += 1;
            }
        }
    }

    /// goコマンド: 探索開始
    fn cmd_go(&mut self, tokens: &[&str]) {
        // 既存の探索を停止
        self.cmd_stop();

        // 制限を解析
        let limits = self.parse_go_options(tokens);

        // 探索を別スレッドで開始（千日手判定のため履歴ごと複製する）
        let mut pos = self.position.clone();

        let mut search = self.search.take().unwrap_or_else(|| Search::new(self.tt_size_mb));
        search.set_skill_options(self.skill_options);
        let stop_flag = search.stop_flag();
        let ponderhit_flag = search.ponderhit_flag();
        self.stop_flag = Some(stop_flag.clone());
        self.ponderhit_flag = Some(ponderhit_flag.clone());

        let builder = thread::Builder::new().stack_size(SEARCH_STACK_SIZE);
        self.search_thread = Some(
            builder
                .spawn(move || {
                    let result = search.go(
                        &mut pos,
                        limits,
                        Some(|info: &SearchInfo| {
                            println!("{}", info.to_usi_string());
                            std::io::stdout().flush().ok();
                        }),
                    );

                    let best_usi = if result.best_move != Move::NONE {
                        result.best_move.to_usi()
                    } else {
                        "resign".to_string()
                    };

                    if result.ponder_move != Move::NONE {
                        println!("bestmove {best_usi} ponder {}", result.ponder_move.to_usi());
                    } else {
                        println!("bestmove {best_usi}");
                    }
                    std::io::stdout().flush().ok();

                    (search, result)
                })
                .expect("failed to spawn search thread"),
        );
    }

    /// goオプションを解析
    fn parse_go_options(&self, tokens: &[&str]) -> LimitsType {
        let mut limits = LimitsType::default();
        // YaneuraOu準拠: go受信時点で探索開始時刻を記録し、この時刻を基準に時間管理する
        limits.set_start_time();
        let mut idx = 1;

        while idx < tokens.len() {
            match tokens[idx] {
                "infinite" => {
                    limits.infinite = true;
                }
                "ponder" => {
                    limits.ponder = true;
                }
                "depth" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.depth = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "nodes" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.nodes = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "movetime" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.movetime = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "mate" => {
                    idx += 1;
                    // `go mate` without a value is treated as infinite (YaneuraOu互換)
                    limits.mate = if idx < tokens.len() {
                        match tokens[idx] {
                            "infinite" => i32::MAX,
                            v => v.parse().unwrap_or(0),
                        }
                    } else {
                        i32::MAX
                    };
                }
                "btime" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.time[0] = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "wtime" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.time[1] = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "binc" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.inc[0] = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "winc" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.inc[1] = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "byoyomi" => {
                    idx += 1;
                    if idx < tokens.len() {
                        let byoyomi: i64 = tokens[idx].parse().unwrap_or(0);
                        limits.byoyomi[0] = byoyomi;
                        limits.byoyomi[1] = byoyomi;
                    }
                }
                "rtime" => {
                    idx += 1;
                    if idx < tokens.len() {
                        limits.rtime = tokens[idx].parse().unwrap_or(0);
                    }
                }
                "searchmoves" => {
                    // searchmoves <move1> <move2> ...
                    idx += 1;
                    while idx < tokens.len() {
                        // 他のオプションに当たったら終了
                        if matches!(
                            tokens[idx],
                            "infinite"
                                | "ponder"
                                | "depth"
                                | "nodes"
                                | "movetime"
                                | "btime"
                                | "wtime"
                                | "binc"
                                | "winc"
                                | "byoyomi"
                                | "rtime"
                                | "mate"
                        ) {
                            idx -= 1; // 巻き戻して次のループで処理
                            break;
                        }
                        if let Some(mv) = Move::from_usi(tokens[idx]) {
                            if let Some(normalized) = self.position.to_move(mv) {
                                limits.search_moves.push(normalized);
                            } else {
                                eprintln!("warning: invalid searchmoves: {}", tokens[idx]);
                            }
                        }
                        idx += 1;
                    }
                }
                _ => {}
            }
            idx += 1;
        }

        // MultiPVを設定
        limits.multi_pv = self.multi_pv;

        limits
    }

    /// stopコマンド: 探索停止
    fn cmd_stop(&mut self) {
        if let Some(stop_flag) = &self.stop_flag {
            stop_flag.store(true, Ordering::SeqCst);
        }
        self.wait_for_search();
    }

    /// ponderhitコマンド: 先読みヒットを通知（現状は停止扱い）
    fn cmd_ponderhit(&mut self) {
        if let Some(flag) = &self.ponderhit_flag {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// 探索スレッドの終了を待ち、Searchを取り戻す
    fn wait_for_search(&mut self) {
        if let Some(handle) = self.search_thread.take() {
            match handle.join() {
                Ok((search, _result)) => {
                    self.search = Some(search);
                }
                Err(_) => {
                    eprintln!("info string search thread panicked, resetting Search");
                    let mut search = Search::new(self.tt_size_mb);
                    search.set_skill_options(self.skill_options);
                    self.search = Some(search);
                }
            }
        }
        self.stop_flag = None;
        self.ponderhit_flag = None;
    }

    /// displayコマンド: 現在の局面を表示（デバッグ用）
    fn cmd_display(&self) {
        println!("SFEN: {}", self.position.to_sfen());
        println!("Side to move: {:?}", self.position.side_to_move());
        println!("Game ply: {}", self.position.game_ply());
    }
}

fn main() -> Result<()> {
    // ロガー初期化（標準エラー出力）
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    let mut engine = UsiEngine::new();
    let stdin = io::stdin();

    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();

        if !engine.process_command(line)? {
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 履歴統計の初期化がスタックを大量に消費するため、別スレッドで実行
    const STACK_SIZE: usize = 64 * 1024 * 1024;

    #[test]
    fn parse_go_mate_sets_limits() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let engine = UsiEngine::new();
                let tokens = vec!["go", "mate", "5"];

                let limits = engine.parse_go_options(&tokens);
                assert_eq!(limits.mate, 5);
                assert!(!limits.use_time_management(), "mate search disables time management");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn parse_go_mate_without_value_defaults_to_infinite() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let engine = UsiEngine::new();
                let tokens = vec!["go", "mate"];

                let limits = engine.parse_go_options(&tokens);
                assert_eq!(limits.mate, i32::MAX);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn parse_go_mate_infinite_defaults_to_max() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let engine = UsiEngine::new();
                let tokens = vec!["go", "mate", "infinite"];

                let limits = engine.parse_go_options(&tokens);
                assert_eq!(limits.mate, i32::MAX);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
