//! USIプロトコルエンジン
//!
//! 将棋GUIとの通信を行うUSIプロトコル実装。

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::Result;
use engine_core::position::Position;
use engine_core::search::{init_search_module, LimitsType, Search, SearchInfo, SearchResult};
use engine_core::types::Move;

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
    /// 探索スレッドのハンドル
    search_thread: Option<thread::JoinHandle<(Search, SearchResult)>>,
    /// 探索停止用のフラグ（探索スレッドと共有）
    stop_flag: Option<Arc<AtomicBool>>,
    /// ponderhit通知フラグ
    ponderhit_flag: Option<Arc<AtomicBool>>,
}

impl UsiEngine {
    /// 新しいUSIエンジンを作成
    fn new() -> Self {
        // 探索モジュールの初期化
        init_search_module();

        let tt_size_mb = 256;

        Self {
            search: Some(Search::new(tt_size_mb)), // デフォルト256MB
            position: Position::new(),
            tt_size_mb,
            search_thread: None,
            stop_flag: None,
            ponderhit_flag: None,
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
        println!("option name USI_Ponder type check default false");
        println!("option name Stochastic_Ponder type check default false");
        println!("option name NetworkDelay type spin default 120 min 0 max 10000");
        println!("option name NetworkDelay2 type spin default 1120 min 0 max 10000");
        println!("option name MinimumThinkingTime type spin default 2000 min 1000 max 100000");
        println!("option name SlowMover type spin default 100 min 1 max 1000");
        println!("option name MaxMovesToDraw type spin default 100000 min 0 max 100000");
        println!("usiok");
    }

    /// isreadyコマンド: 準備完了を通知
    fn cmd_isready(&self) {
        // 必要な初期化があればここで行う
        println!("readyok");
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
            "MaxMovesToDraw" => {
                if let Ok(v) = value.parse::<i32>() {
                    if let Some(search) = self.search.as_mut() {
                        search.set_max_moves_to_draw(v);
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

        // 探索を別スレッドで開始
        let mut pos = Position::new();
        if let Err(e) = pos.set_sfen(&self.position.to_sfen()) {
            eprintln!("info string Error cloning position: {e}");
            return;
        }

        let mut search = self.search.take().unwrap_or_else(|| Search::new(self.tt_size_mb));
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
                        ) {
                            idx -= 1; // 巻き戻して次のループで処理
                            break;
                        }
                        if let Some(mv) = Move::from_usi(tokens[idx]) {
                            limits.search_moves.push(mv);
                        }
                        idx += 1;
                    }
                }
                _ => {}
            }
            idx += 1;
        }

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
                    self.search = Some(Search::new(self.tt_size_mb));
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
