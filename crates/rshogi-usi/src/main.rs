//! USIプロトコルエンジン
//!
//! 将棋GUIとの通信を行うUSIプロトコル実装。

use std::io::{self, BufRead, Write};
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::Result;
use rshogi_core::eval::{
    DEFAULT_PASS_RIGHT_VALUE_EARLY, DEFAULT_PASS_RIGHT_VALUE_LATE, MaterialLevel, disable_material,
    is_material_enabled, set_eval_hash_enabled, set_material_level, set_pass_move_bonus,
    set_pass_right_value_phased,
};
use rshogi_core::nnue::{
    AccumulatorStackVariant, LayerStackBucketMode, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, clear_nnue,
    evaluate_dispatch, get_network, init_nnue, parse_layer_stack_bucket_mode,
    parse_nnue_architecture, print_nnue_stats, reset_layer_stack_progress_kpabs_weights,
    set_fv_scale_override, set_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
    set_nnue_architecture_override,
};
use rshogi_core::position::Position;
use rshogi_core::search::{
    DEFAULT_DRAW_VALUE_BLACK, DEFAULT_DRAW_VALUE_WHITE, LimitsType, Search, SearchInfo,
    SearchResult, SearchTuneParams,
};
use rshogi_core::types::{EnteringKingRule, Move};
use serde_json::json;

/// エンジン名
const ENGINE_NAME: &str = "Shogi Engine";
/// エンジンバージョン
const ENGINE_VERSION: &str = "0.1.0";
/// エンジン作者
const ENGINE_AUTHOR: &str = "sh11235";
/// 探索スレッド用のスタックサイズ（SearchWorkerが大きいため増やす）
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

fn load_progress_coeff_kpabs(path: &str) -> Result<Box<[f32]>, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("failed to read LS_PROGRESS_COEFF '{path}': {e}"))?;
    let expected = SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * size_of::<f64>();
    if bytes.len() != expected {
        return Err(format!(
            "progress.bin size mismatch: got {} bytes, expected {}",
            bytes.len(),
            expected
        ));
    }

    let weights: Vec<f32> = bytes
        .chunks_exact(size_of::<f64>())
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("chunk size is checked")) as f32)
        .collect();

    Ok(weights.into_boxed_slice())
}

/// USIエンジンの状態
struct UsiEngine {
    /// 探索エンジン
    search: Option<Search>,
    /// 現在の局面
    position: Position,
    /// 置換表サイズ（USI_Hashで変更）
    tt_size_mb: usize,
    /// 評価ハッシュサイズ（EvalHashで変更）
    eval_hash_size_mb: usize,
    /// EvalHash使用フラグ（UseEvalHashで変更）
    use_eval_hash: bool,
    /// MultiPV値
    multi_pv: usize,
    /// Skill Level オプション
    skill_options: rshogi_core::search::SkillOptions,
    /// 探索スレッドのハンドル
    search_thread: Option<thread::JoinHandle<(Search, SearchResult)>>,
    /// 探索停止用のフラグ（探索スレッドと共有）
    stop_flag: Option<Arc<AtomicBool>>,
    /// ponderhit通知フラグ
    ponderhit_flag: Option<Arc<AtomicBool>>,
    /// bestmove出力抑制フラグ（cmd_go内部でcmd_stopする際に使用）
    suppress_bestmove: Arc<AtomicBool>,
    /// Stochastic_Ponder オプションのミラー
    stochastic_ponder: bool,
    /// 直近の position コマンド文字列（Stochastic_Ponder の再始動用）
    last_position_cmd: Option<String>,
    /// 直近の go コマンド文字列（Stochastic_Ponder の再始動用）
    last_go_cmd: Option<String>,
    /// EvalFile の明示指定状態
    /// None: 未指定（eval/nn.bin 自動ロード対象）
    /// Some(true): 明示指定されロード成功
    /// Some(false): 明示指定されたがロード失敗
    eval_file_explicit: Option<bool>,
    /// 最後に指定された EvalFile パス（NNUE_ARCHITECTURE 変更時の再読込用）
    eval_file_path: Option<String>,
    /// SPSAParamsFile の明示指定パス（setoption で設定）
    spsa_params_file: Option<String>,
    /// SPSA params ファイルの読み込み済みフラグ
    spsa_params_loaded: bool,
    /// Large Pages使用メッセージの出力済みフラグ
    large_pages_reported: bool,
    // --- 有限パス権（Finite Pass Rights）関連 ---
    /// パス権ルール有効化フラグ
    pass_rights_enabled: bool,
    /// 初期パス権数（デフォルト2）
    initial_pass_count: u8,
    /// パス権評価値（序盤）
    pass_right_value_early: i32,
    /// パス権評価値（終盤）
    pass_right_value_late: i32,
}

