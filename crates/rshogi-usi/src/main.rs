//! USIプロトコルエンジン
//!
//! 将棋GUIとの通信を行うUSIプロトコル実装。

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use rshogi_core::bitboard::Bitboard;
use rshogi_core::eval::{
    DEFAULT_PASS_RIGHT_VALUE_EARLY, DEFAULT_PASS_RIGHT_VALUE_LATE, MaterialLevel, disable_material,
    is_material_enabled, set_eval_hash_enabled, set_material_level, set_pass_move_bonus,
    set_pass_right_value_phased,
};
use rshogi_core::mate::dfpn::{DfpnSolver, MateLimits, MateResult};
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::nnue::{
    AccumulatorStackVariant, LayerStackBucketMode, MAX_LAYER_STACK_BUCKETS,
    NET_DELTA_OPTION_PREFIX, NetCoefficientId, NetDelta, NetDeltaReport, clear_nnue,
    configure_layer_stack_routing, evaluate_dispatch, get_network, init_nnue_with_deltas,
    layer_stack_progress_coeff_required, load_progress_coeff_kpabs_from_bytes,
    parse_layer_stack_bucket_mode, parse_nnue_architecture, print_nnue_stats,
    reset_layer_stack_progress_buckets, reset_layer_stack_progress_kpabs_weights,
    set_fv_scale_override, set_layer_stack_progress_kpabs_weights, set_nnue_architecture_override,
    validate_layer_stack_routing_configuration,
};
use rshogi_core::position::Position;
use rshogi_core::search::{
    DEFAULT_DRAW_VALUE_BLACK, DEFAULT_DRAW_VALUE_WHITE, LimitsType, PonderhitHandle, Search,
    SearchInfo, SearchResult, SearchTuneParams,
};
use rshogi_core::types::{Color, EnteringKingRule, Move, PieceType};
use serde_json::json;

#[cfg(feature = "allocation-stats")]
mod allocation_stats;

#[cfg(all(feature = "mimalloc", feature = "allocation-stats"))]
compile_error!(
    "feature `mimalloc` と `allocation-stats` はどちらも global allocator を設定するため同時に指定できません"
);

#[cfg(all(feature = "mimalloc", not(feature = "allocation-stats")))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// エンジン名
const ENGINE_NAME: &str = "Shogi Engine";
/// エンジンバージョン
const ENGINE_VERSION: &str = "0.1.0";
/// エンジン作者
const ENGINE_AUTHOR: &str = "sh11235";
/// 探索スレッド用のスタックサイズ（SearchWorkerが大きいため増やす）
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;
/// `go mate` の置換表サイズの既定値 (MB)
const DEFAULT_MATE_HASH_MB: usize = 64;

/// USI へ表示するハッシュ表の page 配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TtPageStatus {
    /// Windows の明示的な Large Pages 確保に成功
    LargePages,
    /// Linux/Android の huge-page hint 要求に成功
    HugePageHint,
    /// 通常 page
    Regular,
}

/// 直前に表示した配置から変わったときだけ、表示する文言を返す。
/// 起動直後の通常 page は表示しない。表を取り直して通常 page へ戻った場合は、
/// 以前の表示が残らないよう戻ったことを知らせる。
fn page_status_message(reported: TtPageStatus, current: TtPageStatus) -> Option<&'static str> {
    if reported == current {
        return None;
    }
    Some(match current {
        TtPageStatus::LargePages => "Large Pages are used.",
        TtPageStatus::HugePageHint => {
            "Huge-page hint requested; actual page backing is managed by the OS."
        }
        TtPageStatus::Regular => "The TT now uses regular pages.",
    })
}

/// 探索 thread が panic した場合も探索中状態を解除する。
struct SearchRunningGuard(Arc<AtomicBool>);

impl Drop for SearchRunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
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
    /// EvalHash の確保時に Large Pages を試みるか。
    eval_hash_large_pages: bool,
    /// MultiPV値
    multi_pv: usize,
    /// Skill Level オプション
    skill_options: rshogi_core::search::SkillOptions,
    /// 探索スレッドのハンドル
    search_thread: Option<thread::JoinHandle<(Search, SearchResult)>>,
    /// bestmove 出力前までの探索中状態（thread の終了とは区別する）。
    search_running: Arc<AtomicBool>,
    /// 探索停止用のフラグ（探索スレッドと共有）
    stop_flag: Option<Arc<AtomicBool>>,
    /// ponderhit通知ハンドル
    ponderhit_handle: Option<PonderhitHandle>,
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
    /// LayerStacks の bucket routing mode。LayerStacks 利用時は明示指定が必須。
    ls_bucket_mode: Option<LayerStackBucketMode>,
    /// progresskpabs の推論 bucket 数。0 は未指定として扱う。
    ls_progress_buckets: Option<usize>,
    /// LS_PROGRESS_COEFF の読み込みに成功しているか。
    ls_progress_coeff_loaded: bool,
    /// SPSAParamsFile の明示指定パス（setoption で設定）
    spsa_params_file: Option<String>,
    /// SPSA params ファイルの読み込み済みフラグ
    spsa_params_loaded: bool,
    /// 起動時 spec から広告する net delta option 名。
    spsa_net_spec_names: Vec<String>,
    /// 現在設定されている net delta。
    net_deltas: BTreeMap<NetCoefficientId, i32>,
    /// ロード済み net へ未反映の delta があるか。
    net_deltas_dirty: bool,
    /// 最後に表示した TT の page 配置。未表示のときは表示が不要な Regular。
    reported_page_status: TtPageStatus,
    /// 最後に表示した EvalHash の page 配置。
    reported_eval_hash_page_status: TtPageStatus,
    // --- 有限パス権（Finite Pass Rights）関連 ---
    /// パス権ルール有効化フラグ
    pass_rights_enabled: bool,
    /// 初期パス権数（デフォルト2）
    initial_pass_count: u8,
    /// パス権評価値（序盤）
    pass_right_value_early: i32,
    /// パス権評価値（終盤）
    pass_right_value_late: i32,
    // --- 定跡（opening book）関連 ---
    /// probe パイプラインのオプション群（USI オプションのミラー）
    book_options: rshogi_book::BookOptions,
    /// BookFile（定跡ファイル名。`no_book` で無効）
    book_file: String,
    /// BookExploreFile（空なら無効。変更後の isready でロード）
    book_explore_file: String,
    book_explore: Option<rshogi_book::BookExploreList>,
    /// BookDir（定跡ファイルのディレクトリ）
    book_dir: String,
    /// IgnoreBookPly（末尾手数を無視して検索するか。定跡ロード時のキー正規化に使う）
    ignore_book_ply: bool,
    /// ロード済み定跡（isready 時にロード。BookFile=no_book なら None）
    book: Option<rshogi_book::Book>,
    /// ロード済み定跡の識別子 `(解決済みパス, ignore_book_ply)`。再ロード要否の判定に使う
    book_loaded_sig: Option<(String, bool)>,
    /// 定跡手抽選用の乱数源
    book_rng: rshogi_book::DefaultBookRng,
    // --- 詰み探索（go mate）関連 ---
    /// df-pn の置換表サイズ（MateHashMBで変更）
    mate_hash_mb: usize,
    /// df-pn ソルバー（置換表を持ち回る。最初の go mate で確保）
    mate_solver: Option<DfpnSolver>,
    /// go mate のスレッド（終了時にソルバーを返す。panic したら None）
    mate_thread: Option<thread::JoinHandle<Option<DfpnSolver>>>,
    /// go mate の停止フラグ
    mate_stop: Arc<AtomicBool>,
}

impl UsiEngine {
    /// 新しいUSIエンジンを作成
    #[cfg(test)]
    fn new() -> Self {
        Self::new_with_spsa_net_spec(Vec::new())
    }

    fn new_with_spsa_net_spec(spsa_net_spec_names: Vec<String>) -> Self {
        let tt_size_mb = 256;
        let eval_hash_size_mb = 256;
        let use_eval_hash = true;

        // グローバルフラグをデフォルト値で初期化
        // （USI GUIがsetoptionを送らない場合に備える）
        set_eval_hash_enabled(use_eval_hash);
        reset_layer_stack_progress_buckets();

        Self {
            // EvalHash は最初の `go` 直前まで遅延確保する。
            // selfplay のように起動直後に setoption でサイズを下げるケースで、
            // 先に既定 256MB を確保してしまう無駄を避ける。
            search: Some(Search::new_with_eval_hash_large_pages(tt_size_mb, 0, true)),
            position: Position::new(),
            tt_size_mb,
            eval_hash_size_mb,
            use_eval_hash,
            eval_hash_large_pages: true,
            multi_pv: 1,
            skill_options: rshogi_core::search::SkillOptions::default(),
            search_thread: None,
            search_running: Arc::new(AtomicBool::new(false)),
            stop_flag: None,
            ponderhit_handle: None,
            suppress_bestmove: Arc::new(AtomicBool::new(false)),
            stochastic_ponder: false,
            last_position_cmd: None,
            last_go_cmd: None,
            eval_file_explicit: None,
            eval_file_path: None,
            ls_bucket_mode: None,
            ls_progress_buckets: None,
            ls_progress_coeff_loaded: false,
            spsa_params_file: None,
            spsa_params_loaded: false,
            spsa_net_spec_names,
            net_deltas: BTreeMap::new(),
            net_deltas_dirty: false,
            reported_page_status: TtPageStatus::Regular,
            reported_eval_hash_page_status: TtPageStatus::Regular,
            pass_rights_enabled: false,
            initial_pass_count: 2,
            pass_right_value_early: DEFAULT_PASS_RIGHT_VALUE_EARLY,
            pass_right_value_late: DEFAULT_PASS_RIGHT_VALUE_LATE,
            book_options: rshogi_book::BookOptions::default(),
            // BookFile 既定は no_book(定跡オフ)。既存 SPRT/tournament の挙動を変えない。
            book_file: "no_book".to_string(),
            book_explore_file: String::new(),
            book_explore: None,
            book_dir: "book".to_string(),
            ignore_book_ply: false,
            book: None,
            book_loaded_sig: None,
            book_rng: rshogi_book::DefaultBookRng::new(),
            mate_hash_mb: DEFAULT_MATE_HASH_MB,
            mate_solver: None,
            mate_thread: None,
            mate_stop: Arc::new(AtomicBool::new(false)),
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
                self.cmd_isready()?;
            }
            "setoption" => {
                self.cmd_setoption(&tokens);
            }
            "usinewgame" => {
                self.cmd_usinewgame()?;
            }
            "position" => match self.cmd_position(&tokens) {
                Ok(()) => self.last_position_cmd = Some(line.to_string()),
                Err(error) => eprintln!("info string Error parsing position: {error:#}"),
            },
            "go" => {
                self.last_go_cmd = Some(line.to_string());
                self.cmd_go(&tokens)?;
            }
            "stop" => {
                self.cmd_stop();
            }
            "ponderhit" => {
                self.cmd_ponderhit()?;
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
        println!("option name MateHashMB type spin default {DEFAULT_MATE_HASH_MB} min 1 max 4096");
        println!("option name UseEvalHash type check default true");
        println!("option name EvalHashLargePages type check default true");
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
            "option name LS_BUCKET_MODE type combo default unset var unset var progresskpabs var progresskpabsq16 var kingrank9"
        );
        println!(
            "option name LS_PROGRESS_BUCKETS type spin default 0 min 0 max {MAX_LAYER_STACK_BUCKETS}"
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
        // 定跡（opening book）オプション。既定は BookFile=no_book(オフ)、
        // BookDepthLimit=0(無効)。
        println!("option name USI_OwnBook type check default true");
        println!("option name BookFile type string default no_book");
        println!("option name BookExploreFile type string default <empty>");
        println!("option name BookDir type string default book");
        println!("option name BookMoves type spin default 16 min 0 max 10000");
        println!("option name BookEvalDiff type spin default 30 min 0 max 30000");
        println!("option name BookEvalBlackLimit type spin default 0 min -30000 max 30000");
        println!("option name BookEvalWhiteLimit type spin default -140 min -30000 max 30000");
        println!("option name BookDepthLimit type spin default 0 min 0 max 256");
        println!("option name NarrowBook type check default false");
        println!("option name BookSelectValue type check default false");
        println!("option name ConsiderBookMoveCount type check default false");
        println!("option name IgnoreBookPly type check default false");
        println!("option name FlippedBook type check default true");
        for spec in SearchTuneParams::option_specs() {
            println!(
                "option name {} type spin default {} min {} max {}",
                spec.usi_name, spec.default, spec.min, spec.max
            );
        }
        for name in &self.spsa_net_spec_names {
            println!("option name {name} type spin default 0 min -32768 max 32767");
        }
        println!("usiok");
    }

    /// isreadyコマンド: 準備完了を通知
    /// 探索中は探索を止めず即座に応答し、待機中だけTTをクリアする。
    /// setoption 後は必ず isready が来るので、評価設定変更で陳腐化した EvalHash もここで捨てる。
    fn cmd_isready(&mut self) -> Result<()> {
        if self.search_thread.is_some() && self.search_running.load(Ordering::SeqCst) {
            println!("readyok");
            return Ok(());
        }
        self.wait_for_search();
        if let Some(search) = self.search.as_mut() {
            search.clear_tt();
            search.clear_eval_hash();
        }
        self.maybe_load_spsa_params();
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
                    match self.load_nnue_with_current_deltas(Path::new(DEFAULT_EVAL_FILE)) {
                        Ok(_) => {
                            self.eval_file_path = Some(DEFAULT_EVAL_FILE.to_string());
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
        self.reload_net_deltas_if_dirty()?;
        // version は binary layout の判別にだけ使う。LayerStacks の routing semantics は
        // USI オプションと、ロード済み net の格納 bucket 数を突き合わせて検証する。
        if let Some(stored_bucket_count) =
            get_network().as_deref().and_then(|net| net.layer_stack_num_buckets())
        {
            if let Err(message) = validate_layer_stack_routing(
                stored_bucket_count,
                self.ls_bucket_mode,
                self.ls_progress_buckets,
                self.ls_progress_coeff_loaded,
            ) {
                panic!("Invalid LayerStacks routing configuration: {message}");
            }
            let mode = self.ls_bucket_mode.expect("validated above");
            configure_layer_stack_routing(mode, stored_bucket_count, self.ls_progress_buckets)
                .unwrap_or_else(|message| {
                    panic!("Invalid LayerStacks routing configuration: {message}")
                });
            let routing_bucket_count = match mode {
                LayerStackBucketMode::KingRank9 => 9,
                LayerStackBucketMode::ProgressKPAbs | LayerStackBucketMode::ProgressKPAbsQ16 => {
                    self.ls_progress_buckets.expect("validated above")
                }
                _ => unreachable!("validated above"),
            };
            eprintln!(
                "info string NNUE LayerStack routing mode={} stored_buckets={} routing_buckets={}",
                mode.as_str(),
                stored_bucket_count,
                routing_bucket_count
            );
        }
        self.maybe_report_page_status();
        self.maybe_load_book();
        self.maybe_load_book_explore();
        println!("readyok");
        Ok(())
    }

    /// explore リストを設定変更後の isready で読み込む。読み込み失敗時は未ロードのままとし、次の isready で再試行する。
    fn maybe_load_book_explore(&mut self) {
        if self.book_explore_file.is_empty() || self.book_explore.is_some() {
            return;
        }
        match std::fs::read_to_string(&self.book_explore_file) {
            Ok(text) => {
                self.book_explore = Some(rshogi_book::BookExploreList::parse(&text, |msg| {
                    println!("info string {msg}");
                }));
            }
            Err(e) => {
                println!(
                    "info string BookExploreFile {}: {e}; will retry on next isready",
                    self.book_explore_file
                );
                println!("info string book explore loaded: 0 entries");
                self.book_explore = None;
            }
        }
    }

    /// 定跡ファイルのロード（isready 時に実施）。
    ///
    /// BookFile=no_book なら何もしない。ロード失敗は info string でエラー出力し、
    /// 定跡なし（bookless）で継続する。同一パス・同一 IgnoreBookPly なら再ロードしない。
    fn maybe_load_book(&mut self) {
        if self.book_file == "no_book" || self.book_file.is_empty() {
            self.book = None;
            self.book_loaded_sig = None;
            return;
        }

        // BookDir 配下に解決（BookFile が絶対パス/明示パスならそちらが優先される）。
        let resolved = std::path::Path::new(&self.book_dir).join(&self.book_file);
        let resolved_str = resolved.to_string_lossy().into_owned();
        let sig = (resolved_str.clone(), self.ignore_book_ply);

        // 既ロード & 設定不変なら再ロード不要。
        if self.book.is_some() && self.book_loaded_sig.as_ref() == Some(&sig) {
            return;
        }

        match rshogi_book::Book::from_path(&resolved, self.ignore_book_ply) {
            Ok(book) => {
                println!("info string book loaded: {resolved_str} ({} positions)", book.len());
                self.book = Some(book);
                self.book_loaded_sig = Some(sig);
            }
            Err(e) => {
                println!("info string Error loading book file {resolved_str}: {e}");
                self.book = None;
                self.book_loaded_sig = None;
            }
        }
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

            if name.starts_with(NET_DELTA_OPTION_PREFIX) {
                if self.set_net_delta_value(name, parsed) {
                    applied += 1;
                }
            } else if let Some(search) = self.search.as_mut()
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

    fn set_net_delta_value(&mut self, name: &str, value: i32) -> bool {
        let Some(id) = NetCoefficientId::parse_usi_name(name) else {
            eprintln!("info string Warning: invalid SPSA net option name '{name}'");
            return false;
        };
        let previous = self.net_deltas.get(&id).copied().unwrap_or(0);
        if previous != value {
            self.net_deltas.insert(id, value);
            self.net_deltas_dirty = true;
        }
        true
    }

    fn current_net_deltas(&self) -> Vec<NetDelta> {
        self.net_deltas
            .iter()
            .filter(|(_, delta)| **delta != 0)
            .map(|(id, delta)| NetDelta {
                id: id.clone(),
                delta: *delta,
            })
            .collect()
    }

    fn load_nnue_with_current_deltas(&mut self, path: &Path) -> io::Result<NetDeltaReport> {
        let deltas = self.current_net_deltas();
        let report = init_nnue_with_deltas(path, &deltas)?;
        if !deltas.is_empty() || self.net_deltas_dirty {
            eprintln!(
                "info string net deltas applied: {} coefficients (clamped: {})",
                report.applied, report.clamped
            );
        }
        self.net_deltas_dirty = false;
        Ok(report)
    }

    fn reload_net_deltas_if_dirty(&mut self) -> Result<()> {
        if !self.net_deltas_dirty || get_network().is_none() {
            return Ok(());
        }
        // 探索 worker は global Arc の生存を前提に raw pointer を持つため、差し替え前に join する。
        self.wait_for_search();
        let path = self
            .eval_file_path
            .as_ref()
            .map(PathBuf::from)
            .context("NNUE delta reload requested, but the loaded EvalFile path is unknown")?;
        self.load_nnue_with_current_deltas(&path)
            .with_context(|| format!("failed to reload NNUE file {}", path.display()))?;
        if let Some(search) = self.search.as_mut() {
            search.clear_tt();
            search.resize_eval_hash(self.eval_hash_size_mb);
        }
        self.maybe_report_page_status();
        Ok(())
    }

    fn maybe_report_page_status(&mut self) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let current = if search.tt_uses_large_pages() {
            TtPageStatus::LargePages
        } else if search.tt_huge_page_hint_requested() {
            TtPageStatus::HugePageHint
        } else {
            TtPageStatus::Regular
        };
        let eval_hash = search.eval_hash();
        self.report_eval_hash_page_status(&eval_hash);
        let Some(message) = page_status_message(self.reported_page_status, current) else {
            return;
        };

        let payload = json!({
            "type": "info",
            "message": message,
        });
        println!("info string {}", payload);
        self.reported_page_status = current;
    }

    fn report_eval_hash_page_status(&mut self, hash: &rshogi_core::eval::EvalHash) {
        if self.eval_hash_size_mb == 0 {
            self.reported_eval_hash_page_status = TtPageStatus::Regular;
            return;
        }
        let current = if hash.uses_large_pages() {
            TtPageStatus::LargePages
        } else if hash.huge_page_hint_requested() {
            TtPageStatus::HugePageHint
        } else {
            TtPageStatus::Regular
        };
        if let Some(message) = page_status_message(self.reported_eval_hash_page_status, current) {
            let message = if current == TtPageStatus::Regular {
                "Regular pages are used."
            } else {
                message
            };
            let payload = json!({"type": "info", "message": format!("EvalHash: {message}")});
            println!("info string {}", payload);
            self.reported_eval_hash_page_status = current;
        }
    }

    /// setoptionコマンド: オプション設定
    fn cmd_setoption(&mut self, tokens: &[&str]) {
        // 探索中の設定変更は避ける
        self.wait_for_search();
        // 未解決の go mate infinite は終わらないので、待つ前に止める (checkmate timeout を出す)
        self.mate_stop.store(true, Ordering::SeqCst);
        self.wait_for_mate();

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
        if name.starts_with(NET_DELTA_OPTION_PREFIX) {
            let parsed = match value.parse::<i32>() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("info string Warning: invalid SPSA net value '{}'", value);
                    return;
                }
            };
            self.set_net_delta_value(&name, parsed);
            return;
        }

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
                    self.maybe_report_page_status();
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
            "MateHashMB" => {
                if let Ok(size) = value.parse::<usize>()
                    && size >= 1
                {
                    self.mate_hash_mb = size;
                    // 次の go mate で新しいサイズで確保し直す
                    self.mate_solver = None;
                }
            }
            "EvalHash" => {
                if let Ok(size) = value.parse::<usize>()
                    && let Some(search) = self.search.as_mut()
                {
                    search.resize_eval_hash(size);
                    self.eval_hash_size_mb = size;
                }
                self.maybe_report_page_status();
            }
            "EvalHashLargePages" => {
                let large_pages = value == "true" || value == "1";
                self.eval_hash_large_pages = large_pages;
                if let Some(search) = self.search.as_mut() {
                    search.set_eval_hash_large_pages(large_pages);
                }
                self.maybe_report_page_status();
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
                    reset_layer_stack_progress_buckets();
                    self.eval_file_explicit = None;
                    self.eval_file_path = None;
                } else {
                    // パス指定: ロード試行し、結果を記録
                    self.eval_file_path = Some(value.to_string());
                    match self.load_nnue_with_current_deltas(Path::new(&value)) {
                        Ok(_) => {
                            // 前 net 向けの routing 数を新 net に引き継がない。次の isready で
                            // 再検証・再設定されるまで LayerStacks 評価は loud に失敗する。
                            reset_layer_stack_progress_buckets();
                            self.eval_file_explicit = Some(true);
                            let payload = json!({
                                "type": "info",
                                "message": format!("NNUE loaded: {value}"),
                            });
                            eprintln!("info string {payload}");
                            // LayerStack ネットなら net に格納された bucket 数を出力
                            // (file/option desync 検知用)。
                            if let Some(net) = get_network().as_deref()
                                && net.is_layer_stacks()
                            {
                                #[cfg(feature = "layerstack-arch")]
                                {
                                    let n = net.as_layer_stacks().num_buckets();
                                    eprintln!(
                                        "info string NNUE LayerStack stored_bucket_count={n}"
                                    );
                                }
                            }
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
                    // auto-load した path を明示指定済みの状態へ昇格させないために限定する。
                    if self.eval_file_explicit.is_some()
                        && let Some(path) = self.eval_file_path.clone()
                    {
                        let was_loaded = get_network().is_some();
                        match self.load_nnue_with_current_deltas(Path::new(&path)) {
                            Ok(_) => {
                                reset_layer_stack_progress_buckets();
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
                        reset_layer_stack_progress_buckets();
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
            "LS_BUCKET_MODE" => {
                if value.eq_ignore_ascii_case("unset") || value.is_empty() {
                    self.ls_bucket_mode = None;
                    eprintln!("info string LS_BUCKET_MODE: unset");
                } else {
                    match parse_layer_stack_bucket_mode(&value) {
                        Some(mode) => {
                            self.ls_bucket_mode = Some(mode);
                            eprintln!("info string LS_BUCKET_MODE: {}", mode.as_str());
                        }
                        None => {
                            // 設定済みの正しい mode を typo や旧名 (progress8kpabs) で
                            // 上書き消去しない。isready の「未設定」エラーは誤診を招く。
                            eprintln!(
                                "info string Warning: invalid LS_BUCKET_MODE '{}' ignored, expected progresskpabs, progresskpabsq16 or kingrank9",
                                value
                            );
                        }
                    }
                }
            }
            "LS_PROGRESS_BUCKETS" => match value.parse::<usize>() {
                Ok(0) => {
                    self.ls_progress_buckets = None;
                    eprintln!("info string LS_PROGRESS_BUCKETS: unset");
                }
                Ok(v @ 1..=MAX_LAYER_STACK_BUCKETS) => {
                    self.ls_progress_buckets = Some(v);
                    eprintln!("info string LS_PROGRESS_BUCKETS: {v}");
                }
                _ => {
                    self.ls_progress_buckets = None;
                    eprintln!(
                        "info string Warning: invalid LS_PROGRESS_BUCKETS '{}', expected 0..={MAX_LAYER_STACK_BUCKETS}",
                        value,
                    );
                }
            },
            "LS_PROGRESS_COEFF" => {
                if value.is_empty() || value == "<empty>" {
                    reset_layer_stack_progress_kpabs_weights();
                    rshogi_core::nnue::reset_layer_stack_progress_kpabs_q16_weights();
                    self.ls_progress_coeff_loaded = false;
                    eprintln!("info string LS_PROGRESS_COEFF: unset");
                } else {
                    self.ls_progress_coeff_loaded = false;
                    reset_layer_stack_progress_kpabs_weights();
                    rshogi_core::nnue::reset_layer_stack_progress_kpabs_q16_weights();
                    match std::fs::read(&value).map_err(|e| e.to_string()).and_then(|bytes| {
                        let weights = load_progress_coeff_kpabs_from_bytes(&bytes)?;
                        let q16 =
                            rshogi_core::nnue::load_progress_coeff_kpabs_q16_from_bytes(&bytes)?;
                        rshogi_core::nnue::set_layer_stack_progress_kpabs_q16_weights(q16)?;
                        Ok(weights)
                    }) {
                        Ok(weights) => match set_layer_stack_progress_kpabs_weights(weights) {
                            Ok(()) => {
                                self.ls_progress_coeff_loaded = true;
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
                #[cfg(not(feature = "search-pass-rules"))]
                if v {
                    eprintln!(
                        "info string PassRights: this build does not evaluate pass rights in search (feature `search-pass-rules` is disabled)"
                    );
                }
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
            // --- 定跡（opening book）オプション ---
            "USI_OwnBook" => {
                self.book_options.own_book = value == "true" || value == "1";
            }
            "BookExploreFile" => {
                let value = if value.is_empty() || value == "<empty>" {
                    String::new()
                } else {
                    value
                };
                if self.book_explore_file != value {
                    self.book_explore_file = value;
                    self.book_explore = None;
                }
            }
            "BookFile" => {
                // 実ロードは isready 時。ここでは名前を保持するだけ。
                self.book_file = if value.is_empty() {
                    "no_book".to_string()
                } else {
                    value
                };
            }
            "BookDir" => {
                self.book_dir = if value.is_empty() {
                    "book".to_string()
                } else {
                    value
                };
            }
            "BookMoves" => {
                if let Ok(v) = value.parse::<i32>() {
                    self.book_options.book_moves = v;
                }
            }
            "BookEvalDiff" => {
                if let Ok(v) = value.parse::<i32>() {
                    self.book_options.eval_diff = v;
                }
            }
            "BookEvalBlackLimit" => {
                if let Ok(v) = value.parse::<i32>() {
                    self.book_options.eval_black_limit = v;
                }
            }
            "BookEvalWhiteLimit" => {
                if let Ok(v) = value.parse::<i32>() {
                    self.book_options.eval_white_limit = v;
                }
            }
            "BookDepthLimit" => {
                if let Ok(v) = value.parse::<i32>() {
                    self.book_options.depth_limit = v;
                }
            }
            "NarrowBook" => {
                self.book_options.narrow_book = value == "true" || value == "1";
            }
            "BookSelectValue" => {
                self.book_options.select_value = value == "true" || value == "1";
            }
            "ConsiderBookMoveCount" => {
                self.book_options.consider_move_count = value == "true" || value == "1";
            }
            "IgnoreBookPly" => {
                self.ignore_book_ply = value == "true" || value == "1";
            }
            "FlippedBook" => {
                self.book_options.flipped_book = value == "true" || value == "1";
            }
            _ => {
                // 未知のオプションは無視
            }
        }
    }

    /// usinewgameコマンド: 新しい対局の開始
    fn cmd_usinewgame(&mut self) -> Result<()> {
        self.cmd_stop();
        self.reload_net_deltas_if_dirty()?;

        if let Some(search) = self.search.as_mut() {
            search.clear_tt();
            search.clear_eval_hash();
            search.clear_histories(); // YaneuraOu準拠：履歴統計もクリア
        }
        self.maybe_report_page_status();
        self.position = Position::new();
        Ok(())
    }

    /// positionコマンド: 局面設定
    ///
    /// 拡張形式: `position [sfen <sfen> | startpos] [passrights <black> <white>] [moves <move1> ...]`
    fn cmd_position(&mut self, tokens: &[&str]) -> Result<()> {
        Self::apply_position_tokens(
            &mut self.position,
            tokens,
            self.pass_rights_enabled,
            self.initial_pass_count,
        )
    }

    fn apply_position_tokens(
        position: &mut Position,
        tokens: &[&str],
        pass_rights_enabled: bool,
        initial_pass_count: u8,
    ) -> Result<()> {
        let mut candidate = Position::new();
        let mut idx = 1;
        match tokens.get(idx).copied() {
            Some("startpos") => {
                candidate.set_hirate();
                idx += 1;
            }
            Some("sfen") => {
                idx += 1;
                let start = idx;
                while idx < tokens.len() && !matches!(tokens[idx], "moves" | "passrights") {
                    idx += 1;
                }
                anyhow::ensure!(
                    (3..=4).contains(&(idx - start)),
                    "SFEN must contain a board, side, hand, and optional ply"
                );
                candidate.set_sfen(&tokens[start..idx].join(" "))?;
                anyhow::ensure!(candidate.game_ply() > 0, "SFEN ply must be positive");
            }
            _ => bail!("Expected startpos or sfen"),
        }
        for color in [Color::Black, Color::White] {
            anyhow::ensure!(
                candidate.pieces(color, PieceType::King).count() <= 1,
                "SFEN must contain at most one king per side"
            );
        }
        Self::clear_checkers_without_king(&mut candidate);

        if idx < tokens.len() && tokens[idx] == "passrights" {
            idx += 1;
            let black_pass = tokens.get(idx).context("Missing black passrights")?.parse::<u8>()?;
            let white_pass =
                tokens.get(idx + 1).context("Missing white passrights")?.parse::<u8>()?;
            anyhow::ensure!(black_pass <= 15 && white_pass <= 15, "passrights must be 0..15");
            idx += 2;
            if pass_rights_enabled {
                candidate.enable_pass_rights(black_pass, white_pass);
            }
        } else if pass_rights_enabled {
            candidate.enable_pass_rights(initial_pass_count, initial_pass_count);
        }

        if idx < tokens.len() {
            anyhow::ensure!(tokens[idx] == "moves", "Expected moves, got {}", tokens[idx]);
            idx += 1;
            for token in &tokens[idx..] {
                let mv = Move::from_usi_strict(token)
                    .with_context(|| format!("Invalid move token: {token}"))?;
                anyhow::ensure!(
                    Self::is_legal_position_move(&candidate, mv),
                    "Illegal move: {token}"
                );
                anyhow::ensure!(candidate.game_ply() < i32::MAX, "ply overflow");
                candidate.do_move_with_missing_kings(mv);
                if mv.is_drop()
                    && mv.drop_piece_type() == PieceType::Pawn
                    && candidate.checkers().contains(mv.to())
                {
                    // 入力局面では既存の王手もあり得るため、歩以外の王手を残す応手は数えない。
                    let mut replies = MoveList::new();
                    generate_legal_all(&candidate, &mut replies);
                    anyhow::ensure!(!replies.is_empty(), "Illegal pawn-drop mate: {token}");
                }
            }
        }
        *position = candidate;
        Ok(())
    }

    fn clear_checkers_without_king(position: &mut Position) {
        // 玉のない側の king_square は仮の升なので、入力検証の王手情報には使わない。
        if position.pieces(position.side_to_move(), PieceType::King).is_empty() {
            position.state_mut().checkers = Bitboard::EMPTY;
        }
    }

    fn is_legal_position_move(position: &Position, mv: Move) -> bool {
        if !position.pseudo_legal(mv) {
            return false;
        }
        if mv.is_pass() || mv.is_drop() {
            return true;
        }

        let us = position.side_to_move();
        let captured = position.piece_on(mv.to());
        if captured.is_some() && captured.piece_type() == PieceType::King {
            return false;
        }
        // 自玉がなければ、仮の玉位置に対する玉移動・pin の制約は適用しない。
        position.pieces(us, PieceType::King).is_empty() || position.is_legal(mv)
    }

    fn stochastic_ponder_position(&self) -> Option<Position> {
        let line = self.last_position_cmd.as_deref()?;
        let mut owned: Vec<&str> = line.split_whitespace().collect();
        if owned.len() < 3 {
            return None;
        }

        {
            let moves_idx = owned.iter().position(|token| *token == "moves")?;
            if owned.len() > moves_idx + 1 {
                owned.pop();
            }
        }

        let mut position = Position::new();
        Self::apply_position_tokens(
            &mut position,
            &owned,
            self.pass_rights_enabled,
            self.initial_pass_count,
        )
        .ok()?;
        Some(position)
    }

    /// goコマンド: 探索開始
    fn cmd_go(&mut self, tokens: &[&str]) -> Result<()> {
        // 既存の探索を停止（bestmove出力を抑制する）
        // GUIがstopを送らずにposition+goを送ってきた場合、前のponder探索の
        // bestmoveがstdoutに出力されるとGUIが混乱する（YaneuraOu準拠）
        self.stop_search_silently();
        if tokens.get(1) == Some(&"mate") {
            self.cmd_go_mate(tokens);
            return Ok(());
        }
        self.reload_net_deltas_if_dirty()?;

        // 制限を解析
        let limits = self.parse_go_options(tokens);

        // 定跡 probe（探索の外・root で 1 回）。
        // go infinite / go ponder では probe しない（Phase 1 の単純化、設計メモ §3）。
        // book hit 時は探索スレッドを起こさず bestmove を直接出力する。
        if !limits.infinite && !limits.ponder && self.try_book_probe() {
            return Ok(());
        }

        // Stochastic_Ponder では 1 手戻した局面から先読みする（YaneuraOu 準拠）
        let mut pos = if self.stochastic_ponder && limits.ponder {
            self.stochastic_ponder_position().unwrap_or_else(|| self.position.clone())
        } else {
            self.position.clone()
        };

        let mut search = self.search.take().unwrap_or_else(|| {
            Search::new_with_eval_hash_large_pages(
                self.tt_size_mb,
                self.eval_hash_size_mb,
                self.eval_hash_large_pages,
            )
        });
        if search.eval_hash_size_mb() != self.eval_hash_size_mb {
            search.resize_eval_hash(self.eval_hash_size_mb);
        }
        self.report_eval_hash_page_status(&search.eval_hash());
        search.set_skill_options(self.skill_options);
        // stop/ponderhitフラグをリセット（スレッド生成前に行い、go()内での競合を防ぐ）
        search.reset_flags();
        let stop_flag = search.stop_flag();
        self.stop_flag = Some(stop_flag.clone());
        self.ponderhit_handle = Some(search.ponderhit_handle());

        let suppress_flag = Arc::clone(&self.suppress_bestmove);
        let running_guard = SearchRunningGuard(Arc::clone(&self.search_running));
        self.search_running.store(true, Ordering::SeqCst);
        let builder = thread::Builder::new().stack_size(SEARCH_STACK_SIZE);
        self.search_thread = Some(
            builder
                .spawn(move || {
                    let running_guard = running_guard;
                    // panic 時は Search の helper 終了後に探索中状態を解除する。
                    let mut search = search;
                    #[cfg(feature = "allocation-stats")]
                    let allocation_before = rshogi_core::allocation_stats::snapshot();
                    #[cfg(feature = "tt-write-stats")]
                    let tt_write_before = search.tt_write_stats();
                    let result = search.go(
                        &mut pos,
                        limits,
                        Some(|info: &SearchInfo| {
                            println!("{}", info.to_usi_string());
                            std::io::stdout().flush().ok();
                        }),
                    );

                    #[cfg(feature = "allocation-stats")]
                    allocation_stats::report(allocation_before);

                    #[cfg(feature = "tt-write-stats")]
                    println!(
                        "info string tt_write_events {}",
                        search.tt_write_stats().since(tt_write_before)
                    );

                    // 探索統計レポートを出力（search-stats feature有効時のみ内容あり）
                    if !result.stats_report.is_empty() {
                        for line in result.stats_report.lines() {
                            println!("info string {line}");
                        }
                        std::io::stdout().flush().ok();
                    }

                    // bestmove を受信した側の isready は待機中の準備を行う。
                    running_guard.0.store(false, Ordering::SeqCst);
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
        Ok(())
    }

    /// go mate コマンド: df-pn 詰み探索を別スレッドで走らせ、`checkmate` で応答する。
    ///
    /// `go mate <ミリ秒>` / `go mate infinite` / `go mate`（infinite と同じ）。手番側が詰ませる
    /// 問題として解き、詰みなら `checkmate <手順>`、不詰なら `checkmate nomate`、時間切れ・stop なら
    /// `checkmate timeout` を返す。局面以前の対局履歴（千日手の履歴）は見ない。
    fn cmd_go_mate(&mut self, tokens: &[&str]) {
        let time_ms = parse_go_mate_time(tokens);
        let mut pos = self.position.clone();
        let mut solver =
            self.mate_solver.take().unwrap_or_else(|| DfpnSolver::new(self.mate_hash_mb));
        self.mate_stop.store(false, Ordering::SeqCst);
        let stop = Arc::clone(&self.mate_stop);
        let suppress_flag = Arc::clone(&self.suppress_bestmove);
        // 詰み探索の再帰は手数に比例して深くなるので、通常探索と同じ大きさのスタックを使う
        let builder = thread::Builder::new().stack_size(SEARCH_STACK_SIZE);
        self.mate_thread = Some(
            builder
                .spawn(move || {
                    let limits = MateLimits { nodes: 0, time_ms };
                    // panic しても GUI は checkmate を待ち続けるので、捕まえて timeout として応答する。
                    // panic = "abort" のビルド (production プロファイル) ではプロセスごと終了する。
                    let solved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let result = solver.solve(&mut pos, &limits, &stop, |progress| {
                            let nps = progress.nodes * 1000 / progress.elapsed_ms.max(1);
                            println!(
                                "info time {} nodes {} nps {nps}",
                                progress.elapsed_ms, progress.nodes
                            );
                            std::io::stdout().flush().ok();
                        });
                        (solver, result)
                    }));
                    let (solver, result) = match solved {
                        Ok((solver, result)) => (Some(solver), result),
                        Err(_) => {
                            eprintln!("info string mate search panicked");
                            (None, MateResult::Unknown)
                        }
                    };
                    if !suppress_flag.load(Ordering::SeqCst) {
                        println!("{}", checkmate_response(&result));
                        std::io::stdout().flush().ok();
                    }
                    solver
                })
                .expect("failed to spawn mate search thread"),
        );
    }

    /// 詰み探索スレッドの終了を待ち、ソルバーを取り戻す（panic していたら次の go mate で作り直す）
    fn wait_for_mate(&mut self) {
        if let Some(handle) = self.mate_thread.take() {
            self.mate_solver = handle.join().ok().flatten();
        }
    }

    /// 現在局面を定跡で probe し、hit したら bestmove を直接出力して true を返す。
    ///
    /// USI_OwnBook=false / 定跡未ロード / miss の場合は何も出力せず false を返し、
    /// 呼び出し側は通常探索へフォールバックする。
    fn try_book_probe(&mut self) -> bool {
        if !self.book_options.own_book {
            return false;
        }
        let Some(book) = self.book.as_ref() else {
            return false;
        };

        // probe 中の info string 本文を集めてから出力する（borrow 競合回避）。
        let mut infos: Vec<String> = Vec::new();
        let result = rshogi_book::probe_with_explore(
            book,
            &self.position,
            &self.book_options,
            &mut self.book_rng,
            self.book_explore.as_mut(),
            |msg| infos.push(msg.to_string()),
        );

        for msg in &infos {
            println!("info string {msg}");
        }

        match result {
            Some(r) => {
                let best_usi = r.best_move.to_usi();
                match r.ponder_move {
                    Some(ponder) => {
                        println!("bestmove {best_usi} ponder {}", ponder.to_usi());
                    }
                    None => {
                        println!("bestmove {best_usi}");
                    }
                }
                std::io::stdout().flush().ok();
                true
            }
            None => false,
        }
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
                            if let Some(normalized) = self.position.to_move(mv) {
                                if normalized.is_pass() && !self.position.can_pass() {
                                    eprintln!("warning: invalid searchmoves: {}", tokens[idx]);
                                } else {
                                    limits.search_moves.push(normalized);
                                }
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
        // go mate 中なら打ち切り、探索スレッドが checkmate timeout（または直前に出た結論）を出力する
        self.mate_stop.store(true, Ordering::SeqCst);
        self.wait_for_mate();
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
        self.mate_stop.store(true, Ordering::SeqCst);
        self.wait_for_mate();
        self.suppress_bestmove.store(false, Ordering::SeqCst);
    }

    /// ponderhitコマンド: 先読みヒットを通知
    fn cmd_ponderhit(&mut self) -> Result<()> {
        if self.stochastic_ponder {
            self.restart_after_ponderhit()?;
            return Ok(());
        }

        if let Some(handle) = &self.ponderhit_handle {
            handle.signal();
        }
        Ok(())
    }

    /// Stochastic_Ponder の ponderhit 後に通常探索へ切り替える
    fn restart_after_ponderhit(&mut self) -> Result<()> {
        self.stop_search_silently();

        if let Some(line) = self.last_position_cmd.clone() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            self.cmd_position(&tokens)?;
        }

        if let Some(line) = self.last_go_cmd.clone() {
            let owned: Vec<String> = line
                .split_whitespace()
                .filter(|token| *token != "ponder")
                .map(str::to_owned)
                .collect();
            let tokens: Vec<&str> = owned.iter().map(String::as_str).collect();
            if !tokens.is_empty() {
                self.cmd_go(&tokens)?;
            }
        }
        Ok(())
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
                    let mut search = Search::new_with_eval_hash_large_pages(
                        self.tt_size_mb,
                        self.eval_hash_size_mb,
                        self.eval_hash_large_pages,
                    );
                    search.set_skill_options(self.skill_options);
                    self.search = Some(search);
                }
            }
        }
        self.stop_flag = None;
        self.ponderhit_handle = None;
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
            #[cfg(all(feature = "diagnostics", feature = "layerstack-arch"))]
            {
                use rshogi_core::nnue::NNUENetwork;
                if let NNUENetwork::LayerStacks(ref net) = *network {
                    let value = net.refresh_and_evaluate_with_diagnostics(&self.position);
                    println!("info string Static eval (diagnostics): {}", value.raw());
                } else {
                    println!("info string Error: diagnostics is only supported for LayerStacks");
                }
            }
            #[cfg(all(feature = "diagnostics", not(feature = "layerstack-arch")))]
            {
                let _ = &network;
                println!(
                    "info string Error: 'eval diag' requires the `layerstack-arch` feature \
                     (LayerStacks diagnostics)"
                );
            }
            #[cfg(not(feature = "diagnostics"))]
            {
                let _ = &network;
                println!("info string Error: build with --features diagnostics to use 'eval diag'");
            }
        } else {
            let value = evaluate_dispatch(&self.position, &mut stack, &mut None);
            println!("info string Static eval: {}", value.raw());
        }
        println!("info string SFEN: {}", self.position.to_sfen());
    }
}

/// `go mate` の制限時間（ミリ秒）。`infinite`・省略・0 は無制限（0 を返す）。
fn parse_go_mate_time(tokens: &[&str]) -> u64 {
    match tokens.get(2) {
        None | Some(&"infinite") => 0,
        Some(value) => value.parse().unwrap_or_else(|_| {
            eprintln!("info string Warning: invalid go mate time '{value}', treated as infinite");
            0
        }),
    }
}

/// 詰み探索の結果を USI の `checkmate` 応答にする
fn checkmate_response(result: &MateResult) -> String {
    match result {
        MateResult::Mate(pv) => {
            let moves: Vec<String> = pv.iter().map(|m| m.to_usi()).collect();
            format!("checkmate {}", moves.join(" "))
        }
        MateResult::NoMate => "checkmate nomate".to_string(),
        MateResult::Unknown => "checkmate timeout".to_string(),
        MateResult::Unsupported => "checkmate notimplemented".to_string(),
    }
}

/// LayerStacks の格納 bucket 数と、明示された routing semantics の整合性を検証する。
///
/// NNUE version は binary layout の判別にだけ使い、この判断には使わない。
fn validate_layer_stack_routing(
    stored_bucket_count: usize,
    mode: Option<LayerStackBucketMode>,
    progress_buckets: Option<usize>,
    progress_coeff_loaded: bool,
) -> std::result::Result<(), String> {
    match mode {
        None => Err(
            "LS_BUCKET_MODE must be explicitly set to progresskpabs, progresskpabsq16 or kingrank9 before isready"
                .to_string(),
        ),
        Some(mode @ (LayerStackBucketMode::ProgressKPAbs | LayerStackBucketMode::ProgressKPAbsQ16)) => {
            validate_layer_stack_routing_configuration(
                mode,
                stored_bucket_count,
                progress_buckets,
            )?;
            if !progress_coeff_loaded && layer_stack_progress_coeff_required(progress_buckets) {
                return Err(format!("LS_BUCKET_MODE={} requires LS_PROGRESS_COEFF to be loaded", mode.as_str()));
            }
            Ok(())
        }
        Some(LayerStackBucketMode::KingRank9) => {
            validate_layer_stack_routing_configuration(
                LayerStackBucketMode::KingRank9,
                stored_bucket_count,
                progress_buckets,
            )?;
            if progress_coeff_loaded {
                return Err(
                    "LS_BUCKET_MODE=kingrank9 conflicts with LS_PROGRESS_COEFF; leave it unset"
                        .to_string(),
                );
            }
            Ok(())
        }
        Some(_) => Err("unsupported LS_BUCKET_MODE".to_string()),
    }
}

fn parse_startup_args(args: impl IntoIterator<Item = String>) -> Result<Option<PathBuf>> {
    let mut spec_path = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--spsa-net-spec" => {
                if spec_path.is_some() {
                    bail!("--spsa-net-spec may only be specified once");
                }
                let path = args.next().context("--spsa-net-spec requires a path")?;
                spec_path = Some(PathBuf::from(path));
            }
            _ => bail!("unknown argument: {argument}"),
        }
    }
    Ok(spec_path)
}

fn parse_spsa_net_spec(content: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for (line_index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let without_comment = trimmed.split("//").next().unwrap_or(trimmed);
        let usable = without_comment.replace("[[NOT USED]]", "");
        let usable = usable.trim();
        if usable.is_empty() {
            continue;
        }
        let name = usable.split(',').next().unwrap_or("").trim();
        if NetCoefficientId::parse_usi_name(name).is_none() {
            bail!("invalid SPSA net spec at line {}: '{name}'", line_index + 1);
        }
        names.push(name.to_string());
    }
    Ok(names)
}

fn main() -> Result<()> {
    #[cfg(feature = "allocation-stats")]
    allocation_stats::init()?;

    // ロガー初期化（標準エラー出力）
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    // ビットボードテーブルの初期化（ホットパスでの OnceLock atomic check 回避）
    rshogi_core::bitboard::init_bitboard_tables();

    let spec_path = parse_startup_args(std::env::args().skip(1))?;
    let spec_names = match spec_path {
        Some(path) => {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read SPSA net spec {}", path.display()))?;
            parse_spsa_net_spec(&content)
                .with_context(|| format!("failed to parse SPSA net spec {}", path.display()))?
        }
        None => Vec::new(),
    };
    let mut engine = UsiEngine::new_with_spsa_net_spec(spec_names);
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

    #[test]
    #[serial]
    fn book_explore_option_load_reload_disable_and_read_failure() {
        let mut engine = UsiEngine::new();
        assert!(engine.book_explore_file.is_empty());
        assert!(engine.book_explore.is_none());
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/book-explore-option-test.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1 7g7f\n",
        )
        .unwrap();
        engine.cmd_setoption(&[
            "setoption",
            "name",
            "BookExploreFile",
            "value",
            path.to_str().unwrap(),
        ]);
        assert!(engine.book_explore.is_none());
        engine.maybe_load_book_explore();
        assert_eq!(engine.book_explore.as_ref().unwrap().len(), 1);
        std::fs::remove_file(&path).unwrap();
        engine.maybe_load_book_explore();
        assert_eq!(engine.book_explore.as_ref().unwrap().len(), 1);
        engine.cmd_setoption(&["setoption", "name", "BookExploreFile", "value", ""]);
        engine.maybe_load_book_explore();
        assert!(engine.book_explore.is_none());
        engine.cmd_setoption(&[
            "setoption",
            "name",
            "BookExploreFile",
            "value",
            path.to_str().unwrap(),
        ]);
        engine.maybe_load_book_explore();
        assert!(engine.book_explore.is_none());
        std::fs::write(&path, "# empty\n").unwrap();
        engine.maybe_load_book_explore();
        assert!(engine.book_explore.as_ref().unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
        engine.cmd_setoption(&["setoption", "name", "BookExploreFile", "value", "<empty>"]);
        assert!(engine.book_explore_file.is_empty());
        assert!(engine.book_explore.is_none());
        engine.maybe_load_book_explore();
        assert!(engine.book_explore.is_none());
    }

    #[test]
    fn page_status_is_reported_only_when_it_changes() {
        use TtPageStatus::{HugePageHint, LargePages, Regular};

        assert_eq!(page_status_message(Regular, Regular), None);
        assert_eq!(page_status_message(LargePages, LargePages), None);
        assert_eq!(page_status_message(Regular, LargePages), Some("Large Pages are used."));
        assert!(page_status_message(Regular, HugePageHint).is_some_and(|m| m.contains("hint")));
        assert!(
            page_status_message(LargePages, Regular).is_some_and(|m| m.contains("regular pages"))
        );
        assert!(
            page_status_message(HugePageHint, Regular).is_some_and(|m| m.contains("regular pages"))
        );
    }

    #[test]
    #[serial]
    fn eval_hash_large_pages_option_survives_resize() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut engine = UsiEngine::new();
                assert!(engine.eval_hash_large_pages);
                assert_eq!(engine.eval_hash_size_mb, 256);
                engine.eval_hash_size_mb = 0;
                engine.reported_eval_hash_page_status = TtPageStatus::LargePages;
                engine.report_eval_hash_page_status(&rshogi_core::eval::EvalHash::new(0));
                assert_eq!(engine.reported_eval_hash_page_status, TtPageStatus::Regular);
                engine.eval_hash_size_mb = 256;
                engine.cmd_setoption(&[
                    "setoption",
                    "name",
                    "EvalHashLargePages",
                    "value",
                    "false",
                ]);
                assert_eq!(engine.search.as_ref().unwrap().eval_hash_size_mb(), 0);
                engine.cmd_setoption(&["setoption", "name", "EvalHash", "value", "1"]);
                let regular = engine.search.as_ref().unwrap().eval_hash();
                assert!(!regular.uses_large_pages());
                assert!(!regular.huge_page_hint_requested());
                regular.store(1, 42);
                engine.cmd_setoption(&["setoption", "name", "EvalHashLargePages", "value", "true"]);
                let allocated = engine.search.as_ref().unwrap().eval_hash();
                assert!(!Arc::ptr_eq(&regular, &allocated));
                assert_eq!(allocated.probe(1), None);
                engine.cmd_setoption(&[
                    "setoption",
                    "name",
                    "EvalHashLargePages",
                    "value",
                    "false",
                ]);
                engine.cmd_setoption(&["setoption", "name", "EvalHash", "value", "3"]);
                assert!(!engine.eval_hash_large_pages);
                let resized = engine.search.as_ref().unwrap().eval_hash();
                assert!(!resized.uses_large_pages());
                assert!(!resized.huge_page_hint_requested());
                resized.store(1, -42);
                assert_eq!(resized.probe(1), Some(-42));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[serial]
    fn explicit_spsa_defaults_preserve_unconfigured_search() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut engine = UsiEngine::new();
                let defaults = engine.search.as_ref().unwrap().search_tune_params();
                assert_eq!(SearchTuneParams::option_specs().len(), 170);
                for spec in SearchTuneParams::option_specs() {
                    let value = spec.default.to_string();
                    engine.cmd_setoption(&["setoption", "name", spec.usi_name, "value", &value]);
                    assert_eq!(
                        engine.search.as_ref().unwrap().search_tune_params(),
                        defaults,
                        "{}",
                        spec.usi_name
                    );
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    // 履歴統計の初期化がスタックを大量に消費するため、別スレッドで実行
    // UsiEngine::new() が NNUE グローバル状態に依存するため、全テストを #[serial] で逐次実行
    const STACK_SIZE: usize = 64 * 1024 * 1024;

    #[test]
    fn spsa_net_spec_accepts_names_and_params_rows() {
        let content = r#"
# comment
SPSA_NET_out_w_b3_17
SPSA_NET_out_b_b0_0,int,0,-10,10,1,0.1 // output bias
SPSA_NET_ft_b_1023,int,0,-10,10,1,0.1 [[NOT USED]]
// ignored
"#;
        assert_eq!(
            parse_spsa_net_spec(content).expect("spec"),
            [
                "SPSA_NET_out_w_b3_17",
                "SPSA_NET_out_b_b0_0",
                "SPSA_NET_ft_b_1023"
            ]
        );
    }

    #[test]
    fn spsa_net_spec_reports_invalid_line_and_name() {
        let error = parse_spsa_net_spec("# ok\nSPSA_NET_out_w_17,int,0").expect_err("invalid name");
        let message = error.to_string();
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("SPSA_NET_out_w_17"), "{message}");
    }

    #[test]
    #[serial]
    fn setoption_net_delta_saves_value_and_tracks_dirty_state() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut engine = UsiEngine::new();
                let name = "SPSA_NET_l2_w_b2_45";
                let id = NetCoefficientId::parse_usi_name(name).expect("id");

                engine.cmd_setoption(&["setoption", "name", name, "value", "12"]);
                assert_eq!(engine.net_deltas.get(&id), Some(&12));
                assert!(engine.net_deltas_dirty);

                engine.net_deltas_dirty = false;
                engine.cmd_setoption(&["setoption", "name", name, "value", "12"]);
                assert!(!engine.net_deltas_dirty);

                engine.cmd_setoption(&["setoption", "name", name, "value", "0"]);
                assert_eq!(engine.net_deltas.get(&id), Some(&0));
                assert!(engine.net_deltas_dirty);

                engine.net_deltas_dirty = false;
                engine.cmd_setoption(&["setoption", "name", "SPSA_NET_out_w_3", "value", "1"]);
                assert!(!engine.net_deltas_dirty);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[serial]
    fn parse_go_searchmoves_special_values() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut engine = UsiEngine::new();
                let tokens = [
                    "go",
                    "searchmoves",
                    "pass",
                    "0000",
                    "win",
                    "5i4i",
                    "depth",
                    "1",
                ];
                for rank1 in ["4k4", "4k3p", "4k3P"] {
                    engine.position.set_sfen(&format!("{rank1}/9/9/9/9/9/9/9/4K4 b - 1")).unwrap();
                    let ordinary =
                        engine.position.to_move(Move::from_usi("5i4i").unwrap()).unwrap();
                    let limits = engine.parse_go_options(&tokens);
                    assert_eq!(limits.search_moves, [ordinary]);
                    engine.position.enable_pass_rights(1, 1);
                    let limits = engine.parse_go_options(&tokens);
                    assert_eq!(limits.search_moves, [Move::PASS, Move::PASS, ordinary]);
                    assert_eq!(limits.depth, 1);
                    let pass_only = engine.parse_go_options(&["go", "searchmoves", "pass"]);
                    let roots = rshogi_core::search::RootMoves::from_legal_moves(
                        &engine.position,
                        &pass_only.search_moves,
                    );
                    assert_eq!(roots.len(), 1);
                    assert!(roots.find(Move::PASS).is_some());
                    engine.position.enable_pass_rights(0, 1);
                    assert_eq!(engine.parse_go_options(&tokens).search_moves, [ordinary]);
                }
                engine.position.set_sfen("4k4/9/9/9/9/9/9/4r4/4K4 b - 1").unwrap();
                engine.position.enable_pass_rights(1, 1);
                assert!(engine.position.in_check());
                assert!(
                    engine
                        .parse_go_options(&["go", "searchmoves", "pass", "0000", "win"])
                        .search_moves
                        .is_empty()
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
    #[test]
    fn parse_go_mate_time_accepts_ms_and_infinite() {
        assert_eq!(parse_go_mate_time(&["go", "mate", "1500"]), 1500);
        assert_eq!(parse_go_mate_time(&["go", "mate"]), 0);
        assert_eq!(parse_go_mate_time(&["go", "mate", "infinite"]), 0);
        assert_eq!(parse_go_mate_time(&["go", "mate", "abc"]), 0);
    }

    #[test]
    fn checkmate_response_formats_each_outcome() {
        let pv = vec![Move::from_usi("G*5b").unwrap()];
        assert_eq!(checkmate_response(&MateResult::Mate(pv)), "checkmate G*5b");
        assert_eq!(checkmate_response(&MateResult::NoMate), "checkmate nomate");
        assert_eq!(checkmate_response(&MateResult::Unknown), "checkmate timeout");
        assert_eq!(checkmate_response(&MateResult::Unsupported), "checkmate notimplemented");
    }

    #[test]
    fn position_commands_preserve_existing_enemy_check() {
        let cases = [
            (
                "4k4/9/9/9/4R4/9/9/9/4K4 b G 1",
                &[("G*1h", [2, 0]), ("5a4a", [2, 0]), ("5e4e", [4, 0])][..],
            ),
            (
                "4k4/9/9/9/4R4/9/9/9/9 b G 1",
                &[("G*1h", [2, 0]), ("5a4a", [2, 0]), ("5e4e", [4, 0])],
            ),
            (
                "4k4/9/9/9/4r4/9/9/9/4K4 w g 1",
                &[("G*1b", [0, 2]), ("5i4i", [0, 2]), ("5e4e", [0, 4])],
            ),
            (
                "4k4/9/9/9/4R4/9/9/9/4K4 b G 1",
                &[("G*1h", [2, 0]), ("5a4a", [2, 0]), ("5e5d", [0, 0])],
            ),
            ("4k4/9/9/9/4R4/9/9/9/4K4 b G 1", &[("G*5c", [0, 0])]),
            (
                "4k4/4p4/9/9/4R4/9/9/9/4K4 b - 1",
                &[("5e5b+", [2, 0]), ("5a5b", [2, 0]), ("P*5e", [0, 0])],
            ),
        ];
        let snapshot = |position: &Position| {
            (
                position.to_sfen(),
                position.key(),
                position.state().checkers,
                position.state().continuous_check,
                position.king_square(Color::Black),
                position.king_square(Color::White),
                position.state().last_move,
            )
        };
        for (sfen, steps) in cases {
            let mut position = Position::new();
            let mut line = format!("position sfen {sfen} moves");
            UsiEngine::apply_position_tokens(
                &mut position,
                &line.split_whitespace().collect::<Vec<_>>(),
                false,
                2,
            )
            .unwrap();
            let mut history = Vec::new();
            for &(token, continuous_check) in steps {
                history.push(snapshot(&position));
                line.push(' ');
                line.push_str(token);
                UsiEngine::apply_position_tokens(
                    &mut position,
                    &line.split_whitespace().collect::<Vec<_>>(),
                    false,
                    2,
                )
                .unwrap_or_else(|err| panic!("{line}: {err}"));
                let mut fresh = Position::new();
                fresh.set_sfen(&position.to_sfen()).unwrap();
                UsiEngine::clear_checkers_without_king(&mut fresh);
                assert_eq!(position.state().checkers, fresh.state().checkers, "{line}");
                assert_eq!(position.key(), fresh.key(), "{line}");
                assert_eq!(position.state().continuous_check, continuous_check, "{line}");
                assert_eq!(
                    position.in_check(),
                    continuous_check[(!position.side_to_move()).index()] != 0,
                    "{line}"
                );
                let before = snapshot(&position);
                let invalid = format!("{line} 1a1b");
                assert!(
                    UsiEngine::apply_position_tokens(
                        &mut position,
                        &invalid.split_whitespace().collect::<Vec<_>>(),
                        false,
                        2,
                    )
                    .is_err(),
                    "{invalid}"
                );
                assert_eq!(snapshot(&position), before, "{invalid}");
            }
            for (&(token, _), before) in steps.iter().zip(history).rev() {
                position.undo_move(Move::from_usi_strict(token).unwrap());
                assert_eq!(snapshot(&position), before, "{sfen}: undo {token}");
            }
        }
    }

    #[test]
    fn position_commands_existing_enemy_check_preserves_repetition() {
        for sfen in [
            "4k4/9/9/9/4R4/9/9/9/4K4 b G 1",
            "4k4/9/9/9/4R4/9/9/9/9 b G 1",
        ] {
            let cycle = "5a4a 5e4e 4a5a 4e5e ";
            let moves = format!("G*1h {}", cycle.repeat(3));
            let line = format!("position sfen {sfen} moves {moves}");
            let mut position = Position::new();
            UsiEngine::apply_position_tokens(
                &mut position,
                &line.split_whitespace().collect::<Vec<_>>(),
                false,
                2,
            )
            .unwrap();
            assert!(position.in_check());
            assert_eq!(position.state().continuous_check, [14, 0]);
            assert_eq!(position.game_ply(), 14);
            assert_eq!(position.repetition_state(16), rshogi_core::types::RepetitionState::Win);
            for token in moves.split_whitespace().rev() {
                position.undo_move(Move::from_usi_strict(token).unwrap());
                let mut fresh = Position::new();
                fresh.set_sfen(&position.to_sfen()).unwrap();
                UsiEngine::clear_checkers_without_king(&mut fresh);
                assert_eq!(position.state().checkers, fresh.state().checkers);
            }
            assert_eq!(position.to_sfen(), sfen);
            assert_eq!(position.state().continuous_check, [0, 0]);
        }
    }

    #[test]
    fn position_commands_reject_pawn_drop_mate_during_existing_check() {
        let mut position = Position::new();
        UsiEngine::apply_position_tokens(
            &mut position,
            &["position", "startpos", "moves", "7g7f", "3c3d"],
            false,
            2,
        )
        .unwrap();
        let before = position.to_sfen();
        let key = position.key();
        for line in [
            "position sfen 3pk3R/5g3/4G4/9/9/9/9/9/4K4 b P 1 moves P*5b",
            "position sfen 3pk3R/5g3/4G4/9/9/9/9/9/9 b P 1 moves P*5b",
            "position sfen 4k4/9/9/9/9/9/4g4/3G5/r3KP3 w p 1 moves P*5h",
            "position sfen 9/9/9/9/9/9/4g4/3G5/r3KP3 w p 1 moves P*5h",
        ] {
            let error = UsiEngine::apply_position_tokens(
                &mut position,
                &line.split_whitespace().collect::<Vec<_>>(),
                false,
                2,
            )
            .unwrap_err();
            assert!(error.to_string().contains("Illegal pawn-drop mate"), "{line}: {error}");
            assert_eq!(position.to_sfen(), before, "{line}");
            assert_eq!(position.key(), key, "{line}");
        }
        position.undo_move(Move::from_usi_strict("3c3d").unwrap());
        position.undo_move(Move::from_usi_strict("7g7f").unwrap());
        assert_eq!(position.to_sfen(), rshogi_core::position::SFEN_HIRATE);
    }

    #[test]
    fn position_commands_accept_pawn_checks_with_legal_replies() {
        for (sfen, moves, checker_count, expected) in [
            (
                "3pk3R/5g3/9/9/9/9/9/9/4K4 b P 1",
                "P*5b 5a5b",
                2,
                "3p4R/4kg3/9/9/9/9/9/9/4K4 b p 3",
            ),
            (
                "3pk3R/9/3G5/9/9/9/9/9/4K4 b P 1",
                "P*5b 5a4b",
                2,
                "3p4R/4Pk3/3G5/9/9/9/9/9/4K4 b - 3",
            ),
            (
                "3pk4/5g3/4G4/9/9/9/9/9/4K4 b P 1",
                "P*5b 4b5b",
                1,
                "3pk4/4g4/4G4/9/9/9/9/9/4K4 b p 3",
            ),
            ("3pk3R/5g3/9/9/9/9/9/9/9 b P 1", "P*5b 5a5b", 2, "3p4R/4kg3/9/9/9/9/9/9/9 b p 3"),
        ] {
            let mut position = Position::new();
            position.set_sfen(sfen).unwrap();
            let root_key = position.key();
            let line = format!("position sfen {sfen} moves {moves}");
            UsiEngine::apply_position_tokens(
                &mut position,
                &line.split_whitespace().collect::<Vec<_>>(),
                false,
                2,
            )
            .unwrap_or_else(|error| panic!("{line}: {error}"));
            assert_eq!(position.to_sfen(), expected, "{line}");
            assert!(!position.in_check(), "{line}");
            let mut parsed_moves =
                moves.split_whitespace().map(|m| Move::from_usi_strict(m).unwrap());
            let pawn_drop = parsed_moves.next().unwrap();
            position.undo_move(parsed_moves.next().unwrap());
            assert_eq!(position.checkers().count(), checker_count, "{line}");
            assert!(position.checkers().contains(pawn_drop.to()), "{line}");
            assert_eq!(position.state().continuous_check, [2, 0], "{line}");
            assert_eq!(position.game_ply(), 2, "{line}");
            let mut fresh = Position::new();
            fresh.set_sfen(&position.to_sfen()).unwrap();
            assert_eq!(position.key(), fresh.key(), "{line}");
            assert_eq!(position.checkers(), fresh.checkers(), "{line}");
            position.undo_move(pawn_drop);
            assert_eq!(position.to_sfen(), sfen, "{line}");
            assert_eq!(position.key(), root_key, "{line}");
            assert_eq!(position.state().continuous_check, [0, 0], "{line}");
        }
    }

    #[test]
    fn position_commands_accept_nonchecking_pawn_drop_with_existing_mate() {
        let sfen = "3pk3R/9/4G4/9/9/9/9/9/4K4 b P 1";
        let line = format!("position sfen {sfen} moves P*1h");
        let mut position = Position::new();
        UsiEngine::apply_position_tokens(
            &mut position,
            &line.split_whitespace().collect::<Vec<_>>(),
            false,
            2,
        )
        .unwrap();
        assert_eq!(position.to_sfen(), "3pk3R/9/4G4/9/9/9/9/8P/4K4 w - 2");
        let pawn_drop = Move::from_usi_strict("P*1h").unwrap();
        assert!(position.in_check());
        assert!(!position.checkers().contains(pawn_drop.to()));
        let mut replies = MoveList::new();
        generate_legal_all(&position, &mut replies);
        assert!(replies.is_empty());
        assert_eq!(position.state().continuous_check, [2, 0]);
        position.undo_move(pawn_drop);
        assert_eq!(position.to_sfen(), sfen);
    }

    #[test]
    fn position_commands_reject_duplicate_kings_atomically() {
        let mut position = Position::new();
        UsiEngine::apply_position_tokens(
            &mut position,
            &["position", "startpos", "moves", "7g7f", "3c3d"],
            false,
            2,
        )
        .unwrap();
        let before = position.to_sfen();
        let key = position.key();
        for line in [
            "position sfen 4K4/r8/9/9/9/9/9/9/4K4 b - 1 moves 5a5b",
            "position sfen 4K4/r8/9/9/9/9/9/9/4K4 b - 1",
            "position sfen 4k4/R8/9/9/9/9/9/9/4k4 w - 1 moves 5a5b",
            "position sfen 4k4/R8/9/9/9/9/9/9/4k4 w - 1",
        ] {
            assert!(
                UsiEngine::apply_position_tokens(
                    &mut position,
                    &line.split_whitespace().collect::<Vec<_>>(),
                    false,
                    2,
                )
                .is_err(),
                "{line}"
            );
            assert_eq!(position.to_sfen(), before, "{line}");
            assert_eq!(position.key(), key, "{line}");
        }
        position.undo_move(Move::from_usi_strict("3c3d").unwrap());
        position.undo_move(Move::from_usi_strict("7g7f").unwrap());
        assert_eq!(position.to_sfen(), rshogi_core::position::SFEN_HIRATE);
    }

    #[test]
    fn position_commands_accept_moves_without_kings() {
        let mut failures = Vec::new();
        for (sfen, moves, expected) in [
            ("4k4/9/9/9/9/9/9/9/8r b G 1", "", "4k4/9/9/9/9/9/9/9/8r b G 1"),
            ("4k4/8g/4P4/9/9/9/9/9/9 b G 1", "G*5b", "4k4/4G3g/4P4/9/9/9/9/9/9 w - 2"),
            ("4k4/9/9/9/9/9/9/9/8r b G 1", "G*5b", "4k4/4G4/9/9/9/9/9/9/8r w - 2"),
            ("4k4/9/9/9/9/9/9/9/8r b G 1", "G*5b 5a5b", "9/4k4/9/9/9/9/9/9/8r b g 3"),
            ("8G/7r1/9/9/9/9/9/9/4k4 b - 1", "1a1b", "9/7rG/9/9/9/9/9/9/4k4 w - 2"),
            ("4k4/9/9/9/8G/9/9/9/8r b - 1", "1e2e", "4k4/9/9/9/7G1/9/9/9/8r w - 2"),
            ("7pp/7p1/7G1/9/9/9/9/9/4K4 b P 1", "P*1b", "7pp/7pP/7G1/9/9/9/9/9/4K4 w - 2"),
            ("9/9/9/9/9/9/9/9/4K3R w g 1", "G*5h", "9/9/9/9/9/9/9/4g4/4K3R b - 2"),
            (
                "4k4/9/4p4/4P4/9/9/9/9/9 b - 23",
                "5d5c+ 5a4a P*5e",
                "5k3/9/4+P4/9/4P4/9/9/9/9 w - 26",
            ),
        ] {
            let mut position = Position::new();
            let line = format!("position sfen {sfen} moves {moves}");
            if let Err(err) = UsiEngine::apply_position_tokens(
                &mut position,
                &line.split_whitespace().collect::<Vec<_>>(),
                false,
                2,
            ) {
                failures.push(format!("{line}: {err}"));
                continue;
            }
            assert_eq!(position.to_sfen(), expected, "{line}");
            if position.pieces(position.side_to_move(), PieceType::King).is_empty() {
                assert!(!position.in_check(), "{line}");
            }
            for token in moves.split_whitespace().rev() {
                position.undo_move(Move::from_usi_strict(token).unwrap());
                if position.pieces(position.side_to_move(), PieceType::King).is_empty() {
                    assert!(!position.in_check(), "{line}");
                }
            }
            assert_eq!(position.to_sfen(), sfen, "{line}");
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn position_commands_missing_kings_preserve_repetition_history() {
        let sfen = "9/9/9/9/4G4/9/9/9/8r b - 1";
        let cycle = "5e4e 1i1h 4e5e 1h1i ";
        let line = format!("position sfen {sfen} moves {}", cycle.repeat(3));
        let mut position = Position::new();
        UsiEngine::apply_position_tokens(
            &mut position,
            &line.split_whitespace().collect::<Vec<_>>(),
            false,
            2,
        )
        .unwrap();
        assert_eq!(position.to_sfen(), "9/9/9/9/4G4/9/9/9/8r b - 13");
        assert!(!position.in_check());
        assert_eq!(position.repetition_state(16), rshogi_core::types::RepetitionState::Draw);
        assert_eq!(position.state().continuous_check, [0, 0]);
        for token in cycle.repeat(3).split_whitespace().rev() {
            position.undo_move(Move::from_usi_strict(token).unwrap());
            assert!(!position.in_check());
        }
        assert_eq!(position.to_sfen(), sfen);
    }

    #[test]
    fn position_commands_missing_king_pass_preserves_rights() {
        use rshogi_core::types::Color;

        let sfen = "9/9/9/9/9/9/9/9/8r b G 1";
        let line = format!("position sfen {sfen} passrights 1 1 moves pass 0000");
        let mut position = Position::new();
        UsiEngine::apply_position_tokens(
            &mut position,
            &line.split_whitespace().collect::<Vec<_>>(),
            true,
            2,
        )
        .unwrap();
        assert_eq!(position.to_sfen(), "9/9/9/9/9/9/9/9/8r b G 3");
        assert_eq!(position.pass_rights(Color::Black), 0);
        assert_eq!(position.pass_rights(Color::White), 0);
        assert!(!position.in_check());
        assert_eq!(position.state().continuous_check, [0, 0]);
        let before = position.to_sfen();
        let key = position.key();
        for (enabled, moves) in [(false, "pass"), (true, "pass 0000 pass")] {
            let line = format!("position sfen {sfen} passrights 1 1 moves {moves}");
            assert!(
                UsiEngine::apply_position_tokens(
                    &mut position,
                    &line.split_whitespace().collect::<Vec<_>>(),
                    enabled,
                    2,
                )
                .is_err(),
                "{line}"
            );
            assert_eq!(position.to_sfen(), before);
            assert_eq!(position.key(), key);
        }
        position.undo_move(Move::PASS);
        position.undo_move(Move::PASS);
        assert_eq!(position.to_sfen(), sfen);
        assert_eq!(position.pass_rights(Color::Black), 1);
        assert_eq!(position.pass_rights(Color::White), 1);
        assert!(!position.in_check());

        for sfen in [
            "4k4/9/9/9/4R4/9/9/9/9 b - 1",
            "4k4/9/9/9/4R4/9/9/9/4K4 b - 1",
        ] {
            let line = format!("position sfen {sfen} passrights 1 1 moves pass");
            UsiEngine::apply_position_tokens(
                &mut position,
                &line.split_whitespace().collect::<Vec<_>>(),
                true,
                2,
            )
            .unwrap();
            assert_eq!(position.side_to_move(), Color::White);
            assert!(position.in_check());
            assert!(!position.can_pass());
            assert_eq!(position.state().continuous_check, [2, 0]);
            let before = position.to_sfen();
            let key = position.key();
            let invalid = format!("{line} 0000");
            assert!(
                UsiEngine::apply_position_tokens(
                    &mut position,
                    &invalid.split_whitespace().collect::<Vec<_>>(),
                    true,
                    2,
                )
                .is_err()
            );
            assert_eq!(position.to_sfen(), before);
            assert_eq!(position.key(), key);
            assert_eq!(position.state().continuous_check, [2, 0]);
            position.undo_move(Move::PASS);
            assert_eq!(position.to_sfen(), sfen);
            assert_eq!(position.pass_rights(Color::Black), 1);
            assert_eq!(position.pass_rights(Color::White), 1);
            assert_eq!(position.state().continuous_check, [0, 0]);
        }
    }

    #[test]
    fn position_commands_reject_invalid_input_atomically() {
        let mut position = Position::new();
        UsiEngine::apply_position_tokens(
            &mut position,
            &["position", "startpos", "moves", "7g7f", "3c3d"],
            false,
            2,
        )
        .unwrap();
        let before = position.to_sfen();
        let key = position.key();
        for line in [
            "position",
            "position unknown",
            "position startpos extra",
            "position sfen invalid b - 1",
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 x - 1",
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 b 19P 1",
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 b - invalid",
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 b - 0",
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 b - -1",
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1 extra",
            "position sfen 4k4/9/9/9/9/9/9/9/4K4 b - 2147483647 moves 5i5h",
            "position startpos moves 7g7e",
            "position startpos moves 5e5d",
            "position startpos moves 3c3d",
            "position startpos moves 7g7f+",
            "position startpos moves 6i6h+",
            "position startpos moves P*5e",
            "position startpos moves 7g7f 3c3d 5e5d",
            "position startpos moves 7g7f 3c3d invalid",
            "position startpos moves 7g7fextra",
            "position startpos moves 7g7fあ",
            "position startpos moves none",
            "position startpos moves win",
            "position startpos moves pass",
            "position startpos moves 0000",
            "position sfen 4r3k/9/9/9/9/9/9/4G4/4K4 b - 1 moves 5h4h",
            "position sfen 8k/4P4/9/9/9/9/9/9/4K4 b - 1 moves 5b5a",
            "position sfen 4k4/9/3GN1B2/4R4/9/9/9/9/4K4 b P 1 moves P*5b",
            "position sfen 4k4/9/9/9/4P4/9/9/9/4K4 b P 1 moves P*5d",
            "position sfen 4k4/9/9/9/9/9/4P4/9/9 b - 1 moves 5g5e",
            "position sfen 4k4/9/9/9/9/9/4P4/9/9 b - 1 moves 5g5f+",
            "position sfen 4k4/9/9/9/9/9/9/9/9 b - 1 moves 5e5d",
            "position sfen 4k4/9/9/9/9/9/9/9/9 b - 1 moves 5a5b",
            "position sfen 4k4/9/9/9/9/9/9/9/9 b - 1 moves P*5e",
            "position sfen 4k4/9/9/9/4P4/9/9/9/9 b P 1 moves P*5d",
            "position sfen 4k4/9/9/9/9/9/9/9/9 b P 1 moves P*4a",
            "position sfen 4k4/9/9/9/9/9/9/9/9 b L 1 moves L*4a",
            "position sfen 4k4/9/9/9/9/9/9/9/9 b N 1 moves N*4b",
            "position sfen 8k/4P4/9/9/9/9/9/9/9 b - 1 moves 5b5a",
            "position sfen 8k/4G4/9/9/9/9/9/9/9 b - 1 moves 5b5a+",
            "position sfen 4k4/4G4/9/9/9/9/9/9/9 b - 1 moves 5b5a",
            "position sfen 4k4/9/3GN1B2/4R4/9/9/9/9/9 b P 1 moves P*5b",
            "position sfen 4r4/9/9/9/9/9/9/4G4/4K4 b - 1 moves 5h4h",
            "position sfen 4r4/9/9/9/9/9/9/9/4K4 b - 1 moves 5i5h",
            "position sfen 4k4/9/9/9/9/9/9/9/8r b G 1 moves G*5b 5a5b 5e5d",
        ] {
            let tokens: Vec<_> = line.split_whitespace().collect();
            assert!(
                UsiEngine::apply_position_tokens(&mut position, &tokens, false, 2).is_err(),
                "{line}"
            );
            assert_eq!(position.to_sfen(), before, "{line}");
            assert_eq!(position.key(), key, "{line}");
        }
        position.undo_move(Move::from_usi_strict("3c3d").unwrap());
        position.undo_move(Move::from_usi_strict("7g7f").unwrap());
        assert_eq!(position.to_sfen(), rshogi_core::position::SFEN_HIRATE);
    }

    #[test]
    fn position_commands_preserve_capture_drop_and_repetition_history() {
        let mut position = Position::new();
        let line = "position sfen 4k4/9/4p4/4P4/9/9/9/9/4K4 b - 23 moves 5d5c+ 5a4a P*5e";
        UsiEngine::apply_position_tokens(
            &mut position,
            &line.split_whitespace().collect::<Vec<_>>(),
            false,
            2,
        )
        .unwrap();
        assert_eq!(position.to_sfen(), "5k3/9/4+P4/9/4P4/9/9/9/4K4 w - 26");
        for token in ["P*5e", "5a4a", "5d5c+"] {
            position.undo_move(Move::from_usi_strict(token).unwrap());
        }
        assert_eq!(position.to_sfen(), "4k4/9/4p4/4P4/9/9/9/9/4K4 b - 23");

        let cycle = "5i5h 5a5b 5h5i 5b5a ";
        let line = format!("position sfen 4k4/9/9/9/9/9/9/9/4K4 b P 11 moves {}", cycle.repeat(3));
        UsiEngine::apply_position_tokens(
            &mut position,
            &line.split_whitespace().collect::<Vec<_>>(),
            false,
            2,
        )
        .unwrap();
        assert_eq!(position.game_ply(), 23);
        assert_eq!(position.repetition_state(16), rshogi_core::types::RepetitionState::Draw);
        let before = position.to_sfen();
        assert!(
            UsiEngine::apply_position_tokens(
                &mut position,
                &["position", "startpos", "moves", "5e5d"],
                false,
                2,
            )
            .is_err()
        );
        assert_eq!(position.to_sfen(), before);
        assert_eq!(position.repetition_state(16), rshogi_core::types::RepetitionState::Draw);
    }

    #[test]
    fn position_commands_validate_pass_rights_and_pass_legality() {
        use rshogi_core::types::Color;
        let mut position = Position::new();
        for (line, enabled, black, white) in [
            ("position startpos moves pass 0000", true, 1, 1),
            ("position startpos passrights 1 1 moves pass 0000", true, 0, 0),
            ("position startpos passrights 15 15", true, 15, 15),
            ("position startpos passrights 2 3", false, 0, 0),
        ] {
            UsiEngine::apply_position_tokens(
                &mut position,
                &line.split_whitespace().collect::<Vec<_>>(),
                enabled,
                2,
            )
            .unwrap();
            assert_eq!(position.is_pass_rights_enabled(), enabled);
            assert_eq!(position.pass_rights(Color::Black), black);
            assert_eq!(position.pass_rights(Color::White), white);
        }
        let before = position.to_sfen();
        let key = position.key();
        for enabled in [false, true] {
            for suffix in ["", "1", "x 1", "1 x", "16 1", "1 256", "1 1 extra"] {
                let line = format!("position startpos passrights {suffix}");
                assert!(
                    UsiEngine::apply_position_tokens(
                        &mut position,
                        &line.split_whitespace().collect::<Vec<_>>(),
                        enabled,
                        2,
                    )
                    .is_err()
                );
                assert_eq!(position.to_sfen(), before);
                assert_eq!(position.key(), key);
            }
        }
        for line in [
            "position startpos passrights 1 1 moves pass 0000 pass",
            "position sfen 4r3k/9/9/9/9/9/9/9/4K4 b - 1 passrights 1 1 moves pass",
        ] {
            assert!(
                UsiEngine::apply_position_tokens(
                    &mut position,
                    &line.split_whitespace().collect::<Vec<_>>(),
                    true,
                    2,
                )
                .is_err()
            );
            assert_eq!(position.to_sfen(), before);
            assert_eq!(position.key(), key);
        }
    }

    #[test]
    #[serial]
    fn stochastic_ponder_position_rewinds_last_move() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut engine = UsiEngine::new();
                let line = "position startpos moves 7g7f 3c3d";
                engine.process_command(line).unwrap();
                let before = engine.position.to_sfen();
                engine.process_command("position startpos moves 7g7f 5e5d").unwrap();
                assert_eq!(engine.last_position_cmd.as_deref(), Some(line));
                assert_eq!(engine.position.to_sfen(), before);

                let pos = engine.stochastic_ponder_position().expect("stochastic ponder position");
                assert_eq!(
                    pos.to_sfen(),
                    "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2"
                );
                engine.last_position_cmd = Some("position startpos moves 5e5d 3c3d".into());
                assert!(engine.stochastic_ponder_position().is_none());
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
    fn setoption_layerstack_bucket_updates_pending_configuration() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                use rshogi_core::nnue::{
                    LayerStackBucketMode, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
                    get_layer_stack_progress_buckets, get_layer_stack_progress_kpabs_weights,
                    reset_layer_stack_progress_buckets, reset_layer_stack_progress_kpabs_weights,
                };

                // テスト開始時に既定値へ戻す
                reset_layer_stack_progress_kpabs_weights();
                rshogi_core::nnue::reset_layer_stack_progress_kpabs_q16_weights();

                let mut engine = UsiEngine::new();
                engine.cmd_setoption(&[
                    "setoption",
                    "name",
                    "LS_BUCKET_MODE",
                    "value",
                    "progresskpabs",
                ]);
                assert_eq!(engine.ls_bucket_mode, Some(LayerStackBucketMode::ProgressKPAbs));

                engine.cmd_setoption(&["setoption", "name", "LS_PROGRESS_BUCKETS", "value", "8"]);
                assert_eq!(engine.ls_progress_buckets, Some(8));
                assert_eq!(get_layer_stack_progress_buckets(), None);

                engine.cmd_setoption(&[
                    "setoption",
                    "name",
                    "LS_BUCKET_MODE",
                    "value",
                    "kingrank9",
                ]);
                assert_eq!(engine.ls_bucket_mode, Some(LayerStackBucketMode::KingRank9));

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
                assert!(engine.ls_progress_coeff_loaded);
                let _ = std::fs::remove_file(tmp_path_bin);

                // 他テストへの影響を避けるため復元
                reset_layer_stack_progress_buckets();
                reset_layer_stack_progress_kpabs_weights();
                rshogi_core::nnue::reset_layer_stack_progress_kpabs_q16_weights();
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn validate_layer_stack_routing_matrix() {
        use LayerStackBucketMode::{KingRank9, ProgressKPAbs};

        assert!(validate_layer_stack_routing(9, Some(ProgressKPAbs), Some(8), true).is_ok());
        assert!(validate_layer_stack_routing(8, Some(ProgressKPAbs), Some(8), true).is_ok());
        assert!(validate_layer_stack_routing(9, Some(ProgressKPAbs), Some(9), true).is_ok());
        assert!(validate_layer_stack_routing(9, Some(KingRank9), None, false).is_ok());
        // 1 は常に bucket 0 を選ぶ no-op routing (格納 1 bucket net の唯一の設定経路)。
        // 係数を参照しないため LS_PROGRESS_COEFF 未ロードでも通る (ロード済みも可)。
        assert!(validate_layer_stack_routing(1, Some(ProgressKPAbs), Some(1), false).is_ok());
        assert!(validate_layer_stack_routing(1, Some(ProgressKPAbs), Some(1), true).is_ok());
        assert!(validate_layer_stack_routing(9, Some(ProgressKPAbs), Some(1), false).is_ok());

        assert!(validate_layer_stack_routing(9, None, None, false).is_err());
        assert!(validate_layer_stack_routing(9, Some(ProgressKPAbs), None, true).is_err());
        assert!(validate_layer_stack_routing(8, Some(ProgressKPAbs), Some(9), true).is_err());
        assert!(validate_layer_stack_routing(9, Some(ProgressKPAbs), Some(8), false).is_err());
        assert!(validate_layer_stack_routing(8, Some(KingRank9), None, false).is_err());
        assert!(validate_layer_stack_routing(9, Some(KingRank9), Some(8), false).is_err());
        assert!(validate_layer_stack_routing(9, Some(KingRank9), None, true).is_err());
    }
}