impl UsiEngine {
    /// 新しいUSIエンジンを作成
    fn new() -> Self {
        let tt_size_mb = 256;
        let eval_hash_size_mb = 256;
        let use_eval_hash = true;

        // グローバルフラグをデフォルト値で初期化
        // （USI GUIがsetoptionを送らない場合に備える）
        set_eval_hash_enabled(use_eval_hash);

        Self {
            // EvalHash は最初の `go` 直前まで遅延確保する。
            // selfplay のように起動直後に setoption でサイズを下げるケースで、
            // 先に既定 256MB を確保してしまう無駄を避ける。
            search: Some(Search::new_with_eval_hash(tt_size_mb, 0)),
            position: Position::new(),
            tt_size_mb,
            eval_hash_size_mb,
            use_eval_hash,
            multi_pv: 1,
            skill_options: rshogi_core::search::SkillOptions::default(),
            search_thread: None,
            stop_flag: None,
            ponderhit_flag: None,
            suppress_bestmove: Arc::new(AtomicBool::new(false)),
            stochastic_ponder: false,
            last_position_cmd: None,
            last_go_cmd: None,
            eval_file_explicit: None,
            eval_file_path: None,
            spsa_params_file: None,
            spsa_params_loaded: false,
            large_pages_reported: false,
            pass_rights_enabled: false,
            initial_pass_count: 2,
            pass_right_value_early: DEFAULT_PASS_RIGHT_VALUE_EARLY,
            pass_right_value_late: DEFAULT_PASS_RIGHT_VALUE_LATE,
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
                self.last_position_cmd = Some(line.to_string());
                self.cmd_position(&tokens);
            }
            "go" => {
                self.last_go_cmd = Some(line.to_string());
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
                // NNUE統計を出力（nnue-stats feature有効時のみ実際に出力）
                print_nnue_stats();
                return Ok(false);
            }
            "gameover" => {
                self.cmd_stop();
            }
            // デバッグ用コマンド
            "d" | "display" => {
                self.cmd_display();
            }
            "eval" => {
                let diagnostics = tokens.get(1).is_some_and(|s| *s == "diag");
                self.cmd_eval(diagnostics);
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
        println!(
            "option name DrawValueBlack type spin default {DEFAULT_DRAW_VALUE_BLACK} min -30000 max 30000"
        );
        println!(
            "option name DrawValueWhite type spin default {DEFAULT_DRAW_VALUE_WHITE} min -30000 max 30000"
        );
        println!("option name EvalHash type spin default 256 min 0 max 4096");
        println!("option name UseEvalHash type check default true");
        println!("option name Skill Level type spin default 20 min 0 max 20");
        println!("option name UCI_LimitStrength type check default false");
        println!("option name UCI_Elo type spin default 0 min 0 max 4000");
        println!(
            "option name MaterialLevel type combo default none var none var 1 var 2 var 3 var 4 var 7 var 8 var 9"
        );
        println!("option name EvalFile type string default eval/nn.bin");
        println!(
            "option name EnteringKingRule type combo default CSARule27 var NoEnteringKing var CSARule24 var CSARule24H var CSARule27 var CSARule27H var TryRule"
        );
        // FV_SCALE: 0=自動判定、1以上=指定値でオーバーライド
        // 水匠5等は24、YaneuraOuデフォルトは16
        println!("option name FV_SCALE type spin default 0 min 0 max 100");
        println!(
            "option name LS_BUCKET_MODE type combo default {} var progress8kpabs",
            LayerStackBucketMode::Progress8KPAbs.as_str()
        );
        println!("option name LS_PROGRESS_COEFF type string default <empty>");
        println!(
            "option name NNUE_ARCHITECTURE type combo default auto var auto var halfkp var halfka_hm var halfka var layerstacks var layerstacks-psqt"
        );
        // 有限パス権（Finite Pass Rights）オプション
        println!("option name PassRights type check default false");
        println!("option name InitialPassCount type spin default 2 min 0 max 10");
        println!("option name PassMoveBonus type spin default 0 min -1000 max 1000");
        println!(
            "option name PassRightValueEarly type spin default {DEFAULT_PASS_RIGHT_VALUE_EARLY} min 0 max 500"
        );
        println!(
            "option name PassRightValueLate type spin default {DEFAULT_PASS_RIGHT_VALUE_LATE} min 0 max 500"
        );
        println!("option name SPSAParamsFile type string default <auto>");
        for spec in SearchTuneParams::option_specs() {
            println!(
                "option name {} type spin default {} min {} max {}",
                spec.usi_name, spec.default, spec.min, spec.max
            );
        }
        println!("usiok");
    }

    /// isreadyコマンド: 準備完了を通知
    /// YaneuraOu準拠: isready 受信時にTTをクリアする
    fn cmd_isready(&mut self) {
        if let Some(search) = self.search.as_mut() {
            search.clear_tt();
        }
        // EvalFile の状態を確認し、必要なら NNUE をロード
        match self.eval_file_explicit {
            Some(false) => {
                // EvalFile が明示指定されたがロード失敗 → 致命的エラー
                // eval/nn.bin への暗黙フォールバックはしない
                panic!(
                    "EvalFile was explicitly set but failed to load. \
                     Fix the path or remove the setoption."
                );
            }
            Some(true) => {
                // EvalFile が明示指定されロード成功 → 何もしない
            }
            None if !is_material_enabled() && get_network().is_none() => {
                // EvalFile 未指定 + Material 未指定 + NNUE 未ロード → eval/nn.bin を自動ロード
                const DEFAULT_EVAL_FILE: &str = "eval/nn.bin";
                if std::path::Path::new(DEFAULT_EVAL_FILE).exists() {
                    match init_nnue(DEFAULT_EVAL_FILE) {
                        Ok(()) => {
                            let payload = json!({
                                "type": "info",
                                "message": format!("NNUE auto-loaded: {DEFAULT_EVAL_FILE}"),
                            });
                            eprintln!("info string {payload}");
                        }
                        Err(e) => {
                            panic!("Failed to load default NNUE file {DEFAULT_EVAL_FILE}: {e}");
                        }
                    }
                } else {
                    panic!(
                        "No NNUE file loaded and {DEFAULT_EVAL_FILE} not found. \
                         Use 'setoption name EvalFile value <path>' or \
                         'setoption name MaterialLevel value <n>'."
                    );
                }
            }
            None => {
                // EvalFile 未指定だが Material 有効 or NNUE 既ロード → 何もしない
            }
        }
        // progress8kpabs モードで LS_PROGRESS_COEFF 未指定の場合はエラー
        // NNUE が未ロード（Material 評価）の場合は bucket mode は使われないのでスキップ
        if get_network().is_some() {
            use rshogi_core::nnue::{
                LayerStackBucketMode, get_layer_stack_bucket_mode,
                get_layer_stack_progress_kpabs_weights,
            };
            if get_layer_stack_bucket_mode() == LayerStackBucketMode::Progress8KPAbs
                && get_layer_stack_progress_kpabs_weights().iter().all(|&w| w == 0.0)
            {
                panic!(
                    "LS_BUCKET_MODE=progress8kpabs requires LS_PROGRESS_COEFF to be set. \
                     All weights are zero (progress.bin not loaded). \
                     Use 'setoption name LS_PROGRESS_COEFF value <path>' before isready."
                );
            }
        }
        self.maybe_load_spsa_params();
        self.maybe_report_large_pages();
        println!("readyok");
    }

    /// SPSA params ファイルの自動/明示読み込み。
    /// 優先順位: 1. SPSAParamsFile で明示指定 2. バイナリ同ディレクトリの spsa.params 3. なし
    fn maybe_load_spsa_params(&mut self) {
        if self.spsa_params_loaded {
            return;
        }
        self.spsa_params_loaded = true;

        let path = if let Some(ref explicit) = self.spsa_params_file {
            std::path::PathBuf::from(explicit)
        } else {
            // バイナリと同じディレクトリの spsa.params を探す
            let exe_dir =
                std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
            match exe_dir {
                Some(dir) => dir.join("spsa.params"),
                None => return,
            }
        };

        if !path.exists() {
            if self.spsa_params_file.is_some() {
                eprintln!("info string Warning: SPSAParamsFile not found: {}", path.display());
            }
            return;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("info string Warning: failed to read {}: {e}", path.display());
                return;
            }
        };

        let mut applied = 0usize;
        let mut clamped = 0usize;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // .params format: name,type,value,min,max,c_end,r_end [// comment] [[[NOT USED]]]
            let val_part = trimmed.split("//").next().unwrap_or(trimmed);
            let val_part = val_part.replace("[[NOT USED]]", "");
            let cols: Vec<&str> = val_part.split(',').map(str::trim).collect();
            if cols.len() < 3 {
                continue;
            }
            let name = cols[0];
            let type_name = cols[1];
            let value_str = cols[2];

            let parsed = if type_name.eq_ignore_ascii_case("int") {
                match value_str.parse::<f64>() {
                    Ok(v) => v.round() as i32,
                    Err(_) => continue,
                }
            } else {
                match value_str.parse::<i32>() {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            };

            if let Some(search) = self.search.as_mut()
                && let Some(result) = search.set_search_tune_option(name, parsed)
            {
                applied += 1;
                if result.clamped {
                    clamped += 1;
                }
            }
        }

        if applied > 0 {
            eprintln!(
                "info string SPSA params loaded: {} parameters from {} (clamped: {})",
                applied,
                path.display(),
                clamped
            );
        }
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
        if name.starts_with("SPSA_") {
            let parsed = match value.parse::<i32>() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("info string Warning: invalid SPSA value '{}'", value);
                    return;
                }
            };
            if let Some(search) = self.search.as_mut()
                && let Some(result) = search.set_search_tune_option(name.as_str(), parsed)
            {
                if result.clamped {
                    eprintln!(
                        "info string Warning: {}={} is out of range, clamped to {} ({}..{})",
                        name, parsed, result.applied, result.min, result.max
                    );
                }
                return;
            }
        }

        match name.as_str() {
            "SPSAParamsFile" => {
                if value == "<auto>" || value == "<empty>" || value.is_empty() {
                    self.spsa_params_file = None;
                } else {
                    self.spsa_params_file = Some(value.to_string());
                }
                // 明示指定時は再読み込みを強制
                self.spsa_params_loaded = false;
            }
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
                if let Ok(num) = value.parse::<usize>()
                    && let Some(search) = self.search.as_mut()
                {
                    search.set_num_threads(num);
                }
            }
            "NetworkDelay" => {
                if let Ok(v) = value.parse::<i64>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = search.time_options();
                    opts.network_delay = v;
                    search.set_time_options(opts);
                }
            }
            "NetworkDelay2" => {
                if let Ok(v) = value.parse::<i64>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = search.time_options();
                    opts.network_delay2 = v;
                    search.set_time_options(opts);
                }
            }
            "MinimumThinkingTime" => {
                if let Ok(v) = value.parse::<i64>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = search.time_options();
                    opts.minimum_thinking_time = v;
                    search.set_time_options(opts);
                }
            }
            "SlowMover" => {
                if let Ok(v) = value.parse::<i32>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = search.time_options();
                    opts.slow_mover = v;
                    search.set_time_options(opts);
                }
            }
            "USI_Ponder" => {
                if let Ok(v) = value.parse::<bool>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = search.time_options();
                    opts.usi_ponder = v;
                    search.set_time_options(opts);
                }
            }
            "Stochastic_Ponder" => {
                if let Ok(v) = value.parse::<bool>() {
                    self.stochastic_ponder = v;
                    if let Some(search) = self.search.as_mut() {
                        let mut opts = search.time_options();
                        opts.stochastic_ponder = v;
                        search.set_time_options(opts);
                    }
                }
            }
            "Skill Level" => {
                if let Ok(v) = value.parse::<i32>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = self.skill_options;
                    opts.skill_level = v.clamp(0, 20);
                    self.skill_options = opts;
                    search.set_skill_options(opts);
                }
            }
            "UCI_LimitStrength" => {
                if let Ok(v) = value.parse::<bool>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = self.skill_options;
                    opts.uci_limit_strength = v;
                    self.skill_options = opts;
                    search.set_skill_options(opts);
                }
            }
            "UCI_Elo" => {
                if let Ok(v) = value.parse::<i32>()
                    && let Some(search) = self.search.as_mut()
                {
                    let mut opts = self.skill_options;
                    opts.uci_elo = v;
                    self.skill_options = opts;
                    search.set_skill_options(opts);
                }
            }
            "EvalHash" => {
                if let Ok(size) = value.parse::<usize>()
                    && let Some(search) = self.search.as_mut()
                {
                    search.resize_eval_hash(size);
                    self.eval_hash_size_mb = size;
                }
            }
            "UseEvalHash" => {
                let v = value == "true" || value == "1";
                self.use_eval_hash = v;
                set_eval_hash_enabled(v);
            }
            "MaxMovesToDraw" => {
                if let Ok(v) = value.parse::<i32>()
                    && let Some(search) = self.search.as_mut()
                {
                    search.set_max_moves_to_draw(v);
                }
            }
            "DrawValueBlack" => {
                if let Ok(v) = value.parse::<i32>()
                    && let Some(search) = self.search.as_mut()
                {
                    search.set_draw_value_black(v);
                }
            }
            "DrawValueWhite" => {
                if let Ok(v) = value.parse::<i32>()
                    && let Some(search) = self.search.as_mut()
                {
                    search.set_draw_value_white(v);
                }
            }
            "MultiPV" => {
                if let Ok(v) = value.parse::<usize>() {
                    self.multi_pv = v;
                }
            }
            "MaterialLevel" => {
                if value == "none" {
                    disable_material();
                } else if let Ok(v) = value.parse::<u8>() {
                    if let Some(level) = MaterialLevel::from_value(v) {
                        set_material_level(level);
                    } else {
                        eprintln!("info string Warning: Invalid MaterialLevel value {v}, ignored");
                    }
                } else {
                    eprintln!("info string Warning: MaterialLevel parse error for '{value}'");
                }
            }
            "EnteringKingRule" => {
                if let Some(rule) = EnteringKingRule::from_usi(&value) {
                    // search は new() で常に Some だが、既存パターンに合わせて防御的にチェック
                    if let Some(search) = self.search.as_mut() {
                        search.set_entering_king_rule(rule);
                    }
                } else {
                    eprintln!("info string Warning: unknown EnteringKingRule '{value}'");
                }
            }
            "EvalFile" => {
                if value.is_empty() || value == "<empty>" {
                    // 空 → 明示指定を解除し isready の自動ロードに戻す
                    clear_nnue();
                    self.eval_file_explicit = None;
                    self.eval_file_path = None;
                } else {
                    // パス指定: ロード試行し、結果を記録
                    self.eval_file_path = Some(value.to_string());
                    match init_nnue(&value) {
                        Ok(()) => {
                            self.eval_file_explicit = Some(true);
                            let payload = json!({
                                "type": "info",
                                "message": format!("NNUE loaded: {value}"),
                            });
                            eprintln!("info string {payload}");
                        }
                        Err(e) => {
                            self.eval_file_explicit = Some(false);
                            eprintln!("info string Error loading NNUE file: {e}");
                        }
                    }
                }
            }
            "FV_SCALE" => {
                if let Ok(v) = value.parse::<i32>() {
                    set_fv_scale_override(v);
                    if v == 0 {
                        eprintln!("info string FV_SCALE: auto-detect");
                    } else {
                        eprintln!("info string FV_SCALE: {v}");
                    }
                }
            }
            "NNUE_ARCHITECTURE" => match parse_nnue_architecture(&value) {
                Some(mode) => {
                    set_nnue_architecture_override(mode);
                    // EvalFile が指定済みなら、現在ロード済みか失敗済みかに関係なく再試行する。
                    // arch_str 不整合が原因でロード失敗していた場合、architecture override
                    // 変更後の再試行で成功する可能性がある。再試行しても失敗した場合は
                    // Some(false) のまま維持され、isready の panic 安全策は保持される。
                    if let Some(ref path) = self.eval_file_path {
                        let was_loaded = get_network().is_some();
                        match init_nnue(path) {
                            Ok(()) => {
                                self.eval_file_explicit = Some(true);
                                let action = if was_loaded {
                                    "reloaded"
                                } else {
                                    "retried and loaded"
                                };
                                eprintln!(
                                    "info string NNUE_ARCHITECTURE: {} ({} {})",
                                    value, action, path
                                );
                            }
                            Err(e) => {
                                self.eval_file_explicit = Some(false);
                                let action = if was_loaded {
                                    "reload failed"
                                } else {
                                    "retry failed"
                                };
                                eprintln!(
                                    "info string NNUE_ARCHITECTURE: {} ({}: {})",
                                    value, action, e
                                );
                            }
                        }
                    } else if get_network().is_some() {
                        // EvalFile 未指定で自動ロード済み → クリアして isready に任せる
                        clear_nnue();
                        self.eval_file_explicit = None;
                        eprintln!(
                            "info string NNUE_ARCHITECTURE: {} (NNUE cleared, will reload on isready)",
                            value
                        );
                    } else {
                        eprintln!("info string NNUE_ARCHITECTURE: {}", value);
                    }
                }
                None => {
                    eprintln!(
                        "info string Warning: invalid NNUE_ARCHITECTURE '{}', expected auto, halfkp, halfka_hm, halfka, layerstacks or layerstacks-psqt",
                        value
                    );
                }
            },
            "LS_BUCKET_MODE" => match parse_layer_stack_bucket_mode(&value) {
                Some(mode) => {
                    set_layer_stack_bucket_mode(mode);
                    eprintln!("info string LS_BUCKET_MODE: {}", mode.as_str());
                }
                None => {
                    eprintln!(
                        "info string Warning: invalid LS_BUCKET_MODE '{}', expected progress8kpabs",
                        value
                    );
                }
            },
            "LS_PROGRESS_COEFF" => {
                if value.is_empty() || value == "<empty>" {
                    reset_layer_stack_progress_kpabs_weights();
                    eprintln!("info string LS_PROGRESS_COEFF: reset to built-in default");
                } else {
                    match load_progress_coeff_kpabs(&value) {
                        Ok(weights) => match set_layer_stack_progress_kpabs_weights(weights) {
                            Ok(()) => {
                                eprintln!("info string LS_PROGRESS_COEFF loaded (kpabs): {value}");
                            }
                            Err(err) => {
                                eprintln!("info string Warning: {err}");
                            }
                        },
                        Err(err) => {
                            eprintln!("info string Warning: {err}");
                        }
                    }
                }
            }
            "PassRights" => {
                let v = value == "true" || value == "1";
                self.pass_rights_enabled = v;
                eprintln!("info string PassRights: {}", if v { "enabled" } else { "disabled" });
            }
            "InitialPassCount" => {
                if let Ok(v) = value.parse::<u8>() {
                    self.initial_pass_count = v.clamp(0, 10);
                    eprintln!("info string InitialPassCount: {}", self.initial_pass_count);
                }
            }
            "PassMoveBonus" => {
                if let Ok(v) = value.parse::<i32>() {
                    let clamped = v.clamp(-1000, 1000);
                    set_pass_move_bonus(clamped);
                    eprintln!("info string PassMoveBonus: {clamped}");
                }
            }
            "PassRightValueEarly" => {
                if let Ok(v) = value.parse::<i32>() {
                    self.pass_right_value_early = v.clamp(0, 500);
                    set_pass_right_value_phased(
                        self.pass_right_value_early,
                        self.pass_right_value_late,
                    );
                    eprintln!("info string PassRightValueEarly: {}", self.pass_right_value_early);
                }
            }
            "PassRightValueLate" => {
                if let Ok(v) = value.parse::<i32>() {
                    self.pass_right_value_late = v.clamp(0, 500);
                    set_pass_right_value_phased(
                        self.pass_right_value_early,
                        self.pass_right_value_late,
                    );
                    eprintln!("info string PassRightValueLate: {}", self.pass_right_value_late);
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
    ///
    /// 拡張形式: `position [sfen <sfen> | startpos] [passrights <black> <white>] [moves <move1> ...]`
    fn cmd_position(&mut self, tokens: &[&str]) {
        Self::apply_position_tokens(
            &mut self.position,
            tokens,
            self.pass_rights_enabled,
            self.initial_pass_count,
        );
    }

    fn apply_position_tokens(
        position: &mut Position,
        tokens: &[&str],
        pass_rights_enabled: bool,
        initial_pass_count: u8,
    ) {
        // position [sfen <sfen> | startpos] [passrights <black> <white>] [moves <move1> <move2> ...]
        let mut idx = 1;
        if idx >= tokens.len() {
            return;
        }

        // 局面の設定
        if tokens[idx] == "startpos" {
            position.set_hirate();
            idx += 1;
        } else if tokens[idx] == "sfen" {
            idx += 1;
            // SFENを収集（movesまたはpassrightsの前まで）
            let mut sfen_parts = Vec::new();
            while idx < tokens.len() && tokens[idx] != "moves" && tokens[idx] != "passrights" {
                sfen_parts.push(tokens[idx]);
                idx += 1;
            }
            let sfen = sfen_parts.join(" ");
            if let Err(e) = position.set_sfen(&sfen) {
                eprintln!("info string Error parsing SFEN: {e}");
                return;
            }
        }

        // パス権の設定（passrights キーワード）
        // 形式: passrights <black_count> <white_count>
        if idx < tokens.len() && tokens[idx] == "passrights" {
            idx += 1;
            if pass_rights_enabled {
                // 先手のパス権数
                let black_pass = if idx < tokens.len() {
                    tokens[idx].parse::<u8>().unwrap_or(initial_pass_count)
                } else {
                    initial_pass_count
                };
                idx += 1;

                // 後手のパス権数
                let white_pass = if idx < tokens.len() {
                    tokens[idx].parse::<u8>().unwrap_or(initial_pass_count)
                } else {
                    initial_pass_count
                };
                idx += 1;

                // パス権を設定
                position.enable_pass_rights(black_pass, white_pass);
            } else {
                // パス権が無効な場合は値を読み飛ばす
                idx += 2;
            }
        } else if pass_rights_enabled {
            // passrights キーワードがないがパス権が有効な場合、デフォルト値を設定
            position.enable_pass_rights(initial_pass_count, initial_pass_count);
        }

        // 指し手の適用
        if idx < tokens.len() && tokens[idx] == "moves" {
            idx += 1;
            while idx < tokens.len() {
                if let Some(mv) = Move::from_usi(tokens[idx]) {
                    // PASS の場合は gives_check は false
                    let gives_check = if mv.is_pass() {
                        false
                    } else {
                        position.gives_check(mv)
                    };
                    position.do_move(mv, gives_check);
                } else {
                    eprintln!("info string Error parsing move: {token}", token = tokens[idx]);
                    break;
                }
                idx += 1;
            }
        }
    }

    fn stochastic_ponder_position(&self) -> Option<Position> {
        let line = self.last_position_cmd.as_deref()?;
        let mut owned: Vec<&str> = line.split_whitespace().collect();
        if owned.len() < 3 {
            return None;
        }

        if let Some(moves_idx) = owned.iter().position(|token| *token == "moves") {
            if owned.len() > moves_idx + 1 {
                owned.pop();
            }
        } else {
            return None;
        }

        let mut position = Position::new();
        Self::apply_position_tokens(
            &mut position,
            &owned,
            self.pass_rights_enabled,
            self.initial_pass_count,
        );
        Some(position)
    }

    /// goコマンド: 探索開始
    fn cmd_go(&mut self, tokens: &[&str]) {
        // 既存の探索を停止（bestmove出力を抑制する）
        // GUIがstopを送らずにposition+goを送ってきた場合、前のponder探索の
        // bestmoveがstdoutに出力されるとGUIが混乱する（YaneuraOu準拠）
        self.stop_search_silently();

        // 制限を解析
        let limits = self.parse_go_options(tokens);

        // Stochastic_Ponder では 1 手戻した局面から先読みする（YaneuraOu 準拠）
        let mut pos = if self.stochastic_ponder && limits.ponder {
            self.stochastic_ponder_position().unwrap_or_else(|| self.position.clone())
        } else {
            self.position.clone()
        };

        let mut search = self
            .search
            .take()
            .unwrap_or_else(|| Search::new_with_eval_hash(self.tt_size_mb, self.eval_hash_size_mb));
        if search.eval_hash_size_mb() != self.eval_hash_size_mb {
            search.resize_eval_hash(self.eval_hash_size_mb);
        }
        search.set_skill_options(self.skill_options);
        // stop/ponderhitフラグをリセット（スレッド生成前に行い、go()内での競合を防ぐ）
        search.reset_flags();
        let stop_flag = search.stop_flag();
        let ponderhit_flag = search.ponderhit_flag();
        self.stop_flag = Some(stop_flag.clone());
        self.ponderhit_flag = Some(ponderhit_flag.clone());

        let suppress_flag = Arc::clone(&self.suppress_bestmove);
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

                    // 探索統計レポートを出力（search-stats feature有効時のみ内容あり）
                    if !result.stats_report.is_empty() {
                        for line in result.stats_report.lines() {
                            println!("info string {line}");
                        }
                        std::io::stdout().flush().ok();
                    }

                    // bestmove出力（suppress_bestmoveが立っていない場合のみ）
                    // cmd_goから内部的にstopされた場合は抑制される
                    if !suppress_flag.load(Ordering::SeqCst) {
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
                    }

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

    /// stopコマンド: 探索停止（GUIからの明示的stop — bestmoveは探索スレッドが出力）
    fn cmd_stop(&mut self) {
        if let Some(stop_flag) = &self.stop_flag {
            stop_flag.store(true, Ordering::SeqCst);
        }
        self.wait_for_search();
    }

    /// 探索を停止するがbestmoveを出力しない（cmd_go内部で使用）
    ///
    /// GUIがstopを送らずにposition+goを送ってきた場合、前のponder探索の
    /// bestmoveを出力するとGUIが混乱する（YaneuraOu準拠）
    fn stop_search_silently(&mut self) {
        self.suppress_bestmove.store(true, Ordering::SeqCst);
        if let Some(stop_flag) = &self.stop_flag {
            stop_flag.store(true, Ordering::SeqCst);
        }
        self.wait_for_search();
        self.suppress_bestmove.store(false, Ordering::SeqCst);
    }

    /// ponderhitコマンド: 先読みヒットを通知
    fn cmd_ponderhit(&mut self) {
        if self.stochastic_ponder {
            self.restart_after_ponderhit();
            return;
        }

        if let Some(flag) = &self.ponderhit_flag {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Stochastic_Ponder の ponderhit 後に通常探索へ切り替える
    fn restart_after_ponderhit(&mut self) {
        self.stop_search_silently();

        if let Some(line) = self.last_position_cmd.clone() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            self.cmd_position(&tokens);
        }

        if let Some(line) = self.last_go_cmd.clone() {
            let owned: Vec<String> = line
                .split_whitespace()
                .filter(|token| *token != "ponder")
                .map(str::to_owned)
                .collect();
            let tokens: Vec<&str> = owned.iter().map(String::as_str).collect();
            if !tokens.is_empty() {
                self.cmd_go(&tokens);
            }
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
                    let mut search =
                        Search::new_with_eval_hash(self.tt_size_mb, self.eval_hash_size_mb);
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

    /// evalコマンド: 現在の局面の静的評価値を表示（デバッグ用）
    ///
    /// `eval diag` で diagnostics 付き評価（PSQT 含む中間値をログ出力）
    fn cmd_eval(&self, diagnostics: bool) {
        let Some(network) = get_network() else {
            println!("info string Error: No NNUE network loaded");
            return;
        };

        // アーキテクチャに応じたアキュムレータスタックを作成
        let mut stack = AccumulatorStackVariant::from_network(&network);

        if diagnostics {
            #[cfg(feature = "diagnostics")]
            {
                use rshogi_core::nnue::{LayerStacksNetwork, NNUENetwork};
                // diagnostics モード: LayerStacks のみ対応
                if let NNUENetwork::LayerStacks(ref net) = *network {
                    // L1 variant に dispatch し、refresh + evaluate_with_diagnostics を実行
                    match net {
                        #[cfg(feature = "layerstacks-1536x16x32")]
                        LayerStacksNetwork::L1536x16x32(inner) => {
                            let mut acc = rshogi_core::nnue::AccumulatorLayerStacks::<1536>::new();
                            inner.refresh_accumulator(&self.position, &mut acc);
                            let value = inner.evaluate_with_diagnostics(&self.position, &acc);
                            println!("info string Static eval (diagnostics): {}", value.raw());
                        }
                        #[cfg(feature = "layerstacks-1536x32x32")]
                        LayerStacksNetwork::L1536x32x32(inner) => {
                            let mut acc = rshogi_core::nnue::AccumulatorLayerStacks::<1536>::new();
                            inner.refresh_accumulator(&self.position, &mut acc);
                            let value = inner.evaluate_with_diagnostics(&self.position, &acc);
                            println!("info string Static eval (diagnostics): {}", value.raw());
                        }
                        #[cfg(feature = "layerstacks-768x16x32")]
                        LayerStacksNetwork::L768x16x32(inner) => {
                            let mut acc = rshogi_core::nnue::AccumulatorLayerStacks::<768>::new();
                            inner.refresh_accumulator(&self.position, &mut acc);
                            let value = inner.evaluate_with_diagnostics(&self.position, &acc);
                            println!("info string Static eval (diagnostics): {}", value.raw());
                        }
                        #[cfg(feature = "layerstacks-512x16x32")]
                        LayerStacksNetwork::L512x16x32(inner) => {
                            let mut acc = rshogi_core::nnue::AccumulatorLayerStacks::<512>::new();
                            inner.refresh_accumulator(&self.position, &mut acc);
                            let value = inner.evaluate_with_diagnostics(&self.position, &acc);
                            println!("info string Static eval (diagnostics): {}", value.raw());
                        }
                        #[allow(unreachable_patterns)]
                        _ => println!(
                            "info string Error: no matching LayerStacks L1 variant compiled"
                        ),
                    }
                } else {
                    println!("info string Error: diagnostics is only supported for LayerStacks");
                }
            }
            #[cfg(not(feature = "diagnostics"))]
            {
                println!("info string Error: build with --features diagnostics to use 'eval diag'");
            }
        } else {
            let value = evaluate_dispatch(&self.position, &mut stack, &mut None);
            println!("info string Static eval: {}", value.raw());
        }
        println!("info string SFEN: {}", self.position.to_sfen());
    }
}

fn main() -> Result<()> {
    // ロガー初期化（標準エラー出力）
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    // ビットボードテーブルの初期化（ホットパスでの OnceLock atomic check 回避）
    rshogi_core::bitboard::init_bitboard_tables();

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
    use serial_test::serial;

    // 履歴統計の初期化がスタックを大量に消費するため、別スレッドで実行
    // UsiEngine::new() が NNUE グローバル状態に依存するため、全テストを #[serial] で逐次実行
    const STACK_SIZE: usize = 64 * 1024 * 1024;

    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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

    #[test]
    #[serial]
    fn stochastic_ponder_position_rewinds_last_move() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut engine = UsiEngine::new();
                engine.last_position_cmd = Some("position startpos moves 7g7f 3c3d".to_string());

                let pos = engine.stochastic_ponder_position().expect("stochastic ponder position");
                assert_eq!(
                    pos.to_sfen(),
                    "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2"
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[serial]
    fn setoption_draw_value_updates_search() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut engine = UsiEngine::new();
                engine.cmd_setoption(&["setoption", "name", "DrawValueBlack", "value", "123"]);
                engine.cmd_setoption(&["setoption", "name", "DrawValueWhite", "value", "-456"]);

                let search = engine.search.as_ref().expect("search exists");
                assert_eq!(search.draw_value_black(), 123);
                assert_eq!(search.draw_value_white(), -456);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[serial]
    fn setoption_layerstack_bucket_updates_globals() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                use rshogi_core::nnue::{
                    LayerStackBucketMode, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
                    get_layer_stack_bucket_mode, get_layer_stack_progress_kpabs_weights,
                    reset_layer_stack_progress_kpabs_weights, set_layer_stack_bucket_mode,
                };

                // テスト開始時に既定値へ戻す
                set_layer_stack_bucket_mode(LayerStackBucketMode::Progress8KPAbs);
                reset_layer_stack_progress_kpabs_weights();

                let mut engine = UsiEngine::new();
                engine.cmd_setoption(&[
                    "setoption",
                    "name",
                    "LS_BUCKET_MODE",
                    "value",
                    "progress8kpabs",
                ]);
                assert_eq!(get_layer_stack_bucket_mode(), LayerStackBucketMode::Progress8KPAbs);

                let tmp_path_bin =
                    std::env::temp_dir().join("rshogi_progress_coeff_kpabs_test.bin");
                let mut bytes = Vec::with_capacity(
                    SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * std::mem::size_of::<f64>(),
                );
                for i in 0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS {
                    let value = if i == 0 {
                        1.25f64
                    } else if i == SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS - 1 {
                        -0.75f64
                    } else {
                        0.0f64
                    };
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                std::fs::write(&tmp_path_bin, bytes).unwrap();
                engine.cmd_setoption(&[
                    "setoption",
                    "name",
                    "LS_PROGRESS_COEFF",
                    "value",
                    tmp_path_bin.to_str().unwrap(),
                ]);
                let kpabs = get_layer_stack_progress_kpabs_weights();
                assert_eq!(kpabs.len(), SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS);
                assert_eq!(kpabs[0], 1.25);
                assert_eq!(kpabs[SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS - 1], -0.75);
                let _ = std::fs::remove_file(tmp_path_bin);

                // 他テストへの影響を避けるため復元
                set_layer_stack_bucket_mode(LayerStackBucketMode::Progress8KPAbs);
                reset_layer_stack_progress_kpabs_weights();
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
