use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;
use crossbeam_channel::unbounded;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tools::selfplay::game::{GameConfig, MoveEvent, run_game};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::{
    EngineConfig, EngineProcess, GameOutcome, ParsedPosition, load_start_positions,
};
use tools::spsa_param_mapping::{
    MappingTable, NOT_USED_MARKER as PARAM_NOT_USED_MARKER, RawParamRow, parse_param_line,
};

/// `meta.json` のフォーマットバージョン。
///
/// v2 → v3: `params_sha256` / `init_from_sha256` / `engine_path` /
/// `engine_param_mapping_*` / `param_name_set_hash` / `active_param_count` /
/// `init_mode` / `init_from_path` を追加。`--init-from` の暗黙スキップを禁止し、
/// resume 時に params 内容と name set の整合性を hash で検証する。
///
/// v3 → v4 (本 PR): `current_params_sha256` を追加。各反復で state.params 更新後に
/// その時点の hash を meta に記録する。resume 起動時に on-disk state.params の hash と
/// 突き合わせ、両者が乖離していれば「state.params だけ更新後に meta 更新前にクラッシュ」
/// または「外部から state.params を書き換えられた」として bail (or warn)。
///
/// 互換性: vN は v(N-1) を読まない (hard bail)。古い run dir で resume したい場合は
/// 新規 run dir で `--init-from <canonical>` から fresh start する。
const META_FORMAT_VERSION: u32 = 4;

#[derive(Parser, Debug)]
#[command(author, version, about = "SPSA tuner for USI engines")]
struct Cli {
    /// SPSA 実行ディレクトリ。state / meta / CSV を全てこの dir 配下に配置する。
    ///
    /// 配置されるファイル (override は個別フラグで可能):
    /// - `<run-dir>/state.params`        : SPSA の live 状態 (反復ごとに上書き)
    /// - `<run-dir>/meta.json`           : resume 用メタデータ
    /// - `<run-dir>/values.csv`          : 各 iter のパラメータ値履歴
    /// - `<run-dir>/stats.csv`           : per-seed 統計
    /// - `<run-dir>/stats_aggregate.csv` : seed 横断集計
    ///
    /// 通常は `runs/spsa/$(date -u +%Y%m%d_%H%M%S)_<tag>` のように毎回新規 dir を
    /// 切る。`--init-from <canonical>` を併用すると初回起動時に canonical を
    /// `<run-dir>/state.params` に複製する。
    #[arg(long)]
    run_dir: PathBuf,

    /// 反復回数
    #[arg(long, default_value_t = 1)]
    iterations: u32,

    /// 1イテレーションあたり対局数（偶数必須）
    #[arg(long, default_value_t = 2)]
    games_per_iteration: u32,

    /// 対局並列数（worker数）
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// 更新移動量スケール
    #[arg(long, default_value_t = 1.0)]
    mobility: f64,

    /// Fishtest A ratio（A = a_ratio * iterations）
    #[arg(long = "a-ratio", default_value_t = 0.1)]
    a_ratio: f64,

    /// SPSA alpha（a_k 減衰指数）
    #[arg(long, default_value_t = 0.602)]
    alpha: f64,

    /// SPSA gamma（c_k 減衰指数）
    #[arg(long, default_value_t = 0.101)]
    gamma: f64,

    /// 再開メタデータファイル（既定: <run-dir>/meta.json）
    #[arg(long)]
    meta_file: Option<PathBuf>,

    /// 既存メタデータから反復番号を再開する
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// resume時にmetaのschedule不一致を許可する
    #[arg(long, default_value_t = false)]
    force_schedule: bool,

    /// 反復統計CSVの出力先（resume時は追記）。既定: <run-dir>/stats.csv
    #[arg(long)]
    stats_csv: Option<PathBuf>,

    /// 反復統計CSVの出力を無効化する
    #[arg(long, default_value_t = false)]
    no_stats_csv: bool,

    /// 反復統計のseed横断集計CSV（平均・分散）。既定: <run-dir>/stats_aggregate.csv
    #[arg(long)]
    stats_aggregate_csv: Option<PathBuf>,

    /// seed横断集計CSVの出力を無効化する
    #[arg(long, default_value_t = false)]
    no_stats_aggregate_csv: bool,

    /// 反復ごとのパラメータ値履歴CSV（wide形式）。既定: <run-dir>/values.csv
    #[arg(long)]
    param_values_csv: Option<PathBuf>,

    /// パラメータ値履歴CSVの出力を無効化する
    #[arg(long, default_value_t = false)]
    no_param_values_csv: bool,

    /// 乱数seed（単一）
    #[arg(long, conflicts_with = "seeds")]
    seed: Option<u64>,

    /// 乱数seed一覧（カンマ区切り）
    #[arg(long, value_delimiter = ',', num_args = 1.., conflicts_with = "seed")]
    seeds: Option<Vec<u64>>,

    /// エンジンバイナリパス（未指定時: target/release/rshogi-usi）
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// エンジン追加引数
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// 追加USIオプション（Name=Value形式、複数指定可）
    #[arg(long = "usi-option", num_args = 1..)]
    usi_options: Option<Vec<String>>,

    /// Threads option
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Hash/USI_Hash (MiB)
    #[arg(long, default_value_t = 256)]
    hash_mb: u32,

    /// 秒読み(ms)。--btime 指定時は無視される。
    #[arg(long, default_value_t = 1000)]
    byoyomi: u64,

    /// フィッシャー: 持ち時間(ms)。指定時は byoyomi を無視しフィッシャーモードになる。
    #[arg(long)]
    btime: Option<u64>,

    /// フィッシャー: 加算時間(ms)。--btime と併用する。
    #[arg(long, default_value_t = 0)]
    binc: u64,

    /// ノード数制限。指定時は時間制御の代わりに `go nodes N` を使用する。
    #[arg(long)]
    nodes: Option<u64>,

    /// 1局あたり最大手数
    #[arg(long, default_value_t = 320)]
    max_moves: u32,

    /// タイムアウト判定マージン(ms)
    #[arg(long, default_value_t = 1000)]
    timeout_margin_ms: u64,

    /// 開始局面ファイル
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// --startpos-file の指定を必須化する
    #[arg(long, default_value_t = false)]
    require_startpos_file: bool,

    /// 単一開始局面（position行またはSFEN）
    #[arg(long)]
    sfen: Option<String>,

    /// 開始局面をランダム選択
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    random_startpos: bool,

    /// チューニング対象パラメータ名を正規表現で限定する
    #[arg(long)]
    active_only_regex: Option<String>,

    /// 早期停止: avg_abs_update の閾値（以下で条件成立）
    #[arg(long)]
    early_stop_avg_abs_update_threshold: Option<f64>,

    /// 早期停止: result_variance の閾値（以下で条件成立）
    #[arg(long)]
    early_stop_result_variance_threshold: Option<f64>,

    /// 早期停止: 条件連続成立回数（0で無効）
    #[arg(long, default_value_t = 0)]
    early_stop_patience: u32,

    /// エンジン側パラメータ名マッピング TOML（例: tune/yo_rshogi_mapping.toml）。
    /// 指定時、`.params` の rshogi 名 (`SPSA_*`) を、setoption する直前にエンジン側名前空間
    /// （例: YaneuraOu の `correction_value_1`）に翻訳し、必要なら符号を反転する。
    /// マッピング表に存在しないパラメータはそのままの名前で送る。
    #[arg(long)]
    engine_param_mapping: Option<PathBuf>,

    /// canonical (起点) parameter ファイル。
    ///
    /// 用途:
    /// - **fresh start**: `<run-dir>/state.params` 不在時、canonical を
    ///   `state.params` にコピーして開始する。
    /// - **resume の整合性検証**: `--resume` と併用すると、起動時に既存
    ///   `state.params` と canonical の値乖離を diagnostic 出力する
    ///   (閾値超過時の bail は `--strict-init-check` で有効化)。
    ///
    /// 既存 `state.params` がある状態での fresh 系操作は `--resume` か
    /// `--force-init` のいずれかの明示が必要 (詳細: runbook §4.1)。
    #[arg(long)]
    init_from: Option<PathBuf>,

    /// 既存の `<run-dir>/state.params` を canonical で atomic に上書きして
    /// fresh start する。`--init-from` の指定が必須。
    ///
    /// 既存 `meta.json` / 各 CSV も削除して fresh run として扱う。`--resume`
    /// とは同時指定不可 (意味が矛盾)。
    #[arg(long, default_value_t = false)]
    force_init: bool,

    /// 既存 `<run-dir>/state.params` を canonical の代わりに「そのまま起点」として
    /// fresh start を許可する。
    ///
    /// 通常運用ではこのフラグは不要。`--init-from` を指定して canonical を明示する
    /// のが推奨経路。本フラグは「外部ツールで生成した state.params を直接 spsa に
    /// 食わせる」「過去 run の最終 state を seed に新 run を始める (=resume では
    /// なく fresh)」等の特殊ユースケース向けに、明示的な意思表示として用意する。
    ///
    /// 既定で `--init-from` なし + 既存 state は bail (silent fresh は事故の温床
    /// だったため)。本フラグは `--init-from` / `--resume` / `--force-init` のいずれ
    /// とも同時指定不可 (意味が矛盾する)。
    #[arg(long, default_value_t = false)]
    use_existing_state_as_init: bool,

    /// `<run-dir>/.lock` が残留している場合に強制削除して取得を試みる。
    ///
    /// 通常 lock は process 正常終了時 / panic 時に削除される。電源断・
    /// SIGKILL 等で残ってしまった場合のみこのフラグを使う。間違って実行中
    /// の SPSA を巻き込むと state.params / meta.json が race condition で
    /// 壊れるので、必ず lock 内容 (PID/hostname/start) を確認して当該
    /// プロセスが死んでいることを目視確認してから指定すること。
    #[arg(long, default_value_t = false)]
    force_unlock: bool,

    /// `--resume` + `--init-from` 併用時の整合性チェックを strict にする。
    ///
    /// デフォルトは warning 出力のみで継続。strict 指定時は median ≥ 0.5 step
    /// または max ≥ 5 step の乖離があれば bail する。CI / 自動化で「想定外の
    /// resume」を早期検出したい場合に使う。
    #[arg(long, default_value_t = false)]
    strict_init_check: bool,

    /// `--seeds` を 2 つ以上指定したとき、iter 内の seed 群を並列実行する。
    /// SPSA の数学的妥当性は保たれる（各 seed は独立な摂動方向を持ち、iter 末で
    /// 平均化される）。`--concurrency / seeds_count` を各 seed に配分するため、
    /// 最大効率には **`--concurrency` は `seeds_count` の倍数を推奨**
    /// （割り切れない端数は浪費される: 例 conc=10, seeds=3 → per_seed=3, 1 枠無駄）。
    /// 単一 seed のときは通常通り順次実行（フラグは無視）。
    #[arg(long, default_value_t = false)]
    parallel_seeds: bool,
}

#[derive(Clone, Debug)]
struct SpsaParam {
    name: String,
    type_name: String,
    is_int: bool,
    value: f64,
    min: f64,
    max: f64,
    /// Fishtest c_end: 最終摂動幅
    c_end: f64,
    /// Fishtest r_end: 最終学習率係数
    r_end: f64,
    comment: String,
    not_used: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct ScheduleConfig {
    alpha: f64,
    gamma: f64,
    a_ratio: f64,
    mobility: f64,
    total_iterations: u32,
}

/// Fishtest 方式の per-param スケジュール定数。イテレーション開始前に一度だけ計算する。
#[derive(Clone, Copy, Debug)]
struct ParamScheduleConstants {
    /// c_0 = c_end × N^γ
    c_0: f64,
    /// a_0 = r_end × c_end² × (A + N)^α
    a_0: f64,
}

impl ParamScheduleConstants {
    fn compute(
        c_end: f64,
        r_end: f64,
        total_iter: u32,
        a_ratio: f64,
        alpha: f64,
        gamma: f64,
    ) -> Self {
        let n = total_iter as f64;
        let big_a = a_ratio * n;
        let c_0 = c_end * n.powf(gamma);
        let a_end = r_end * c_end * c_end;
        let a_0 = a_end * (big_a + n).powf(alpha);
        Self { c_0, a_0 }
    }

    /// イテレーション k (0-indexed) での (c_k, R_k) を返す。
    fn at_iteration(&self, k: u32, big_a: f64, alpha: f64, gamma: f64) -> (f64, f64) {
        let t = k as f64 + 1.0;
        let c_k = self.c_0 / t.powf(gamma);
        let r_k = self.a_0 / (big_a + t).powf(alpha) / (c_k * c_k);
        (c_k, r_k)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResumeMetaData {
    format_version: u32,
    /// `<run-dir>/state.params` の絶対 or 相対パス文字列 (起動時に記録)。
    state_params_file: String,
    completed_iterations: u32,
    total_games: usize,
    last_raw_result_mean: f64,
    last_avg_abs_update: f64,
    updated_at_utc: String,
    schedule: ScheduleConfig,
    // --- v3 で追加 ---
    /// 起動時 (iter 0) の `<run-dir>/state.params` 全体の SHA-256 hex。fresh start / force-init 時に
    /// その時点の params 内容を記録する。SPSA 進行で値は変わるので resume 中の
    /// 検証では使わず、事故解析用の起動時スナップショットとして残す。
    init_params_sha256: String,
    /// `--init-from` 指定時のソース hex。同 path で再走時に「同じ canonical を
    /// 使ったか」を後追い確認可能。
    init_from_sha256: Option<String>,
    /// `--init-from` のパス文字列 (起動時の指定そのまま、絶対パス化はしない)。
    init_from_path: Option<String>,
    /// param 名集合の SHA-256 hex (sort 済み name を `\n` join して hash)。
    /// resume 時に param 集合が変わっていないことの検証に使う。
    param_name_set_sha256: String,
    /// 起動時の active param 数 (active_only_regex / not_used / mapping 適用後)。
    active_param_count: usize,
    /// 起動時の engine binary パス (resolve 後の絶対 or 相対パス、解決時のまま)。
    engine_path: String,
    /// `--engine-param-mapping` のパス (指定時のみ)。
    engine_param_mapping_path: Option<String>,
    /// `--engine-param-mapping` ファイルの SHA-256 hex (指定時のみ)。
    engine_param_mapping_sha256: Option<String>,
    /// 起動モード。`InitMode` の serde 表現 (kebab-case)。
    init_mode: InitMode,
    // --- v4 で追加 ---
    /// 反復ごとに更新される現 state.params の SHA-256。`save_meta` の直前に
    /// hash を計算して記録する (write_params → meta save の transactional 復旧
    /// 検証に使う)。
    /// resume 起動時に on-disk hash と突き合わせ、乖離があれば「state だけ更新で
    /// 落ちた」or「外部から state を書き換えられた」と判断して bail。
    /// 反復 0 (起動時 snapshot) では `init_params_sha256` と同値で開始する。
    current_params_sha256: String,
}

/// 起動時に決まる SPSA 走行モード。
///
/// `meta.json` 内に kebab-case 文字列で保存される (`"fresh-init-from"` 等)。
/// String 直書きは typo の温床なので enum + serde で型安全化。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum InitMode {
    /// `--init-from` 指定 + `<run-dir>/state.params` 不在 → canonical を copy して fresh start。
    FreshInitFrom,
    /// `--init-from` なし + `<run-dir>/state.params` 既存 → 既存ファイルでそのまま fresh start。
    FreshExisting,
    /// `--init-from` 指定 + `<run-dir>/state.params` 既存 + `--force-init` → 上書き再初期化。
    ForceInit,
    /// `--resume` で既存 run を継続 (run 全体としてのモードは初回起動時のもの)。
    Resume,
}

impl std::fmt::Display for InitMode {
    /// stderr/log 表示用 kebab-case 文字列。`meta.json` の serde 表現と一致させる
    /// ことで「何が記録されたか」「何が起動したか」を視覚的に紐付けやすくする。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::FreshInitFrom => "fresh-init-from",
            Self::FreshExisting => "fresh-existing",
            Self::ForceInit => "force-init",
            Self::Resume => "resume",
        };
        f.write_str(s)
    }
}

#[derive(Clone, Copy, Debug)]
struct IterationStats {
    iteration: u32,
    seed: u64,
    games: u32,
    plus_wins: u32,
    minus_wins: u32,
    draws: u32,
    raw_result: f64,
    active_params: usize,
    avg_abs_shift: f64,
    updated_params: usize,
    avg_abs_update: f64,
    max_abs_update: f64,
    total_games: usize,
}

#[derive(Clone, Copy, Debug)]
struct AggregateIterationStats {
    iteration: u32,
    seed_count: usize,
    games_per_seed: u32,
    raw_result_mean: f64,
    raw_result_variance: f64,
    plus_wins_mean: f64,
    plus_wins_variance: f64,
    minus_wins_mean: f64,
    minus_wins_variance: f64,
    draws_mean: f64,
    draws_variance: f64,
    total_games: usize,
}

#[derive(Clone, Copy, Debug)]
struct GameTask {
    game_idx: u32,
    plus_is_black: bool,
    start_pos_index: usize,
    game_id: u32,
}

#[derive(Clone, Copy)]
struct GameTaskResult {
    game_idx: u32,
    plus_is_black: bool,
    plus_score: f64,
    outcome: GameOutcome,
}

#[derive(Clone, Copy, Debug)]
struct SeedGameStats {
    step_sum: f64,
    plus_wins: u32,
    minus_wins: u32,
    draws: u32,
}

/// 1 seed × 1 iter 分の事前計算結果（rng / flips / shifts / plus / minus / startpos インデックス）。
///
/// `compute_seed_prep` で生成し、`run_seed_games_parallel` の入力として使う。
/// 事前計算を seed 並列実行から分離することで、決定論を維持したまま重いゲーム実行のみを並列化できる。
struct SeedPrep {
    base_seed: u64,
    flips: Vec<f64>,
    plus_values: Vec<f64>,
    minus_values: Vec<f64>,
    start_pos_indices: Vec<usize>,
    active_params: usize,
    avg_abs_shift: f64,
    seed_total_games_start: usize,
}

struct SeedRunContext<'a> {
    concurrency: usize,
    base_cfg: &'a EngineConfig,
    params: &'a [SpsaParam],
    plus_values: &'a [f64],
    minus_values: &'a [f64],
    start_positions: &'a [ParsedPosition],
    start_pos_indices: &'a [usize],
    game_cfg: &'a GameConfig,
    tc: TimeControl,
    total_games_start: usize,
    iteration: u32,
    seed_idx: usize,
    seed_count: usize,
    base_seed: u64,
    translator: &'a EngineNameTranslator,
    active_mask: &'a [bool],
}

/// rshogi `.params` の名前 → エンジン側 USI option 名 への翻訳器
///
/// 不変条件: `from_mapping_file` / `empty` で構築後は **immutable**。
/// `&Self` は worker thread 間で `thread::scope` 経由で共有して安全（`HashMap`
/// は `Sync` であり、`translate` は内部状態を変更しない）。将来 `enabled` を
/// `AtomicBool` 化したり内部可変性を入れる場合は、共有読み取りの安全性を
/// 再評価すること。
#[derive(Debug, Default)]
struct EngineNameTranslator {
    /// rshogi 名 → (エンジン側名, 符号反転)。
    table: HashMap<String, (String, bool)>,
    /// マッピング表がロードされているか
    enabled: bool,
}

impl EngineNameTranslator {
    fn empty() -> Self {
        Self {
            table: HashMap::new(),
            enabled: false,
        }
    }

    fn from_mapping_file(path: &Path) -> Result<Self> {
        let mapping = MappingTable::load(path)?;
        let table = mapping
            .mappings
            .iter()
            .map(|m| (m.rshogi.clone(), (m.yo.clone(), m.sign_flip)))
            .collect();
        Ok(Self {
            table,
            enabled: true,
        })
    }

    /// `value` を必要に応じて符号反転し、エンジン側に送る (name, value) を返す。
    /// マッピング表にない name はそのまま通す。
    fn translate<'a>(&'a self, name: &'a str, value: f64) -> (&'a str, f64) {
        match self.table.get(name) {
            Some((engine_name, sign_flip)) => {
                let v = if *sign_flip { -value } else { value };
                (engine_name.as_str(), v)
            }
            None => (name, value),
        }
    }

    fn len(&self) -> usize {
        self.table.len()
    }

    /// マッピング表がロードされているか
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// rshogi 名がマッピング表に登録されているか
    fn is_mapped(&self, rshogi_name: &str) -> bool {
        self.table.contains_key(rshogi_name)
    }
}

#[derive(Clone, Copy, Debug)]
struct EarlyStopConfig {
    avg_abs_update_threshold: f64,
    result_variance_threshold: f64,
    patience: u32,
}

/// `<run-dir>/state.params`: SPSA の live 状態ファイル。
fn state_params_path(run_dir: &Path) -> PathBuf {
    run_dir.join("state.params")
}

/// `--force-init` 時に削除する run-dir 直下の派生ファイル一覧。
///
/// state.params は `apply_init_action` 内で atomic copy される (削除→上書きでなく
/// rename) ため、本リストには含めない。`meta.json` も apply_init_action が個別に
/// 「失敗で bail」セマンティクスで削除するため別扱い。
///
/// **CSV override 先 (`--stats-csv` / `--stats-aggregate-csv` /
/// `--param-values-csv` で run-dir 外を指定した場合) はこの関数の戻り値に
/// 含めない**: CSV は run の物理進行ログであり、外部集約 CSV に append する
/// 運用 (複数 run の比較ログ等) を force-init で破壊しないため。なお
/// `--meta-file` の override 先は本関数では扱わず、`apply_init_action` 側で
/// 別途削除される (active resume state は run-dir 外でも force-init の対象)。
fn default_force_init_cleanup_paths(run_dir: &Path) -> Vec<PathBuf> {
    vec![
        default_param_values_csv_path(run_dir),
        default_stats_csv_path(run_dir),
        default_stats_aggregate_csv_path(run_dir),
    ]
}

fn default_meta_path(run_dir: &Path) -> PathBuf {
    run_dir.join("meta.json")
}

fn default_param_values_csv_path(run_dir: &Path) -> PathBuf {
    run_dir.join("values.csv")
}

fn default_stats_csv_path(run_dir: &Path) -> PathBuf {
    run_dir.join("stats.csv")
}

fn default_stats_aggregate_csv_path(run_dir: &Path) -> PathBuf {
    run_dir.join("stats_aggregate.csv")
}

fn schedule_matches(lhs: ScheduleConfig, rhs: ScheduleConfig) -> bool {
    const EPS: f64 = 1e-12;
    (lhs.alpha - rhs.alpha).abs() <= EPS
        && (lhs.gamma - rhs.gamma).abs() <= EPS
        && (lhs.a_ratio - rhs.a_ratio).abs() <= EPS
        && (lhs.mobility - rhs.mobility).abs() <= EPS
        && lhs.total_iterations == rhs.total_iterations
}

fn is_param_active(
    param: &SpsaParam,
    active_only_regex: Option<&Regex>,
    translator: &EngineNameTranslator,
) -> bool {
    if param.not_used {
        return false;
    }
    if let Some(re) = active_only_regex
        && !re.is_match(&param.name)
    {
        return false;
    }
    // P1: マッピング表がロード済みかつ name が未マッピングの場合、エンジン側で
    // setoption が黙ってスキップされるため SPSA で摂動・更新するのは無駄かつ有害
    // （unmapped.rshogi 系の値がランダムウォークして .params を汚染する）。
    // ここで active 集合から除外する。
    if translator.is_enabled() && !translator.is_mapped(&param.name) {
        return false;
    }
    true
}

fn format_param_value_for_csv(param: &SpsaParam) -> String {
    if param.is_int {
        format!("{}", param.value.round() as i64)
    } else {
        format!("{:.6}", param.value)
    }
}

fn write_stats_csv_header(writer: &mut BufWriter<File>) -> Result<()> {
    writeln!(
        writer,
        "iteration,seed,games,plus_wins,minus_wins,draws,raw_result,active_params,\
         avg_abs_shift,updated_params,avg_abs_update,max_abs_update,total_games"
    )?;
    Ok(())
}

fn write_stats_aggregate_csv_header(writer: &mut BufWriter<File>) -> Result<()> {
    writeln!(
        writer,
        "iteration,seeds,games_per_seed,raw_result_mean,raw_result_variance,\
         plus_wins_mean,plus_wins_variance,minus_wins_mean,minus_wins_variance,draws_mean,draws_variance,total_games"
    )?;
    Ok(())
}

fn write_param_values_csv_header(writer: &mut BufWriter<File>, params: &[SpsaParam]) -> Result<()> {
    write!(writer, "iteration")?;
    for param in params {
        write!(writer, ",{}", param.name)?;
    }
    writeln!(writer)?;
    Ok(())
}

/// CSV writer の出力先の親ディレクトリを必要に応じて作成する。
///
/// `--stats-csv subdir/foo.csv` のように override で深いパスを指定された
/// 場合、親 dir が存在しないと `open()` が失敗する。run-dir デフォルト経路
/// では `apply_init_action` が `--run-dir` を作成済みのため redundant だが、
/// override 経路でのみ意味がある (race-safe な idempotent 操作)。
fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent dir for {}", path.display()))?;
    }
    Ok(())
}

fn open_stats_csv_writer(path: &Path, resume: bool) -> Result<BufWriter<File>> {
    ensure_parent_dir(path)?;
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_stats_csv_header(&mut writer)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn open_stats_aggregate_csv_writer(path: &Path, resume: bool) -> Result<BufWriter<File>> {
    ensure_parent_dir(path)?;
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_stats_aggregate_csv_header(&mut writer)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn open_param_values_csv_writer(
    path: &Path,
    resume: bool,
    params: &[SpsaParam],
) -> Result<BufWriter<File>> {
    ensure_parent_dir(path)?;
    let write_header = if resume {
        if !path.exists() {
            true
        } else {
            std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .len()
                == 0
        }
    } else {
        true
    };
    let file = if resume {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {} for append", path.display()))?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?
    };
    let mut writer = BufWriter::new(file);
    if write_header {
        write_param_values_csv_header(&mut writer, params)?;
        writer.flush()?;
    }
    Ok(writer)
}

fn write_stats_csv_row(writer: &mut BufWriter<File>, stats: IterationStats) -> Result<()> {
    writeln!(
        writer,
        "{},{},{},{},{},{},{:+.6},{},{:.6},{},{:.6},{:.6},{}",
        stats.iteration,
        stats.seed,
        stats.games,
        stats.plus_wins,
        stats.minus_wins,
        stats.draws,
        stats.raw_result,
        stats.active_params,
        stats.avg_abs_shift,
        stats.updated_params,
        stats.avg_abs_update,
        stats.max_abs_update,
        stats.total_games
    )?;
    Ok(())
}

fn write_stats_aggregate_csv_row(
    writer: &mut BufWriter<File>,
    stats: AggregateIterationStats,
) -> Result<()> {
    writeln!(
        writer,
        "{},{},{},{:+.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{}",
        stats.iteration,
        stats.seed_count,
        stats.games_per_seed,
        stats.raw_result_mean,
        stats.raw_result_variance,
        stats.plus_wins_mean,
        stats.plus_wins_variance,
        stats.minus_wins_mean,
        stats.minus_wins_variance,
        stats.draws_mean,
        stats.draws_variance,
        stats.total_games
    )?;
    Ok(())
}

fn write_param_values_csv_row(
    writer: &mut BufWriter<File>,
    iteration: u32,
    params: &[SpsaParam],
) -> Result<()> {
    write!(writer, "{iteration}")?;
    for param in params {
        write!(writer, ",{}", format_param_value_for_csv(param))?;
    }
    writeln!(writer)?;
    Ok(())
}

fn load_meta(path: &Path) -> Result<ResumeMetaData> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let meta = serde_json::from_reader(reader)
        .with_context(|| format!("failed to parse JSON {}", path.display()))?;
    Ok(meta)
}

/// `meta.json` を atomic に保存する (temp file + rename)。
///
/// 注意: serde_json::to_writer_pretty は内部で flush しないため、明示的に
/// `BufWriter::flush()` を呼ぶ必要がある (Drop での自動 flush は失敗を握り潰す)。
/// さらに `std::fs::rename` は同一 filesystem 内で atomic なので、書き込み途中の
/// クラッシュで既存 meta が破損するのを防げる。
fn save_meta(path: &Path, meta: &ResumeMetaData) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".spsa_meta_")
        .suffix(".json.tmp")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temp file under {}", parent.display()))?;
    {
        let mut writer = BufWriter::new(tmp.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, meta)
            .with_context(|| format!("failed to write JSON {}", path.display()))?;
        writer
            .flush()
            .with_context(|| format!("failed to flush meta writer for {}", path.display()))?;
    }
    tmp.persist(path)
        .with_context(|| format!("failed to atomic-rename meta to {}", path.display()))?;
    Ok(())
}

// =============================================================================
// init-from 安全性: 状態遷移と検証ヘルパ (v3 新設)
// =============================================================================

/// `--init-from` / `<run-dir>/state.params` の有無 / `--resume` / `--force-init` の
/// 4 引数から起動時に取るべき動作を一意に決める純粋関数。
///
/// テスト容易性のため副作用 (FS 操作 / println) を持たない。実 dispatch は
/// `apply_init_action` 側で行う。
#[derive(Clone, Debug, PartialEq, Eq)]
enum InitAction {
    /// `--init-from` を `<run-dir>/state.params` にコピーして fresh start。
    /// (state 不在 + init-from 指定 + !resume + !force-init)
    CopyInitFromFresh,
    /// 既存 `<run-dir>/state.params` をそのまま fresh start で使う。
    /// (state 存在 + init-from なし + !resume + !force-init)
    UseExistingFresh,
    /// 既存 `<run-dir>/state.params` で resume 継続。
    /// (state 存在 + resume 指定。init-from は整合性検証にのみ使う)
    Resume { verify_init: bool },
    /// 既存 `<run-dir>/state.params` を atomic に上書きして fresh start (init-from 強制適用)。
    /// (state 存在 + init-from 指定 + force-init + !resume)
    ForceInitOverwrite,
    /// 設定エラーで bail。
    Bail(InitError),
}

/// `apply_init_action` 通過後の確定モード。`Bail` を排除した narrow type で
/// main 側の match から `unreachable!` を消すために使う。
#[derive(Clone, Debug, PartialEq, Eq)]
enum NonBailAction {
    CopyInitFromFresh,
    UseExistingFresh,
    Resume { verify_init: bool },
    ForceInitOverwrite,
}

impl NonBailAction {
    fn from_init_action(a: &InitAction) -> Option<Self> {
        match a {
            InitAction::CopyInitFromFresh => Some(Self::CopyInitFromFresh),
            InitAction::UseExistingFresh => Some(Self::UseExistingFresh),
            InitAction::Resume { verify_init } => Some(Self::Resume {
                verify_init: *verify_init,
            }),
            InitAction::ForceInitOverwrite => Some(Self::ForceInitOverwrite),
            InitAction::Bail(_) => None,
        }
    }

    fn init_mode(&self) -> InitMode {
        match self {
            Self::CopyInitFromFresh => InitMode::FreshInitFrom,
            Self::UseExistingFresh => InitMode::FreshExisting,
            Self::ForceInitOverwrite => InitMode::ForceInit,
            Self::Resume { .. } => InitMode::Resume,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InitError {
    /// `--init-from` 指定済み + `<run-dir>/state.params` 既存 + `--resume` も `--force-init` もなし。
    InitFromExistsRequiresFlag,
    /// `--resume` と `--force-init` は意味が矛盾するため同時指定不可。
    ResumeForceInitConflict,
    /// `--resume` 指定だが `<run-dir>/state.params` が存在しない。
    ResumeRequiresExistingParams,
    /// `--force-init` 指定だが `--init-from` が指定されていない。
    ForceInitRequiresInitFrom,
    /// `--force-init` 指定だが `<run-dir>/state.params` が存在しない (上書き対象がない)。
    ForceInitRequiresExistingParams,
    /// `<run-dir>/state.params` 不在 + `--init-from` なし + `--resume` なし。
    NoInitNorExistingParams,
    /// 既存 `<run-dir>/state.params` あり + `--init-from` / `--resume` / `--force-init`
    /// すべてなし。silent な fresh start は事故の温床のため明示フラグを要求する。
    UseExistingRequiresFlag,
    /// `--use-existing-state-as-init` が他のフラグ (`--init-from` / `--resume` /
    /// `--force-init`) と同時指定された。
    UseExistingConflictsWithOtherFlags,
}

impl InitError {
    fn message(&self) -> String {
        match self {
            Self::InitFromExistsRequiresFlag => {
                "--init-from が指定されていますが <run-dir>/state.params は既に存在します。\n\
                 意図に応じて以下のいずれかを指定してください:\n  \
                 --resume     : 既存 state から続行 (--init-from は内容検証にのみ使用)\n  \
                 --force-init : 既存 state を atomic 上書きして --init-from から再初期化\n  \
                 または --run-dir に新規 timestamped dir を指定する"
                    .to_owned()
            }
            Self::ResumeForceInitConflict => {
                "--resume と --force-init は同時指定できません (意味が矛盾します)。\n\
                 - 継続実行したい → --resume のみ\n\
                 - 既存を破棄して再初期化したい → --force-init のみ"
                    .to_owned()
            }
            Self::ResumeRequiresExistingParams => {
                "--resume が指定されていますが <run-dir>/state.params が存在しません。\n\
                 fresh start したい場合は --resume を外してください。"
                    .to_owned()
            }
            Self::ForceInitRequiresInitFrom => {
                "--force-init には --init-from の指定が必須です (上書き元が必要)。".to_owned()
            }
            Self::ForceInitRequiresExistingParams => {
                "--force-init は既存の <run-dir>/state.params を上書きする操作ですが、対象ファイルがありません。\n\
                 fresh start なら --force-init を外して --init-from だけで起動してください。"
                    .to_owned()
            }
            Self::NoInitNorExistingParams => {
                "<run-dir>/state.params が存在せず --init-from も指定されていません。\n\
                 --init-from で canonical (起点) ファイルを指定してください。"
                    .to_owned()
            }
            Self::UseExistingRequiresFlag => {
                "<run-dir>/state.params が既に存在しますが --init-from / --resume / --force-init / --use-existing-state-as-init のいずれも指定されていません。\n\
                 意図に応じて以下のいずれかを指定してください:\n  \
                 --init-from CANON --force-init      : 既存 state を canonical で atomic 上書き再初期化\n  \
                 --resume                            : 既存 state から続行 (推奨経路)\n  \
                 --use-existing-state-as-init        : 既存 state を canonical 代わりに fresh start (特殊用途)"
                    .to_owned()
            }
            Self::UseExistingConflictsWithOtherFlags => {
                "--use-existing-state-as-init は --init-from / --resume / --force-init と同時指定できません。\n\
                 これらは「state.params をどう用意するか」の意思表示が排他的に重なるためです。\n\
                 既存 state をそのまま起点にしたいなら --use-existing-state-as-init のみ指定してください。"
                    .to_owned()
            }
        }
    }
}

/// 純粋関数: CLI フラグと FS 状態 (params 存在性) から `InitAction` を決定する。
///
/// 入力 5 boolean (32 通り)。`use_existing_state_as_init` は他フラグと排他的
/// 意思表示として、true 時は他フラグ全て false でなければ bail する。
fn decide_init_action(
    has_init_from: bool,
    params_exists: bool,
    resume: bool,
    force_init: bool,
    use_existing_state_as_init: bool,
) -> InitAction {
    use InitAction::*;
    use InitError::*;

    // フラグ間の矛盾を最優先で弾く
    if resume && force_init {
        return Bail(ResumeForceInitConflict);
    }
    if force_init && !has_init_from {
        return Bail(ForceInitRequiresInitFrom);
    }
    // --use-existing-state-as-init は他の意思表示フラグと排他
    if use_existing_state_as_init && (has_init_from || resume || force_init) {
        return Bail(UseExistingConflictsWithOtherFlags);
    }
    // --use-existing-state-as-init は state.params が無いと意味がない
    if use_existing_state_as_init && !params_exists {
        return Bail(NoInitNorExistingParams);
    }
    // resume は params 必須 (force_init との矛盾は上で除去済み)
    if resume && !params_exists {
        return Bail(ResumeRequiresExistingParams);
    }
    // 通常分岐 (この時点で use_existing_state_as_init=true なら他フラグは全て false かつ params_exists=true)
    if use_existing_state_as_init {
        return UseExistingFresh;
    }
    match (has_init_from, params_exists, resume, force_init) {
        // resume
        (true, true, true, false) => Resume { verify_init: true },
        (false, true, true, false) => Resume { verify_init: false },
        // force-init
        (true, true, false, true) => ForceInitOverwrite,
        (true, false, false, true) => Bail(ForceInitRequiresExistingParams),
        // 通常
        (true, false, false, false) => CopyInitFromFresh,
        (true, true, false, false) => Bail(InitFromExistsRequiresFlag),
        (false, true, false, false) => Bail(UseExistingRequiresFlag),
        (false, false, false, false) => Bail(NoInitNorExistingParams),
        // 上のガードで除去済みの組み合わせ (型システム上 unreachable)
        _ => unreachable!("decide_init_action: invariant violated by guards above"),
    }
}

/// SHA-256 hex (lowercase) を計算する。ファイル全体を一度に読む。
fn sha256_hex_of_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read for hash: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// param 名集合の hash。sort 済みで決定的。
///
/// **前提**: name に改行 `\n` を含まないこと。`spsa_param_mapping::parse_param_line`
/// が CSV 1 行 1 param で読み込むため、現状この前提は parse 段階で実質保証されている。
/// 将来 parse 経路を変える場合、この関数も区切り文字を `\0` 等に変更すること
/// (改行混入時に異なる名前集合が同じ hash を返す可能性があるため)。
///
/// debug ビルドでは `\n` 含有を `debug_assert!` で検知し、parse 経路変更時の
/// regression を test 段階で捕捉する。release ビルドではコストゼロ。
fn param_name_set_sha256(params: &[SpsaParam]) -> String {
    let mut names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    let mut hasher = Sha256::new();
    for n in &names {
        debug_assert!(
            !n.contains('\n'),
            "param name must not contain '\\n' (would corrupt name-set hash): {n:?}"
        );
        hasher.update(n.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// `--init-from` の内容と既存 `<run-dir>/state.params` の値列を比較し、診断結果を返す。
///
/// resume 時に「想定の canonical で開始した run を resume しているか」の検証に使う。
/// `--strict-init-check` 時は閾値超過で error、デフォルトでは warning に留める。
#[derive(Debug)]
struct InitMatchReport {
    total: usize,
    matched_within_half_step: usize,
    median_step_units: f64,
    max_step_units: f64,
    extra_in_init: Vec<String>,
    missing_in_init: Vec<String>,
    top_diffs: Vec<(String, f64, f64, f64)>, // (name, init_v, existing_v, |Δ|/step)
}

fn verify_init_matches_existing(init_path: &Path, existing_path: &Path) -> Result<InitMatchReport> {
    let init_params = read_params(init_path)
        .with_context(|| format!("verify: failed to read init-from {}", init_path.display()))?;
    let existing_params = read_params(existing_path)
        .with_context(|| format!("verify: failed to read existing {}", existing_path.display()))?;

    use std::collections::BTreeSet;
    let init_names: BTreeSet<&str> = init_params.iter().map(|p| p.name.as_str()).collect();
    let exist_names: BTreeSet<&str> = existing_params.iter().map(|p| p.name.as_str()).collect();
    let extra: Vec<String> = init_names.difference(&exist_names).map(|s| (*s).to_owned()).collect();
    let missing: Vec<String> =
        exist_names.difference(&init_names).map(|s| (*s).to_owned()).collect();

    let exist_by_name: HashMap<&str, &SpsaParam> =
        existing_params.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut diffs: Vec<(String, f64, f64, f64)> = Vec::new();
    for ip in &init_params {
        if let Some(ep) = exist_by_name.get(ip.name.as_str()) {
            // step は c_end (= 最終摂動幅) をそのまま使う。
            // c_end == 0 の防御的フォールバックのみ 1.0 に補正する。
            // (旧実装の `c_end.max(1.0)` は c_end < 1 のパラメータで σ を過小評価していた)
            let step = if ip.c_end > 0.0 { ip.c_end } else { 1.0 };
            let d = (ip.value - ep.value).abs() / step;
            if d.is_nan() {
                bail!(
                    "verify_init: NaN diff detected for param '{}' (init.value={} existing.value={} step={})",
                    ip.name,
                    ip.value,
                    ep.value,
                    step
                );
            }
            diffs.push((ip.name.clone(), ip.value, ep.value, d));
        }
    }
    let total = diffs.len();
    let matched = diffs.iter().filter(|(_, _, _, d)| *d < 0.5).count();
    let mut sorted_d: Vec<f64> = diffs.iter().map(|(_, _, _, d)| *d).collect();
    sorted_d.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // 厳密中央値: 偶数個のときは下側中値と上側中値の平均を取る。
    // 旧実装は `sorted_d[n/2]` で常に上側中値を返しており、--strict-init-check の
    // 閾値 (0.5σ) 判定をわずかに過大評価していた。
    let median = match sorted_d.len() {
        0 => 0.0,
        n if n.is_multiple_of(2) => (sorted_d[n / 2 - 1] + sorted_d[n / 2]) / 2.0,
        n => sorted_d[n / 2],
    };
    let max = sorted_d.iter().copied().fold(0.0_f64, f64::max);

    // 上位 5 件の乖離を抽出 (大きい順)
    let mut top = diffs.clone();
    top.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    top.truncate(5);

    Ok(InitMatchReport {
        total,
        matched_within_half_step: matched,
        median_step_units: median,
        max_step_units: max,
        extra_in_init: extra,
        missing_in_init: missing,
        top_diffs: top,
    })
}

impl InitMatchReport {
    /// strict mode で bail すべきか判定。median ≥ 0.5 step または max ≥ 5 step で true。
    fn exceeds_strict_threshold(&self) -> bool {
        self.median_step_units >= 0.5 || self.max_step_units >= 5.0
    }

    /// 名前集合に差異があるかどうか。
    fn has_name_set_mismatch(&self) -> bool {
        !self.extra_in_init.is_empty() || !self.missing_in_init.is_empty()
    }

    /// 整合性の人間可読サマリを stderr に出す。
    fn print_summary(&self, init_path: &Path, existing_path: &Path) {
        eprintln!(
            "init-from 整合性チェック: init={} vs existing={}",
            init_path.display(),
            existing_path.display()
        );
        eprintln!(
            "  名前一致: {} (init側にしかない: {}, existing側にしかない: {})",
            self.total,
            self.extra_in_init.len(),
            self.missing_in_init.len()
        );
        eprintln!(
            "  値整合性 (|Δ|/step): median={:.3}σ, max={:.3}σ, <0.5σ 一致率={}/{}",
            self.median_step_units, self.max_step_units, self.matched_within_half_step, self.total
        );
        if !self.top_diffs.is_empty() && self.max_step_units >= 0.5 {
            eprintln!("  上位乖離 (最大 5 件):");
            for (name, iv, ev, d) in &self.top_diffs {
                eprintln!("    {name}: init={iv:.3} existing={ev:.3} |Δ|/step={d:.3}σ");
            }
        }
    }
}

/// `decide_init_action` の結果を実際に FS に反映するヘルパ。
///
/// 副作用: ファイル copy / atomic overwrite / 関連 (meta / CSV) の削除。
/// `force_init` 時は **削除を先に行い** (失敗時は bail)、その後 params を atomic copy
/// する。順序が逆だと「新 params + 旧 meta」の不整合 run dir が中断時に残り、
/// 次回 resume で `completed_iterations` 等が古いまま継ぎ足される事故になる。
///
/// 戻り値: `Bail` を排除した `NonBailAction`。呼び出し側の `match` から
/// `unreachable!` 分岐を消せる。
fn apply_init_action(
    action: &InitAction,
    init_from: Option<&Path>,
    params_path: &Path,
    meta_path: &Path,
    related_csv_paths: &[&Path],
) -> Result<NonBailAction> {
    match action {
        InitAction::CopyInitFromFresh => {
            let src = init_from.expect("CopyInitFromFresh requires init_from");
            atomic_copy_file(src, params_path)?;
            eprintln!(
                "init-from: copied {} -> {} (fresh start)",
                src.display(),
                params_path.display()
            );
        }
        InitAction::UseExistingFresh => {
            eprintln!(
                "init: using existing {} as fresh start (no --init-from, no --resume)",
                params_path.display()
            );
        }
        InitAction::Resume { .. } => {
            // resume 時は params をそのまま使う。verify_init は呼び出し側で実施。
            eprintln!("init: resuming from existing {}", params_path.display());
        }
        InitAction::ForceInitOverwrite => {
            let src = init_from.expect("ForceInitOverwrite requires init_from");
            // (1) meta を先に削除 (失敗で bail)。中断耐性のため atomic copy より前に行う。
            //     params を先に書くと「中断 → 新 params + 旧 meta」となり、次回 resume で
            //     completed_iterations が古いまま継ぎ足される事故が起きる。
            if meta_path.exists() {
                std::fs::remove_file(meta_path).with_context(|| {
                    format!("force-init: failed to remove stale meta {}", meta_path.display())
                })?;
            }
            // (2) related CSV を削除 (best-effort warn だが、削除に失敗するファイルは
            //     後段の append/truncate で再処理可能なので致命ではない)。
            for p in related_csv_paths {
                if p.exists()
                    && let Err(e) = std::fs::remove_file(p)
                {
                    eprintln!("warning: force-init: failed to remove stale {} ({e})", p.display());
                }
            }
            // (3) params を atomic copy で上書き (rename は同一 FS 内 atomic)。
            atomic_copy_file(src, params_path)?;
            eprintln!(
                "init-from: force-init overwrite {} -> {} (stale meta/CSV removed)",
                src.display(),
                params_path.display()
            );
        }
        InitAction::Bail(err) => bail!("init/resume 設定エラー: {}", err.message()),
    }
    // ここに到達するのは Bail 以外の 4 バリアント。`Bail` は上の match で `bail!` 早期 return
    // するため、`from_init_action` が `None` を返す経路は論理的に到達不能。
    // `unreachable!` でなく `expect` を使うのは、万が一バグで Bail が漏れたときに
    // 内部 invariant 違反として明示的に panic するため (anyhow::Error にせず fail-fast)。
    Ok(NonBailAction::from_init_action(action)
        .expect("invariant: Bail handled above; non-Bail variants always convertible"))
}

/// 起動時にしか変わらないメタフィールドのスナップショット。
///
/// fresh / force-init 時は `for_fresh_start` で計算、resume 時は `from_existing_meta`
/// で既存 meta から引き継ぐ。これにより resume が「最初に何で起動したか」の情報を
/// 失わずに保持できる。
#[derive(Clone, Debug)]
struct InitMetaSnapshot {
    init_params_sha256: String,
    init_from_sha256: Option<String>,
    init_from_path: Option<String>,
    engine_path: String,
    engine_param_mapping_path: Option<String>,
    engine_param_mapping_sha256: Option<String>,
    init_mode: InitMode,
}

impl InitMetaSnapshot {
    /// fresh / force-init 起動用に現在の状態から構築する。
    ///
    /// `Resume` バリアントは `from_existing_meta` を使うべきで、ここに渡したら
    /// プログラムバグなので panic で fail-fast する (silent な "unknown" 化を防ぐ)。
    fn for_fresh_start(
        action: &NonBailAction,
        params_path: &Path,
        init_from: Option<&Path>,
        engine_path: &Path,
        engine_param_mapping: Option<&Path>,
    ) -> Result<Self> {
        if matches!(action, NonBailAction::Resume { .. }) {
            unreachable!(
                "for_fresh_start should not be called with Resume; use from_existing_meta instead"
            );
        }
        let init_mode = action.init_mode();
        let init_params_sha256 = sha256_hex_of_file(params_path)?;
        let (init_from_sha256, init_from_path) = match init_from {
            Some(p) => (Some(sha256_hex_of_file(p)?), Some(p.display().to_string())),
            None => (None, None),
        };
        let (mapping_path, mapping_sha) = match engine_param_mapping {
            Some(p) => (Some(p.display().to_string()), Some(sha256_hex_of_file(p)?)),
            None => (None, None),
        };
        // TODO(PR2 / follow-up): engine_path / mapping_path は CLI 引数のままで
        // cwd 相対の場合がある。後追い解析で「どのバイナリで起動したか」を知るには
        // `std::fs::canonicalize` を通したい (失敗時は raw path にフォールバック)。
        Ok(Self {
            init_params_sha256,
            init_from_sha256,
            init_from_path,
            engine_path: engine_path.display().to_string(),
            engine_param_mapping_path: mapping_path,
            engine_param_mapping_sha256: mapping_sha,
            init_mode,
        })
    }

    /// resume 時に既存 meta から起動時情報を復元する。
    fn from_existing_meta(meta: &ResumeMetaData) -> Self {
        Self {
            init_params_sha256: meta.init_params_sha256.clone(),
            init_from_sha256: meta.init_from_sha256.clone(),
            init_from_path: meta.init_from_path.clone(),
            engine_path: meta.engine_path.clone(),
            engine_param_mapping_path: meta.engine_param_mapping_path.clone(),
            engine_param_mapping_sha256: meta.engine_param_mapping_sha256.clone(),
            init_mode: meta.init_mode,
        }
    }
}

/// `src` の内容を `dst` に atomic にコピーする (temp file + rename)。
///
/// 同一 filesystem 内なら rename は atomic なので、書き込み中のクラッシュで
/// `dst` が中途半端な状態になることを防ぐ。
///
/// **前提**:
/// - `dst.parent()` (or 親が空なら CWD) に書き込み権限と十分な inode/space が必要。
/// - 同一 FS 内 atomic を担保するため tempfile を `dst.parent()` 直下に作成する。
///   tmpfs/persist FS 跨ぎ (`/tmp` から `/mnt`) では `tempfile::persist` が
///   `EXDEV` で失敗する可能性がある (rename(2) の制約)。
/// - tempfile の permission は umask 由来 (通常 0600)。元 `dst` が group/world
///   readable だった場合、rename 後に permission が縮退する可能性がある。共有
///   FS 運用では呼び出し側で chmod 後処理を行うこと。
fn atomic_copy_file(src: &Path, dst: &Path) -> Result<()> {
    let parent = dst.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create parent dir for {}", dst.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".spsa_init_")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temp file under {}", parent.display()))?;
    {
        let mut reader = File::open(src)
            .with_context(|| format!("failed to open init source {}", src.display()))?;
        let mut writer = BufWriter::new(tmp.as_file_mut());
        std::io::copy(&mut reader, &mut writer)
            .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
        writer
            .flush()
            .with_context(|| format!("failed to flush copy writer for {}", dst.display()))?;
    }
    tmp.persist(dst)
        .with_context(|| format!("failed to atomic-rename to {}", dst.display()))?;
    Ok(())
}

impl SpsaParam {
    fn from_raw(raw: RawParamRow, line_no: usize) -> Result<Self> {
        let RawParamRow {
            name,
            kind,
            value_text,
            min_text,
            max_text,
            col5_text,
            col6_text,
            comment,
            not_used,
        } = raw;
        let is_int = kind.eq_ignore_ascii_case("int");
        Ok(SpsaParam {
            name,
            type_name: kind,
            is_int,
            value: value_text
                .parse::<f64>()
                .with_context(|| format!("invalid v at line {line_no}"))?,
            min: min_text
                .parse::<f64>()
                .with_context(|| format!("invalid min at line {line_no}"))?,
            max: max_text
                .parse::<f64>()
                .with_context(|| format!("invalid max at line {line_no}"))?,
            c_end: col5_text
                .parse::<f64>()
                .with_context(|| format!("invalid c_end at line {line_no}"))?,
            r_end: col6_text
                .parse::<f64>()
                .with_context(|| format!("invalid r_end at line {line_no}"))?,
            comment,
            not_used,
        })
    }
}

/// `<run-dir>/.lock` の中身。lock 衝突時にユーザが「誰が掴んでいるか」を
/// 判断するための forensic 情報。
#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    hostname: String,
    started_at_utc: String,
}

/// run-dir の排他 lock。`OpenOptions::create_new(true)` の atomic file
/// creation を使うので、同一 host の同一 FS 内でのみ有効 (NFS では
/// create_new の atomicity が保証されないため非推奨)。
///
/// 取得後は `Drop` で lock ファイルを削除する。panic 時も Drop は走るが、
/// SIGKILL / 電源断では残留する。残留 lock は `--force-unlock` で削除可能。
#[derive(Debug)]
struct RunDirLock {
    path: PathBuf,
}

impl RunDirLock {
    fn acquire(run_dir: &Path, force_unlock: bool) -> Result<Self> {
        let path = run_dir.join(".lock");
        if force_unlock && path.exists() {
            std::fs::remove_file(&path).with_context(|| {
                format!("failed to remove stale lock {} (--force-unlock)", path.display())
            })?;
            eprintln!("--force-unlock: 古い lock {} を削除しました", path.display());
        }
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(mut f) => {
                let info = LockInfo {
                    pid: std::process::id(),
                    hostname: read_hostname(),
                    started_at_utc: Utc::now().to_rfc3339(),
                };
                let body = serde_json::to_string(&info).context("failed to serialize lock info")?;
                writeln!(f, "{body}").with_context(|| {
                    format!("failed to write lock contents to {}", path.display())
                })?;
                f.flush().ok();
                Ok(RunDirLock { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let body = std::fs::read_to_string(&path).unwrap_or_else(|_| "(unreadable)".into());
                bail!(
                    "他プロセスが run-dir を使用中の可能性があります: {}\n  内容: {}\n  当該プロセスが既に死んでいることを目視確認したうえで --force-unlock を指定してください。",
                    path.display(),
                    body.trim()
                );
            }
            Err(e) => Err(anyhow::Error::new(e))
                .with_context(|| format!("failed to create lock {}", path.display())),
        }
    }
}

impl Drop for RunDirLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// hostname 取得。forensic 用途なので exact correctness より「何かしら名前が
/// 入る」ことを優先する。優先順: $HOSTNAME → /proc/sys/kernel/hostname →
/// "unknown"。
fn read_hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME")
        && !h.is_empty()
    {
        return h;
    }
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let trimmed = h.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "unknown".into()
}

fn read_params(path: &Path) -> Result<Vec<SpsaParam>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut params = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line?;
        if let Some(raw) = parse_param_line(&line, line_no)? {
            params.push(SpsaParam::from_raw(raw, line_no)?);
        }
    }
    if params.is_empty() {
        bail!("no parameters loaded from {}", path.display());
    }
    Ok(params)
}

/// state.params を tempfile + persist で atomic に書き込む。
///
/// 反復ごとに呼ばれるため、SIGINT / OOM / 電源断で truncate 中の壊れた
/// state.params が残ると resume 不能になる。`atomic_copy_file` と同じ
/// 「同一 FS 内 tempfile → flush → persist (rename)」パターンに統一する。
fn write_params(path: &Path, params: &[SpsaParam]) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create parent dir for {}", path.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".spsa_state_")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temp file under {}", parent.display()))?;
    {
        let mut w = BufWriter::new(tmp.as_file_mut());
        for p in params {
            // float は `{:.6}` で固定桁にしてラウンドトリップ・git diff の安定性を保つ
            // (`{}` (Display) は `1e-7` のような指数表記や精度不定の桁を出すため)
            let v_str = if p.is_int {
                format!("{}", p.value.round() as i64)
            } else {
                format!("{:.6}", p.value)
            };
            let mut line = format!(
                "{},{},{},{},{},{},{}",
                p.name, p.type_name, v_str, p.min, p.max, p.c_end, p.r_end
            );
            if !p.comment.is_empty() {
                line.push_str(" //");
                line.push_str(&p.comment);
            }
            if p.not_used {
                line.push_str(PARAM_NOT_USED_MARKER);
            }
            writeln!(w, "{line}")?;
        }
        w.flush()
            .with_context(|| format!("failed to flush state writer for {}", path.display()))?;
    }
    tmp.persist(path)
        .with_context(|| format!("failed to atomic-rename to {}", path.display()))?;
    Ok(())
}

fn option_value_string(param: &SpsaParam, value: f64) -> String {
    if param.is_int {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.6}")
    }
}

fn clamped_value(param: &SpsaParam, raw: f64) -> f64 {
    raw.clamp(param.min, param.max)
}

fn resolve_engine_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(path) = &cli.engine_path {
        return Ok(path.clone());
    }
    let release = PathBuf::from("target/release/rshogi-usi");
    if release.exists() {
        return Ok(release);
    }
    let debug = PathBuf::from("target/debug/rshogi-usi");
    if debug.exists() {
        return Ok(debug);
    }
    bail!("engine binary not found. specify --engine-path or build target/release/rshogi-usi");
}

fn apply_parameter_vector(
    engine: &mut EngineProcess,
    params: &[SpsaParam],
    values: &[f64],
    translator: &EngineNameTranslator,
    active_mask: &[bool],
) -> Result<()> {
    debug_assert_eq!(params.len(), values.len());
    debug_assert_eq!(params.len(), active_mask.len());
    for ((p, &v), &active) in params.iter().zip(values.iter()).zip(active_mask.iter()) {
        // 非 active (not_used / regex 不一致 / translator enabled & unmapped) は
        // engine 側で `set_option_if_available` が黙ってスキップする上、SPSA 側でも
        // 値が変わらないので毎ゲーム送信は無駄。
        if !active {
            continue;
        }
        let (engine_name, engine_value) = translator.translate(&p.name, v);
        // `engine_value` は translator で sign_flip 後の値。SPSA 側の clamp は呼び出し
        // 元 (`update_parameter_vector`) で `p.min/max` 適用済みなので、ここで再 clamp
        // しない。translator は名前と符号だけを変換する役割で、YO 側 USI option の
        // min/max との整合性は運用責任（runbook §10.6 + check_param_mapping --yo-binary
        // で事前検証する想定）。
        engine.set_option_if_available(engine_name, &option_value_string(p, engine_value))?;
    }
    engine.sync_ready()?;
    Ok(())
}

fn plus_score_from_outcome(outcome: GameOutcome, plus_is_black: bool) -> f64 {
    match outcome {
        GameOutcome::Draw | GameOutcome::InProgress => 0.0,
        GameOutcome::BlackWin => {
            if plus_is_black {
                1.0
            } else {
                -1.0
            }
        }
        GameOutcome::WhiteWin => {
            if plus_is_black {
                -1.0
            } else {
                1.0
            }
        }
    }
}

fn pick_startpos_index(
    start_positions_len: usize,
    rng: &mut impl rand::Rng,
    random: bool,
    game_index: usize,
) -> Result<usize> {
    if start_positions_len == 0 {
        bail!("no start positions available");
    }
    if random {
        Ok(rng.random_range(0..start_positions_len))
    } else {
        Ok(game_index % start_positions_len)
    }
}

fn resolve_seeds(cli: &Cli) -> Vec<u64> {
    if let Some(seeds) = &cli.seeds {
        return seeds.clone();
    }
    if let Some(seed) = cli.seed {
        return vec![seed];
    }
    let mut rng = rand::rng();
    vec![rng.random()]
}

fn mean_and_variance(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().copied().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| {
            let diff = value - mean;
            diff * diff
        })
        .sum::<f64>()
        / values.len() as f64;
    (mean, variance)
}

/// `JoinHandle::join` の panic payload から人間可読なメッセージを抽出する。
/// `panic!` に渡された値が `&'static str` か `String` の典型ケースのみ拾い、
/// それ以外は型情報のみのプレースホルダを返す。
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<panic payload of unknown type>".to_string()
    }
}

fn seed_for_iteration(base_seed: u64, iteration_index: u32) -> u64 {
    let iter_term = (iteration_index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    base_seed ^ iter_term
}

/// `compute_seed_prep` のセッション定数バンドル（iter 内で全 seed 共通）。
///
/// 引数増加によるシグネチャ複雑化を避けるため `compute_seed_prep` の入力をまとめる。
struct SeedPrepCtx<'a> {
    big_a: f64,
    schedule: ScheduleConfig,
    params: &'a [SpsaParam],
    param_schedules: &'a [ParamScheduleConstants],
    active_only_regex: Option<&'a Regex>,
    translator: &'a EngineNameTranslator,
    start_positions_len: usize,
    games_per_iteration: usize,
    random_startpos: bool,
}

/// 1 seed × 1 iter 分の事前計算（RNG / flips / shifts / plus/minus / startpos インデックス）。
///
/// 各 seed 独立に決定論的に計算可能なため、後段の重いゲーム実行を seed 並列化する際の
/// 前処理として使う。`seed_total_games_start` は startpos の cyclic indexing にのみ使われ、
/// 並列実行時はセッション累積 + `seed_idx * games_per_iter` で seed 間の重複を避ける。
fn compute_seed_prep(
    ctx: &SeedPrepCtx<'_>,
    iter: u32,
    base_seed: u64,
    seed_total_games_start: usize,
) -> Result<SeedPrep> {
    let iter_seed = seed_for_iteration(base_seed, iter);
    let mut rng = ChaCha8Rng::seed_from_u64(iter_seed);

    // Per-param Fishtest 摂動: shift_j = c_k_j × flip_j
    let flips: Vec<f64> = ctx
        .params
        .iter()
        .map(|p| {
            if !is_param_active(p, ctx.active_only_regex, ctx.translator) {
                0.0
            } else if rng.random_bool(0.5) {
                1.0
            } else {
                -1.0
            }
        })
        .collect();
    let shifts: Vec<f64> = ctx
        .params
        .iter()
        .zip(ctx.param_schedules.iter())
        .zip(flips.iter())
        .map(|((p, sched), &flip)| {
            if !is_param_active(p, ctx.active_only_regex, ctx.translator) {
                0.0
            } else {
                let (c_k, _) =
                    sched.at_iteration(iter, ctx.big_a, ctx.schedule.alpha, ctx.schedule.gamma);
                c_k * flip
            }
        })
        .collect();
    let plus_values: Vec<f64> = ctx
        .params
        .iter()
        .zip(shifts.iter())
        .map(|(p, s)| clamped_value(p, p.value + s))
        .collect();
    let minus_values: Vec<f64> = ctx
        .params
        .iter()
        .zip(shifts.iter())
        .map(|(p, s)| clamped_value(p, p.value - s))
        .collect();

    let mut active_params = 0usize;
    let mut abs_shift_sum = 0.0f64;
    for (p, &shift) in ctx.params.iter().zip(shifts.iter()) {
        if !is_param_active(p, ctx.active_only_regex, ctx.translator) {
            continue;
        }
        active_params += 1;
        abs_shift_sum += shift.abs();
    }
    let avg_abs_shift = if active_params > 0 {
        abs_shift_sum / active_params as f64
    } else {
        0.0
    };

    let mut start_pos_indices = Vec::with_capacity(ctx.games_per_iteration);
    for game_idx in 0..ctx.games_per_iteration {
        start_pos_indices.push(pick_startpos_index(
            ctx.start_positions_len,
            &mut rng,
            ctx.random_startpos,
            seed_total_games_start + game_idx,
        )?);
    }

    Ok(SeedPrep {
        base_seed,
        flips,
        plus_values,
        minus_values,
        start_pos_indices,
        active_params,
        avg_abs_shift,
        seed_total_games_start,
    })
}

fn duplicate_engine_config(cfg: &EngineConfig) -> EngineConfig {
    EngineConfig {
        path: cfg.path.clone(),
        args: cfg.args.clone(),
        threads: cfg.threads,
        hash_mb: cfg.hash_mb,
        network_delay: cfg.network_delay,
        network_delay2: cfg.network_delay2,
        minimum_thinking_time: cfg.minimum_thinking_time,
        slowmover: cfg.slowmover,
        ponder: cfg.ponder,
        usi_options: cfg.usi_options.clone(),
    }
}

fn run_seed_games_parallel(ctx: SeedRunContext<'_>) -> Result<SeedGameStats> {
    let SeedRunContext {
        concurrency,
        base_cfg,
        params,
        plus_values,
        minus_values,
        start_positions,
        start_pos_indices,
        game_cfg,
        tc,
        total_games_start,
        iteration,
        seed_idx,
        seed_count,
        base_seed,
        translator,
        active_mask,
    } = ctx;

    let game_count = start_pos_indices.len();
    if game_count == 0 {
        return Ok(SeedGameStats {
            step_sum: 0.0,
            plus_wins: 0,
            minus_wins: 0,
            draws: 0,
        });
    }
    let worker_count = concurrency.clamp(1, game_count);
    let (task_tx, task_rx) = unbounded::<GameTask>();
    let (result_tx, result_rx) = unbounded::<Result<GameTaskResult>>();

    std::thread::scope(|scope| -> Result<SeedGameStats> {
        for worker_idx in 0..worker_count {
            let task_rx = task_rx.clone();
            let result_tx = result_tx.clone();
            let worker_cfg = duplicate_engine_config(base_cfg);
            let worker_label = format!("seed{}_worker{}", seed_idx + 1, worker_idx + 1);
            scope.spawn(move || {
                let mut plus_engine =
                    match EngineProcess::spawn(&worker_cfg, format!("plus_{worker_label}")) {
                        Ok(engine) => engine,
                        Err(err) => {
                            let _ = result_tx.send(Err(err));
                            return;
                        }
                    };
                let mut minus_engine =
                    match EngineProcess::spawn(&worker_cfg, format!("minus_{worker_label}")) {
                        Ok(engine) => engine,
                        Err(err) => {
                            let _ = result_tx.send(Err(err));
                            return;
                        }
                    };
                for task in task_rx {
                    let result = (|| -> Result<GameTaskResult> {
                        if task.plus_is_black {
                            apply_parameter_vector(
                                &mut plus_engine,
                                params,
                                plus_values,
                                translator,
                                active_mask,
                            )?;
                            apply_parameter_vector(
                                &mut minus_engine,
                                params,
                                minus_values,
                                translator,
                                active_mask,
                            )?;
                        } else {
                            apply_parameter_vector(
                                &mut plus_engine,
                                params,
                                minus_values,
                                translator,
                                active_mask,
                            )?;
                            apply_parameter_vector(
                                &mut minus_engine,
                                params,
                                plus_values,
                                translator,
                                active_mask,
                            )?;
                        }
                        plus_engine.new_game()?;
                        minus_engine.new_game()?;

                        let start_pos = &start_positions[task.start_pos_index];
                        let mut on_move = |_event: &MoveEvent| {};
                        let result = if task.plus_is_black {
                            run_game(
                                &mut plus_engine,
                                &mut minus_engine,
                                start_pos,
                                tc,
                                game_cfg,
                                task.game_id,
                                &mut on_move,
                                None,
                            )?
                        } else {
                            run_game(
                                &mut minus_engine,
                                &mut plus_engine,
                                start_pos,
                                tc,
                                game_cfg,
                                task.game_id,
                                &mut on_move,
                                None,
                            )?
                        };
                        let plus_score =
                            plus_score_from_outcome(result.outcome, task.plus_is_black);
                        Ok(GameTaskResult {
                            game_idx: task.game_idx,
                            plus_is_black: task.plus_is_black,
                            plus_score,
                            outcome: result.outcome,
                        })
                    })();
                    if result_tx.send(result).is_err() {
                        break;
                    }
                }
            });
        }
        drop(task_rx);
        drop(result_tx);

        for (idx, &start_pos_index) in start_pos_indices.iter().enumerate() {
            let game_idx = u32::try_from(idx).context("game index overflow")?;
            let game_id = u32::try_from(total_games_start + idx + 1).context("game id overflow")?;
            task_tx
                .send(GameTask {
                    game_idx,
                    plus_is_black: idx % 2 == 0,
                    start_pos_index,
                    game_id,
                })
                .context("failed to dispatch game task")?;
        }
        drop(task_tx);

        let mut step_sum = 0.0f64;
        let mut plus_wins = 0u32;
        let mut minus_wins = 0u32;
        let mut draws = 0u32;

        for _ in 0..game_count {
            let result =
                result_rx.recv().context("failed to receive game result from worker")??;
            step_sum += result.plus_score;
            if result.plus_score > 0.0 {
                plus_wins += 1;
            } else if result.plus_score < 0.0 {
                minus_wins += 1;
            } else {
                draws += 1;
            }
            eprintln!(
                "iter={} seed={}/{}({}) game={}/{} plus_is_black={} outcome={} plus_score={:+.1}",
                iteration,
                seed_idx + 1,
                seed_count,
                base_seed,
                result.game_idx + 1,
                game_count,
                result.plus_is_black,
                result.outcome.label(),
                result.plus_score
            );
        }

        Ok(SeedGameStats {
            step_sum,
            plus_wins,
            minus_wins,
            draws,
        })
    })
}

/// `print_startup_summary` の入力をまとめた構造体。位置引数の取り違えを防ぎ、
/// 将来項目を増やしても呼び出し側の修正が小さくなる。
struct StartupContext<'a> {
    snapshot: &'a InitMetaSnapshot,
    schedule: &'a ScheduleConfig,
    params: &'a [SpsaParam],
    active_mask: &'a [bool],
    active_param_count: usize,
    start_iteration: u32,
    end_iteration: u32,
    seed_values: &'a [u64],
    params_path: &'a Path,
    meta_path: &'a Path,
}

/// scalar (i32 想定の f64) を `is_int` に応じて整形する小ヘルパ。`frac` は
/// 浮動小数時の有効桁。startup summary 内の value/min/max を統一表記するため。
fn fmt_param_scalar(p: &SpsaParam, v: f64, frac: usize) -> String {
    if p.is_int {
        format!("{}", v.round() as i64)
    } else {
        format!("{:.*}", frac, v)
    }
}

/// SPSA 起動時に「どんな状態で SPSA を始めるのか」を 1 ブロックで stderr に出力する。
///
/// 「init mode が想定通り」「active params 上位 5 件が想定値」を起動 5 秒で目視確認
/// できる形にすることで、誤った canonical を投入したまま長時間 run を回す
/// 事故への二度目の予防線とする。出力先は stderr なので CSV パイプ運用
/// (`spsa | tee log.csv`) を阻害しない。
fn print_startup_summary(ctx: &StartupContext<'_>) {
    eprintln!("=== SPSA Startup Summary ===");
    eprintln!("init mode:      {}", ctx.snapshot.init_mode);
    eprintln!("params:         {}", ctx.params_path.display());
    eprintln!("meta:           {}", ctx.meta_path.display());
    eprintln!("params sha256:  {} (起動時スナップショット)", ctx.snapshot.init_params_sha256);
    if let (Some(p), Some(h)) =
        (ctx.snapshot.init_from_path.as_deref(), ctx.snapshot.init_from_sha256.as_deref())
    {
        eprintln!("--init-from:    {p} (sha256: {h})");
    } else {
        eprintln!("--init-from:    (none)");
    }
    eprintln!("engine:         {}", ctx.snapshot.engine_path);
    if let Some(p) = ctx.snapshot.engine_param_mapping_path.as_deref() {
        let h = ctx.snapshot.engine_param_mapping_sha256.as_deref().unwrap_or("?");
        eprintln!("mapping:        {p} (sha256: {h})");
    }
    eprintln!(
        "schedule:       α={} γ={} a_ratio={} mobility={} total_iter={}",
        ctx.schedule.alpha,
        ctx.schedule.gamma,
        ctx.schedule.a_ratio,
        ctx.schedule.mobility,
        ctx.schedule.total_iterations
    );
    eprintln!(
        "iteration plan: {} → {} ({} new iter), seeds={:?}",
        ctx.start_iteration,
        ctx.end_iteration,
        ctx.end_iteration.saturating_sub(ctx.start_iteration),
        ctx.seed_values
    );
    eprintln!("active params:  {}/{}", ctx.active_param_count, ctx.params.len());

    // 起動時 active params の上位 5 件を表示。
    // active_mask を使うことで「active_only_regex / mapping translator で除外された
    // param が誤って summary に出る」のを防ぐ (not_used フィルタだけだと不十分)。
    let preview: Vec<&SpsaParam> = ctx
        .params
        .iter()
        .zip(ctx.active_mask.iter())
        .filter(|(_, a)| **a)
        .map(|(p, _)| p)
        .take(5)
        .collect();
    if !preview.is_empty() {
        eprintln!("starting values (first 5 of {} active params):", ctx.active_param_count);
        for p in preview {
            eprintln!(
                "  {:<48} = {:>10} (range [{}, {}], step {})",
                p.name,
                fmt_param_scalar(p, p.value, 4),
                fmt_param_scalar(p, p.min, 2),
                fmt_param_scalar(p, p.max, 2),
                p.c_end
            );
        }
    }
    eprintln!("=== End Summary ===");
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stderr)
        .init();

    let cli = Cli::parse();
    if cli.games_per_iteration == 0 || cli.games_per_iteration % 2 != 0 {
        bail!("--games-per-iteration must be an even number >= 2");
    }
    if cli.iterations == 0 {
        bail!("--iterations must be >= 1");
    }
    if cli.concurrency == 0 {
        bail!("--concurrency must be >= 1");
    }
    if cli.alpha <= 0.0 || cli.gamma <= 0.0 {
        bail!("--alpha and --gamma must be > 0");
    }
    if cli.a_ratio < 0.0 {
        bail!("--a-ratio must be >= 0");
    }
    if let Some(v) = cli.early_stop_avg_abs_update_threshold
        && v < 0.0
    {
        bail!("--early-stop-avg-abs-update-threshold must be >= 0");
    }
    if let Some(v) = cli.early_stop_result_variance_threshold
        && v < 0.0
    {
        bail!("--early-stop-result-variance-threshold must be >= 0");
    }
    let early_stop_config = match (
        cli.early_stop_avg_abs_update_threshold,
        cli.early_stop_result_variance_threshold,
        cli.early_stop_patience,
    ) {
        (None, None, 0) => None,
        (Some(avg), Some(var), patience) if patience > 0 => Some(EarlyStopConfig {
            avg_abs_update_threshold: avg,
            result_variance_threshold: var,
            patience,
        }),
        _ => {
            bail!(
                "early stopを有効化するには \
                 --early-stop-avg-abs-update-threshold, \
                 --early-stop-result-variance-threshold, \
                 --early-stop-patience(>0) を全て指定してください"
            );
        }
    };

    let active_only_regex = cli
        .active_only_regex
        .as_deref()
        .map(Regex::new)
        .transpose()
        .context("invalid --active-only-regex")?;
    let seed_values = resolve_seeds(&cli);
    if seed_values.is_empty() {
        bail!("at least one seed is required");
    }
    eprintln!("using base seeds: {:?}", seed_values);

    // --parallel-seeds 指定時、`cli.concurrency` が seed 数で割り切れない、または
    // 1 seed あたりの worker 数が `games_per_iteration` を超える設定だと実効並列度が
    // 指定値より下がる（`run_seed_games_parallel` 側で `worker_count.clamp(1, game_count)`
    // の clamp が効くため）。vast.ai 等で `--concurrency` を盛ったときに気付けないと
    // 単純に CPU が余るので、起動時に一度だけ警告して気付かせる。
    if cli.parallel_seeds && seed_values.len() >= 2 {
        let per_seed_concurrency = (cli.concurrency / seed_values.len()).max(1);
        let games_per_iter = cli.games_per_iteration as usize;
        let effective_per_seed = per_seed_concurrency.min(games_per_iter);
        let effective_total = effective_per_seed * seed_values.len();
        if effective_total < cli.concurrency {
            eprintln!(
                "warning: --parallel-seeds の実効並列度が --concurrency より低い \
                 (concurrency={}, seeds={}, games_per_iteration={} → \
                 per_seed={} (clamped to {}), 実効合計={}, 未使用={})。\
                 `--concurrency` を seeds × games_per_iteration の倍数に揃えると無駄なく回る。",
                cli.concurrency,
                seed_values.len(),
                games_per_iter,
                per_seed_concurrency,
                effective_per_seed,
                effective_total,
                cli.concurrency - effective_total,
            );
        }
    }

    let engine_path = resolve_engine_path(&cli)?;
    let engine_args = cli.engine_args.clone().unwrap_or_default();
    // run_dir を確保 (state / meta / CSV を全て同 dir 配下に置く前提)
    std::fs::create_dir_all(&cli.run_dir)
        .with_context(|| format!("failed to create run-dir {}", cli.run_dir.display()))?;

    // 同一 run-dir に対する二重起動を防ぐため、最初に exclusive lock を取る。
    // 取得失敗時は他プロセスが state.params/meta.json/CSV を書き換える危険が
    // あるので即 bail。lock は process 終了時 (Drop) に自動削除されるが、
    // SIGKILL / 電源断で残留した場合は --force-unlock で消せる。
    let _run_dir_lock = RunDirLock::acquire(&cli.run_dir, cli.force_unlock)?;

    let state_params = state_params_path(&cli.run_dir);

    // ========================================================================
    // init/resume 分岐: decide_init_action で意思決定 → apply_init_action で副作用を実行
    // ========================================================================
    let meta_path = cli.meta_file.clone().unwrap_or_else(|| default_meta_path(&cli.run_dir));
    let init_action = decide_init_action(
        cli.init_from.is_some(),
        state_params.exists(),
        cli.resume,
        cli.force_init,
        cli.use_existing_state_as_init,
    );
    // force-init 時に削除する run-dir 直下の派生 CSV 群。CSV writer は cli.resume=false
    // で truncate もするが、能動削除しておくことで run-dir の状態を fresh と一致させる
    // (例: --no-stats-csv で writer が走らないケースでも stale CSV が残らない)。
    // CSV override (--stats-csv / --stats-aggregate-csv / --param-values-csv) で
    // run-dir 外を指定した場合、その override 先は本リストに含まれない (外部集約 CSV
    // append 運用を保護するため)。一方 --meta-file の override 先は active resume
    // state とみなし `apply_init_action` 側で別途削除される。詳細は
    // `default_force_init_cleanup_paths` の doc を参照。
    let force_init_cleanup_paths = default_force_init_cleanup_paths(&cli.run_dir);
    let force_init_cleanup_refs: Vec<&Path> =
        force_init_cleanup_paths.iter().map(|p| p.as_path()).collect();
    let effective_action = apply_init_action(
        &init_action,
        cli.init_from.as_deref(),
        &state_params,
        meta_path.as_path(),
        &force_init_cleanup_refs,
    )?;

    let translator = match &cli.engine_param_mapping {
        Some(path) => {
            let t = EngineNameTranslator::from_mapping_file(path)?;
            eprintln!("engine param mapping: {} entries loaded from {}", t.len(), path.display());
            t
        }
        None => EngineNameTranslator::empty(),
    };
    let mut params = read_params(&state_params)?;
    let schedule = ScheduleConfig {
        alpha: cli.alpha,
        gamma: cli.gamma,
        a_ratio: cli.a_ratio,
        mobility: cli.mobility,
        total_iterations: cli.iterations,
    };
    let (start_iteration, mut total_games, init_snapshot) = match &effective_action {
        NonBailAction::Resume { verify_init } => {
            let meta = load_meta(&meta_path).with_context(|| {
                format!("--resume was set but metadata load failed: {}", meta_path.display())
            })?;
            // v3 hard bail: 古い meta は再開不可 (新規 run dir で fresh start を要求)
            if meta.format_version != META_FORMAT_VERSION {
                bail!(
                    "meta format version 不一致 (got v{}, expected v{}) in {}.\n\
                     v{} 形式は v{} とは互換性がありません。\n\
                     新規 run dir で `--init-from <canonical>` から fresh start してください。",
                    meta.format_version,
                    META_FORMAT_VERSION,
                    meta_path.display(),
                    meta.format_version,
                    META_FORMAT_VERSION,
                );
            }
            if !schedule_matches(meta.schedule, schedule) {
                if cli.force_schedule {
                    eprintln!(
                        "warning: schedule differs from metadata but continuing due to --force-schedule \
                         (meta={}, meta_schedule={:?}, cli_schedule={:?})",
                        meta_path.display(),
                        meta.schedule,
                        schedule
                    );
                } else {
                    bail!(
                        "schedule mismatch with {}. use --force-schedule to override \
                         (meta_schedule={:?}, cli_schedule={:?})",
                        meta_path.display(),
                        meta.schedule,
                        schedule
                    );
                }
            }
            // state.params の transactional 整合性検証 (v4 で追加):
            // 反復ごとに「write_params → meta save」の順で書くため、両者の間で落ちると
            // meta.completed_iterations より state.params が 1 反復先行する状態が残る。
            // resume 時に on-disk state.params の hash を meta.current_params_sha256 と
            // 突き合わせ、乖離があれば bail させて状況をユーザに見せる。
            let on_disk_state_hash = sha256_hex_of_file(&state_params)?;
            if meta.current_params_sha256 != on_disk_state_hash {
                bail!(
                    "state.params と meta.json が不整合です ({}).\n\
                     meta.current_params_sha256 = {}\n\
                     on-disk state.params hash  = {}\n\
                     考えられる原因:\n  \
                       1. write_params → save_meta の間で前回 run がクラッシュした (1 反復差)\n  \
                       2. state.params が外部から書き換えられた\n  \
                     いずれにせよ resume を継続すると SPSA の進行状態が破綻するため停止します。\n\
                     対処: 新規 run dir で `--init-from <canonical>` から fresh start するか、\n  \
                     原因 (1) と分かっていて 1 反復差を許容する場合は新規 run dir で\n  \
                     `--init-from {state_path} --use-existing-state-as-init` でやり直してください。",
                    meta_path.display(),
                    meta.current_params_sha256,
                    on_disk_state_hash,
                    state_path = state_params.display(),
                );
            }

            // param 名集合の hash 検証 (resume 時に param 集合が変わっていないこと)。
            // TODO(PR2): mapping 表に新パラメータを追加した正当な変更も現状 hard bail
            // になる。`--force-name-set` か `--allow-param-set-change` の escape hatch を
            // PR2 で導入検討。それまでは新規 run dir で fresh start する運用で凌ぐ。
            let current_name_hash = param_name_set_sha256(&params);
            if meta.param_name_set_sha256 != current_name_hash {
                bail!(
                    "param 名集合が meta と不一致です ({}).\n\
                     meta.param_name_set_sha256 = {}\n\
                     current  param_name_set_sha256 = {}\n\
                     param 集合変更は resume 不可 (本 PR では escape hatch なし)。\n\
                     新規 run dir で fresh start してください。",
                    meta_path.display(),
                    meta.param_name_set_sha256,
                    current_name_hash,
                );
            }
            // --init-from 指定時は整合性検証を実施 (resume が想定 canonical で開始した
            // run なら値は近いはず。乖離があれば誤った canonical 混入のサイン)。
            if *verify_init {
                let init_path =
                    cli.init_from.as_ref().expect("Resume{verify_init:true} requires init_from");
                let report = verify_init_matches_existing(init_path, &state_params)?;
                report.print_summary(init_path, &state_params);
                if report.has_name_set_mismatch() {
                    eprintln!(
                        "warning: --init-from と既存 params で param 名集合が異なります \
                         (extra_in_init={}, missing_in_init={})",
                        report.extra_in_init.len(),
                        report.missing_in_init.len()
                    );
                }
                if cli.strict_init_check && report.exceeds_strict_threshold() {
                    bail!(
                        "--strict-init-check: init-from と existing で乖離が閾値超過 \
                         (median={:.3}σ, max={:.3}σ)",
                        report.median_step_units,
                        report.max_step_units
                    );
                }
            }
            let snapshot = InitMetaSnapshot::from_existing_meta(&meta);
            (meta.completed_iterations, meta.total_games, snapshot)
        }
        NonBailAction::CopyInitFromFresh
        | NonBailAction::UseExistingFresh
        | NonBailAction::ForceInitOverwrite => {
            let snapshot = InitMetaSnapshot::for_fresh_start(
                &effective_action,
                &state_params,
                cli.init_from.as_deref(),
                &engine_path,
                cli.engine_param_mapping.as_deref(),
            )?;
            (0, 0, snapshot)
        }
    };
    let end_iteration = start_iteration
        .checked_add(cli.iterations)
        .context("iteration index overflow")?;
    let stats_csv_path: Option<PathBuf> = if cli.no_stats_csv {
        None
    } else {
        Some(cli.stats_csv.clone().unwrap_or_else(|| default_stats_csv_path(&cli.run_dir)))
    };
    let aggregate_csv_path: Option<PathBuf> = if cli.no_stats_aggregate_csv {
        None
    } else if let Some(path) = &cli.stats_aggregate_csv {
        Some(path.clone())
    } else if seed_values.len() > 1 {
        // `--stats-csv` が明示指定されている場合のみ、aggregate を `<stats_csv>.aggregate.csv`
        // で派生させる。run-dir モードの既定では `<run-dir>/stats_aggregate.csv`。
        if let Some(stats_path) = &cli.stats_csv {
            Some(PathBuf::from(format!("{}.aggregate.csv", stats_path.display())))
        } else {
            Some(default_stats_aggregate_csv_path(&cli.run_dir))
        }
    } else {
        None
    };
    let mut stats_csv_writer = if let Some(path) = stats_csv_path.as_deref() {
        Some(open_stats_csv_writer(path, cli.resume)?)
    } else {
        None
    };
    let mut stats_aggregate_csv_writer = if let Some(path) = aggregate_csv_path.as_deref() {
        Some(open_stats_aggregate_csv_writer(path, cli.resume)?)
    } else {
        None
    };
    let param_values_csv_path: Option<PathBuf> = if cli.no_param_values_csv {
        None
    } else {
        Some(
            cli.param_values_csv
                .clone()
                .unwrap_or_else(|| default_param_values_csv_path(&cli.run_dir)),
        )
    };
    let mut param_values_csv_writer = if let Some(path) = param_values_csv_path.as_deref() {
        Some(open_param_values_csv_writer(path, cli.resume, &params)?)
    } else {
        None
    };

    // iter 0 スナップショット: 起動時の params を記録する (fresh / force-init / use-existing-fresh)。
    // resume 時は既存 CSV に既に iter 0 行が含まれる前提で append 継続するため、ここではスキップ。
    // 判定は `effective_action` (NonBailAction) で行うことで、ユーザが手動で
    // `meta.completed_iterations: 0` を作って --resume したエッジケースで重複書きを防ぐ
    // (`start_iteration == 0` だけだとそのケースで誤って iter 0 行を append してしまう)。
    // これがあれば事故解析時 (誤った canonical 混入等) に「最初に何で起動したか」を CSV だけで追える。
    let is_fresh_start = matches!(
        effective_action,
        NonBailAction::CopyInitFromFresh
            | NonBailAction::UseExistingFresh
            | NonBailAction::ForceInitOverwrite,
    );
    if is_fresh_start && let Some(writer) = param_values_csv_writer.as_mut() {
        write_param_values_csv_row(writer, 0, &params)?;
        // 即 flush: iter 1 完了前にクラッシュしても iter 0 行を CSV に残し、
        // 「何で起動したか」を後追い解析できるようにする (事故解析用途で必須)。
        writer.flush()?;
    }

    if cli.startpos_file.is_none() {
        if cli.require_startpos_file {
            bail!("--require-startpos-file was set but --startpos-file was not provided");
        }
        eprintln!(
            "warning: --startpos-file is not specified. opening diversity may be insufficient"
        );
    }

    let (start_positions, _) =
        load_start_positions(cli.startpos_file.as_deref(), cli.sfen.as_deref(), None, None)?;
    // active mask は iteration 中に変化しない（params の値だけが更新され、name/not_used
    // /regex マッチ性は不変）ため、ここで 1 度だけ計算してホットパス (apply_parameter_vector)
    // で再利用する。
    let active_mask: Vec<bool> = params
        .iter()
        .map(|p| is_param_active(p, active_only_regex.as_ref(), &translator))
        .collect();
    let active_param_count = active_mask.iter().filter(|&&b| b).count();
    if active_param_count == 0 {
        bail!(
            "no active parameters (active_only_regex={:?}, not_used filtering may have excluded all)",
            cli.active_only_regex
        );
    }
    eprintln!("active params: {active_param_count}/{}", params.len());

    // 翻訳器有効時、`active_only_regex` でマッチしたが unmapped で除外されたパラメータを
    // info 出力する。「期待した parameter が摂動されていない」事象に気づきやすくする。
    if translator.is_enabled() {
        let mut unmapped_active: Vec<&str> = params
            .iter()
            .filter(|p| {
                !p.not_used
                    && active_only_regex.as_ref().is_none_or(|re| re.is_match(&p.name))
                    && !translator.is_mapped(&p.name)
            })
            .map(|p| p.name.as_str())
            .collect();
        if !unmapped_active.is_empty() {
            unmapped_active.sort();
            eprintln!(
                "info: {} param(s) matched --active-only-regex but are unmapped (translator skipped):",
                unmapped_active.len()
            );
            for n in &unmapped_active {
                eprintln!("  - {n}");
            }
        }
    }

    print_startup_summary(&StartupContext {
        snapshot: &init_snapshot,
        schedule: &schedule,
        params: &params,
        active_mask: &active_mask,
        active_param_count,
        start_iteration,
        end_iteration,
        seed_values: &seed_values,
        params_path: &state_params,
        meta_path: &meta_path,
    });

    // 公平な対局条件のため、tournament と同様に NetworkDelay=0 と
    // MinimumThinkingTime をデフォルトで注入する。ユーザーが明示的に
    // --usi-option で指定した場合はそちらを優先。
    // - NetworkDelay: 0 以外だと秒境界切り上げで思考時間が短縮され、
    //   時間切れ・思考時間の偏りの原因になる。
    // - MinimumThinkingTime: byoyomi 時は byoyomi と一致させることで秒読み全体を使い切れる。
    //   フィッシャー/ノード数モードでは 0（エンジンの時間管理に委ねる）。
    let min_think = if cli.nodes.is_none() && cli.btime.is_none() && cli.byoyomi > 0 {
        cli.byoyomi.to_string()
    } else {
        "0".to_string()
    };
    let time_defaults: [(&str, &str); 3] = [
        ("NetworkDelay", "0"),
        ("NetworkDelay2", "0"),
        ("MinimumThinkingTime", min_think.as_str()),
    ];
    let mut usi_options = cli.usi_options.clone().unwrap_or_default();
    for (name, default_value) in &time_defaults {
        let already_set =
            usi_options.iter().any(|o| o.split_once('=').is_some_and(|(k, _)| k == *name));
        if !already_set {
            usi_options.push(format!("{name}={default_value}"));
        }
    }

    let base_cfg = EngineConfig {
        path: engine_path,
        args: engine_args,
        threads: cli.threads,
        hash_mb: cli.hash_mb,
        network_delay: None,
        network_delay2: None,
        minimum_thinking_time: None,
        slowmover: None,
        ponder: false,
        usi_options,
    };

    let game_cfg = GameConfig {
        max_moves: cli.max_moves,
        timeout_margin_ms: cli.timeout_margin_ms,
        pass_rights: None,
        go_depth: None,
        go_nodes: cli.nodes,
    };
    let tc = if cli.nodes.is_some() {
        // ノード数指定時は時間制御不要だが、タイムアウト検出用に十分大きな値を設定
        TimeControl::new(0, 0, 0, 0, 0)
    } else if let Some(btime) = cli.btime {
        TimeControl::new(btime, btime, cli.binc, cli.binc, 0)
    } else {
        TimeControl::new(0, 0, 0, 0, cli.byoyomi)
    };
    let mut early_stop_consecutive = 0u32;

    // Fishtest 方式: per-param スケジュール定数を初期化
    let big_a = schedule.a_ratio * end_iteration as f64;
    let param_schedules: Vec<ParamScheduleConstants> = params
        .iter()
        .map(|p| {
            ParamScheduleConstants::compute(
                p.c_end,
                p.r_end,
                end_iteration,
                schedule.a_ratio,
                schedule.alpha,
                schedule.gamma,
            )
        })
        .collect();

    for iter in start_iteration..end_iteration {
        let mut update_sums = vec![0.0f64; params.len()];
        let mut seed_raw_results = Vec::with_capacity(seed_values.len());
        let mut seed_plus_wins = Vec::with_capacity(seed_values.len());
        let mut seed_minus_wins = Vec::with_capacity(seed_values.len());
        let mut seed_draws = Vec::with_capacity(seed_values.len());
        let mut seed_rows = Vec::with_capacity(seed_values.len());

        // Phase A: 全 seed の事前計算（CPU-light, sequential）。各 seed の RNG/flips/shifts/
        // start_pos_indices を生成。total_games_start はセッション累積に seed_idx × games_per_iter
        // を足して決定論的に求める。
        let prep_ctx = SeedPrepCtx {
            big_a,
            schedule,
            params: &params,
            param_schedules: &param_schedules,
            active_only_regex: active_only_regex.as_ref(),
            translator: &translator,
            start_positions_len: start_positions.len(),
            games_per_iteration: cli.games_per_iteration as usize,
            random_startpos: cli.random_startpos,
        };
        let mut preps = Vec::with_capacity(seed_values.len());
        for (seed_idx, base_seed) in seed_values.iter().copied().enumerate() {
            let seed_total_games_start = total_games
                .checked_add(seed_idx * cli.games_per_iteration as usize)
                .context("total_games offset overflow")?;
            preps.push(compute_seed_prep(&prep_ctx, iter, base_seed, seed_total_games_start)?);
        }

        // Phase B: ゲーム実行（heavy）。--parallel-seeds 指定 + seed 数 ≥ 2 のとき thread::scope
        // で seed 並列実行、それ以外は順次実行。--concurrency を seed 数で分配して
        // CPU の取り合いを避ける。
        let parallelize = cli.parallel_seeds && seed_values.len() >= 2;
        let per_seed_concurrency = if parallelize {
            (cli.concurrency / seed_values.len()).max(1)
        } else {
            cli.concurrency
        };
        let seed_count = seed_values.len();
        let run_stats: Vec<SeedGameStats> = if parallelize {
            // 借用を closure 外で確定 (move closure でも参照を捕捉)
            let base_cfg_ref = &base_cfg;
            let params_ref = &params;
            let start_positions_ref = &start_positions;
            let game_cfg_ref = &game_cfg;
            let translator_ref = &translator;
            let active_mask_ref = &active_mask;
            let preps_ref = &preps;
            std::thread::scope(|scope| -> Result<Vec<SeedGameStats>> {
                let handles: Vec<_> = (0..seed_count)
                    .map(|seed_idx| {
                        let h = scope.spawn(move || -> Result<SeedGameStats> {
                            let prep = &preps_ref[seed_idx];
                            run_seed_games_parallel(SeedRunContext {
                                concurrency: per_seed_concurrency,
                                base_cfg: base_cfg_ref,
                                params: params_ref,
                                plus_values: &prep.plus_values,
                                minus_values: &prep.minus_values,
                                start_positions: start_positions_ref,
                                start_pos_indices: &prep.start_pos_indices,
                                game_cfg: game_cfg_ref,
                                tc,
                                total_games_start: prep.seed_total_games_start,
                                iteration: iter + 1,
                                seed_idx,
                                seed_count,
                                base_seed: prep.base_seed,
                                translator: translator_ref,
                                active_mask: active_mask_ref,
                            })
                        });
                        (seed_idx, h)
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|(seed_idx, h)| {
                        h.join().map_err(|payload| {
                            anyhow::anyhow!(
                                "seed worker panicked: seed_idx={seed_idx}: {}",
                                panic_payload_to_string(&payload),
                            )
                        })?
                    })
                    .collect()
            })?
        } else {
            preps
                .iter()
                .enumerate()
                .map(|(seed_idx, prep)| -> Result<SeedGameStats> {
                    run_seed_games_parallel(SeedRunContext {
                        concurrency: per_seed_concurrency,
                        base_cfg: &base_cfg,
                        params: &params,
                        plus_values: &prep.plus_values,
                        minus_values: &prep.minus_values,
                        start_positions: &start_positions,
                        start_pos_indices: &prep.start_pos_indices,
                        game_cfg: &game_cfg,
                        tc,
                        total_games_start: prep.seed_total_games_start,
                        iteration: iter + 1,
                        seed_idx,
                        seed_count,
                        base_seed: prep.base_seed,
                        translator: &translator,
                        active_mask: &active_mask,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };

        // Phase C: 集計（CPU-light, sequential, seed 順序維持）。
        for (prep, stats) in preps.iter().zip(run_stats.iter()) {
            total_games = total_games
                .checked_add(cli.games_per_iteration as usize)
                .context("total_games overflow")?;

            let raw_result = stats.step_sum;
            let plus_wins = stats.plus_wins;
            let minus_wins = stats.minus_wins;
            let draws = stats.draws;

            // Fishtest 更新: signal_j = R_k_j × c_k_j × result × flip_j
            for (idx, (p, (&flip, sched))) in
                params.iter().zip(prep.flips.iter().zip(param_schedules.iter())).enumerate()
            {
                if !is_param_active(p, active_only_regex.as_ref(), &translator)
                    || p.c_end.abs() <= f64::EPSILON
                {
                    continue;
                }
                let (c_k, r_k) = sched.at_iteration(iter, big_a, schedule.alpha, schedule.gamma);
                update_sums[idx] += r_k * c_k * raw_result * flip;
            }

            seed_raw_results.push(raw_result);
            seed_plus_wins.push(plus_wins as f64);
            seed_minus_wins.push(minus_wins as f64);
            seed_draws.push(draws as f64);

            seed_rows.push(IterationStats {
                iteration: iter + 1,
                seed: prep.base_seed,
                games: cli.games_per_iteration,
                plus_wins,
                minus_wins,
                draws,
                raw_result,
                active_params: prep.active_params,
                avg_abs_shift: prep.avg_abs_shift,
                updated_params: 0,
                avg_abs_update: 0.0,
                max_abs_update: 0.0,
                total_games: 0,
            });
        }

        // Seed 平均後にパラメータ更新
        let mut updated_params = 0usize;
        let mut abs_update_sum = 0.0f64;
        let mut max_abs_update = 0.0f64;
        for (idx, p) in params.iter_mut().enumerate() {
            if !is_param_active(p, active_only_regex.as_ref(), &translator)
                || p.c_end.abs() <= f64::EPSILON
            {
                continue;
            }
            let before = p.value;
            let avg_signal = update_sums[idx] / seed_values.len() as f64;
            let updated = clamped_value(p, p.value + avg_signal * cli.mobility);
            p.value = if p.is_int { updated.round() } else { updated };
            let abs_update = (p.value - before).abs();
            updated_params += 1;
            abs_update_sum += abs_update;
            if abs_update > max_abs_update {
                max_abs_update = abs_update;
            }
        }
        let avg_abs_update = if updated_params > 0 {
            abs_update_sum / updated_params as f64
        } else {
            0.0
        };
        if let Some(writer) = stats_csv_writer.as_mut() {
            for row in &mut seed_rows {
                row.updated_params = updated_params;
                row.avg_abs_update = avg_abs_update;
                row.max_abs_update = max_abs_update;
                row.total_games = total_games;
                write_stats_csv_row(writer, *row)?;
            }
            writer.flush()?;
        }

        let (raw_result_mean, raw_result_variance) = mean_and_variance(&seed_raw_results);
        let (plus_wins_mean, plus_wins_variance) = mean_and_variance(&seed_plus_wins);
        let (minus_wins_mean, minus_wins_variance) = mean_and_variance(&seed_minus_wins);
        let (draws_mean, draws_variance) = mean_and_variance(&seed_draws);

        write_params(&state_params, &params)?;
        if let Some(writer) = param_values_csv_writer.as_mut() {
            write_param_values_csv_row(writer, iter + 1, &params)?;
            writer.flush()?;
        }
        // state.params 更新 → meta 更新の transactional 復旧用に、書き込み直後の
        // state.params を hash して meta に焼き込む。resume 起動時に on-disk hash と
        // 突き合わせて両者の乖離を検出する。
        let current_params_sha256 = sha256_hex_of_file(&state_params)?;
        let meta = ResumeMetaData {
            format_version: META_FORMAT_VERSION,
            state_params_file: state_params.display().to_string(),
            completed_iterations: iter + 1,
            total_games,
            last_raw_result_mean: raw_result_mean,
            last_avg_abs_update: avg_abs_update,
            updated_at_utc: Utc::now().to_rfc3339(),
            schedule,
            init_params_sha256: init_snapshot.init_params_sha256.clone(),
            init_from_sha256: init_snapshot.init_from_sha256.clone(),
            init_from_path: init_snapshot.init_from_path.clone(),
            param_name_set_sha256: param_name_set_sha256(&params),
            active_param_count,
            engine_path: init_snapshot.engine_path.clone(),
            engine_param_mapping_path: init_snapshot.engine_param_mapping_path.clone(),
            engine_param_mapping_sha256: init_snapshot.engine_param_mapping_sha256.clone(),
            init_mode: init_snapshot.init_mode,
            current_params_sha256,
        };
        save_meta(&meta_path, &meta)?;
        eprintln!(
            "iter={} seeds={} raw_result_mean={:+.3} raw_result_var={:.6} \
             avg_abs_update={:.6} max_abs_update={:.6} checkpoint={} meta={}",
            iter + 1,
            seed_values.len(),
            raw_result_mean,
            raw_result_variance,
            avg_abs_update,
            max_abs_update,
            state_params.display(),
            meta_path.display()
        );
        if let Some(writer) = stats_aggregate_csv_writer.as_mut() {
            write_stats_aggregate_csv_row(
                writer,
                AggregateIterationStats {
                    iteration: iter + 1,
                    seed_count: seed_values.len(),
                    games_per_seed: cli.games_per_iteration,
                    raw_result_mean,
                    raw_result_variance,
                    plus_wins_mean,
                    plus_wins_variance,
                    minus_wins_mean,
                    minus_wins_variance,
                    draws_mean,
                    draws_variance,
                    total_games,
                },
            )?;
            writer.flush()?;
        }

        if let Some(config) = early_stop_config {
            let early_stop_hit = avg_abs_update <= config.avg_abs_update_threshold
                && raw_result_variance <= config.result_variance_threshold;
            if early_stop_hit {
                early_stop_consecutive = early_stop_consecutive.saturating_add(1);
            } else {
                early_stop_consecutive = 0;
            }
            eprintln!(
                "iter={} early_stop_hit={} consecutive={}/{} thresholds(avg_abs_update<={:.6}, result_variance<={:.6})",
                iter + 1,
                early_stop_hit,
                early_stop_consecutive,
                config.patience,
                config.avg_abs_update_threshold,
                config.result_variance_threshold
            );
            if early_stop_consecutive >= config.patience {
                eprintln!(
                    "early stop triggered at iter={} (consecutive={})",
                    iter + 1,
                    early_stop_consecutive
                );
                break;
            }
        }
    }

    // 正常完了時に <run-dir>/final.params を atomic に書き出す。
    // state.params は反復ごとに更新され続ける live state なので、外部ツール
    // (tune.py apply 等) に渡す確定スナップショットとして final.params を別 path で
    // 提供する。これにより SPSA を裏で続行しつつ確定値の apply を並行実行できる。
    let final_path = cli.run_dir.join("final.params");
    write_params(&final_path, &params)?;
    eprintln!("final params written: {}", final_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // decide_init_action: 16 通り (4 boolean 入力) を網羅
    // ========================================================================

    fn decide(init: bool, exists: bool, resume: bool, force: bool) -> InitAction {
        decide_init_action(init, exists, resume, force, false)
    }

    fn decide_with_use_existing(
        init: bool,
        exists: bool,
        resume: bool,
        force: bool,
        use_existing: bool,
    ) -> InitAction {
        decide_init_action(init, exists, resume, force, use_existing)
    }

    #[test]
    fn decide_resume_force_init_conflict() {
        for init in [false, true] {
            for exists in [false, true] {
                let action = decide(init, exists, true, true);
                assert!(
                    matches!(action, InitAction::Bail(InitError::ResumeForceInitConflict)),
                    "init={init} exists={exists} resume=force=true → ResumeForceInitConflict, got {action:?}"
                );
            }
        }
    }

    #[test]
    fn decide_force_init_requires_init_from() {
        // force_init=true && has_init_from=false (resume=false 限定)
        for exists in [false, true] {
            let action = decide(false, exists, false, true);
            assert!(
                matches!(action, InitAction::Bail(InitError::ForceInitRequiresInitFrom)),
                "init=false exists={exists} force=true → ForceInitRequiresInitFrom, got {action:?}"
            );
        }
    }

    #[test]
    fn decide_resume_requires_existing_params() {
        // resume=true && exists=false && force=false
        for init in [false, true] {
            let action = decide(init, false, true, false);
            assert!(
                matches!(action, InitAction::Bail(InitError::ResumeRequiresExistingParams)),
                "init={init} exists=false resume=true → ResumeRequiresExistingParams, got {action:?}"
            );
        }
    }

    #[test]
    fn decide_resume_with_existing_params() {
        // init=false → verify_init=false
        let action = decide(false, true, true, false);
        assert_eq!(action, InitAction::Resume { verify_init: false });
        // init=true → verify_init=true
        let action = decide(true, true, true, false);
        assert_eq!(action, InitAction::Resume { verify_init: true });
    }

    #[test]
    fn decide_force_init_overwrite_happy_path() {
        let action = decide(true, true, false, true);
        assert_eq!(action, InitAction::ForceInitOverwrite);
    }

    #[test]
    fn decide_force_init_requires_existing_params() {
        let action = decide(true, false, false, true);
        assert!(matches!(action, InitAction::Bail(InitError::ForceInitRequiresExistingParams)));
    }

    #[test]
    fn decide_copy_init_from_fresh() {
        let action = decide(true, false, false, false);
        assert_eq!(action, InitAction::CopyInitFromFresh);
    }

    #[test]
    fn decide_init_from_exists_requires_flag() {
        // --init-from の暗黙スキップを bail する本命ケース
        let action = decide(true, true, false, false);
        assert!(matches!(action, InitAction::Bail(InitError::InitFromExistsRequiresFlag)));
    }

    #[test]
    fn decide_use_existing_requires_flag() {
        // 旧版の silent fresh start を bail する。
        // 既存 state + フラグ指定なし → UseExistingRequiresFlag bail
        let action = decide(false, true, false, false);
        assert!(
            matches!(action, InitAction::Bail(InitError::UseExistingRequiresFlag)),
            "got {action:?}"
        );
    }

    #[test]
    fn decide_use_existing_state_as_init_happy_path() {
        // 既存 state + --use-existing-state-as-init のみ指定 → UseExistingFresh
        let action = decide_with_use_existing(false, true, false, false, true);
        assert_eq!(action, InitAction::UseExistingFresh);
    }

    #[test]
    fn decide_use_existing_state_as_init_without_state_bails() {
        // state 不在で --use-existing-state-as-init を指定 → 意味がないため bail
        let action = decide_with_use_existing(false, false, false, false, true);
        assert!(
            matches!(action, InitAction::Bail(InitError::NoInitNorExistingParams)),
            "got {action:?}"
        );
    }

    #[test]
    fn decide_use_existing_state_as_init_conflicts_with_other_flags() {
        // --use-existing-state-as-init は他の意思表示フラグと排他。
        // 他フラグ間の矛盾 (e.g. force=true && !init → ForceInitRequiresInitFrom)
        // が先に発火するケースもあるので、ここでは「何らかの Bail に落ちること」のみ assert。
        for (init, resume, force) in [
            (true, false, false), // init + use_existing
            (false, true, false), // resume + use_existing
            (false, false, true), // force + use_existing (force は init 必須で別 Bail)
            (true, true, false),  // init + resume + use_existing
            (true, false, true),  // init + force + use_existing
        ] {
            let action = decide_with_use_existing(init, true, resume, force, true);
            assert!(
                matches!(action, InitAction::Bail(_)),
                "init={init} resume={resume} force={force}: 期待は Bail、got {action:?}"
            );
        }
    }

    #[test]
    fn decide_no_init_nor_existing_params() {
        let action = decide(false, false, false, false);
        assert!(matches!(action, InitAction::Bail(InitError::NoInitNorExistingParams)));
    }

    /// 32 通り全網羅: 5 boolean 入力の各組み合わせが unreachable に落ちないこと
    #[test]
    fn decide_covers_all_thirty_two_combinations() {
        for init in [false, true] {
            for exists in [false, true] {
                for resume in [false, true] {
                    for force in [false, true] {
                        for use_existing in [false, true] {
                            let _ =
                                decide_with_use_existing(init, exists, resume, force, use_existing);
                        }
                    }
                }
            }
        }
    }

    // ========================================================================
    // hash ヘルパ: 決定性 / 順序非依存
    // ========================================================================

    fn make_param_for_hash(name: &str) -> SpsaParam {
        SpsaParam {
            name: name.to_string(),
            type_name: "int".into(),
            is_int: true,
            value: 0.0,
            min: 0.0,
            max: 1.0,
            c_end: 1.0,
            r_end: 0.002,
            comment: String::new(),
            not_used: false,
        }
    }

    #[test]
    fn param_name_set_sha256_is_order_independent() {
        let a = vec![
            make_param_for_hash("foo"),
            make_param_for_hash("bar"),
            make_param_for_hash("baz"),
        ];
        let b = vec![
            make_param_for_hash("baz"),
            make_param_for_hash("foo"),
            make_param_for_hash("bar"),
        ];
        assert_eq!(param_name_set_sha256(&a), param_name_set_sha256(&b));
    }

    #[test]
    fn param_name_set_sha256_distinguishes_different_sets() {
        let a = vec![make_param_for_hash("foo"), make_param_for_hash("bar")];
        let b = vec![make_param_for_hash("foo"), make_param_for_hash("BAR")];
        assert_ne!(param_name_set_sha256(&a), param_name_set_sha256(&b));
    }

    #[test]
    fn sha256_hex_of_file_matches_known_vector() {
        // 空ファイルの SHA-256 は既知の固定値
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty");
        std::fs::write(&p, b"").unwrap();
        let hex = sha256_hex_of_file(&p).unwrap();
        assert_eq!(hex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    // ========================================================================
    // verify_init_matches_existing: 整合性検証ロジック
    // ========================================================================

    fn write_params_file(path: &Path, lines: &[&str]) {
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn verify_median_with_even_count_uses_average_of_middle_pair() {
        // 4 件 (偶数) で diff が step 単位で {0, 2, 4, 6} になるよう構築。
        // 厳密中央値 = (2 + 4) / 2 = 3.0σ (旧実装 `[n/2]` だと上側中値 4.0σ になる)。
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("init.params");
        let existing = dir.path().join("existing.params");
        // step=10 で diff が 0/20/40/60 → step 単位 {0, 2, 4, 6}
        write_params_file(
            &init,
            &[
                "p0,int,100,0,1000,10,0.002",
                "p1,int,100,0,1000,10,0.002",
                "p2,int,100,0,1000,10,0.002",
                "p3,int,100,0,1000,10,0.002",
            ],
        );
        write_params_file(
            &existing,
            &[
                "p0,int,100,0,1000,10,0.002",
                "p1,int,120,0,1000,10,0.002",
                "p2,int,140,0,1000,10,0.002",
                "p3,int,160,0,1000,10,0.002",
            ],
        );
        let report = verify_init_matches_existing(&init, &existing).unwrap();
        assert_eq!(report.total, 4);
        assert!(
            (report.median_step_units - 3.0).abs() < 1e-9,
            "median should be 3.0σ (average of 2 and 4), got {}",
            report.median_step_units
        );
    }

    #[test]
    fn verify_reports_perfect_match() {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("init.params");
        let existing = dir.path().join("existing.params");
        let line = "foo,int,100,0,1000,50,0.002";
        write_params_file(&init, &[line]);
        write_params_file(&existing, &[line]);
        let report = verify_init_matches_existing(&init, &existing).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.matched_within_half_step, 1);
        assert!((report.median_step_units - 0.0).abs() < 1e-9);
        assert!((report.max_step_units - 0.0).abs() < 1e-9);
        assert!(!report.exceeds_strict_threshold());
        assert!(!report.has_name_set_mismatch());
    }

    #[test]
    fn verify_reports_strict_threshold_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("init.params");
        let existing = dir.path().join("existing.params");
        // step=50 で diff=300 → 6σ (>5σ)
        write_params_file(&init, &["foo,int,100,0,1000,50,0.002"]);
        write_params_file(&existing, &["foo,int,400,0,1000,50,0.002"]);
        let report = verify_init_matches_existing(&init, &existing).unwrap();
        assert_eq!(report.total, 1);
        assert!(report.max_step_units >= 5.0);
        assert!(report.exceeds_strict_threshold());
    }

    #[test]
    fn verify_uses_actual_c_end_for_step_when_below_one() {
        // c_end=0.1 のパラメータで diff=1 → 10σ。
        // 旧実装の `c_end.max(1.0)` だと step=1 と扱われ 1σ になり strict が誤って通る。
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("init.params");
        let existing = dir.path().join("existing.params");
        write_params_file(&init, &["foo,int,1,0,10,0.1,0.002"]);
        write_params_file(&existing, &["foo,int,2,0,10,0.1,0.002"]);
        let report = verify_init_matches_existing(&init, &existing).unwrap();
        // diff=1, step=0.1 → 10σ
        assert!(
            report.max_step_units > 5.0,
            "c_end < 1 should not be inflated to 1; got max={}σ",
            report.max_step_units
        );
        assert!(report.exceeds_strict_threshold());
    }

    #[test]
    fn verify_handles_zero_c_end_gracefully() {
        // c_end=0 は防御的に step=1 にフォールバック (NaN/inf を出さない)。
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("init.params");
        let existing = dir.path().join("existing.params");
        write_params_file(&init, &["foo,int,5,0,10,0,0.002"]);
        write_params_file(&existing, &["foo,int,5,0,10,0,0.002"]);
        let report = verify_init_matches_existing(&init, &existing).unwrap();
        assert!(!report.max_step_units.is_nan());
        assert_eq!(report.matched_within_half_step, 1);
    }

    #[test]
    fn verify_detects_name_set_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("init.params");
        let existing = dir.path().join("existing.params");
        write_params_file(&init, &["foo,int,100,0,1000,50,0.002", "bar,int,200,0,1000,50,0.002"]);
        write_params_file(
            &existing,
            &["foo,int,100,0,1000,50,0.002", "qux,int,200,0,1000,50,0.002"],
        );
        let report = verify_init_matches_existing(&init, &existing).unwrap();
        assert!(report.has_name_set_mismatch());
        assert_eq!(report.extra_in_init, vec!["bar"]);
        assert_eq!(report.missing_in_init, vec!["qux"]);
    }

    // ========================================================================
    // atomic_copy_file / save_meta: I/O ヘルパ
    // ========================================================================

    #[test]
    fn atomic_copy_file_replaces_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"hello").unwrap();
        std::fs::write(&dst, b"old content").unwrap();
        atomic_copy_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello");
    }

    #[test]
    fn write_params_replaces_existing_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.params");
        // 古い内容を残して、tempfile + persist で完全置換されることを確認
        std::fs::write(&path, b"STALE,STALE,STALE\n").unwrap();
        let params = vec![SpsaParam {
            name: "Foo".into(),
            type_name: "int".into(),
            is_int: true,
            value: 42.0,
            min: 0.0,
            max: 100.0,
            c_end: 1.0,
            r_end: 0.001,
            comment: String::new(),
            not_used: false,
        }];
        write_params(&path, &params).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("Foo,int,42,"), "actual: {body}");
        // ラウンドトリップ
        let reloaded = read_params(&path).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].name, "Foo");
        assert_eq!(reloaded[0].value, 42.0);
        // 一時ファイルが残っていないこと
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let name = entry.unwrap().file_name();
            let s = name.to_string_lossy();
            assert!(!s.starts_with(".spsa_state_"), "tempfile leaked: {s}");
        }
    }

    #[test]
    fn atomic_copy_file_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("nested/sub/dst");
        std::fs::write(&src, b"data").unwrap();
        atomic_copy_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"data");
    }

    // ========================================================================
    // RunDirLock: 排他制御
    // ========================================================================

    #[test]
    fn run_dir_lock_prevents_double_acquire() {
        let dir = tempfile::tempdir().unwrap();
        let lock1 = RunDirLock::acquire(dir.path(), false).unwrap();
        let err = RunDirLock::acquire(dir.path(), false).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("他プロセスが run-dir を使用中"), "actual: {msg}");
        // 中身に PID が記録されていること
        let body = std::fs::read_to_string(dir.path().join(".lock")).unwrap();
        assert!(body.contains("\"pid\""), "lock body: {body}");
        drop(lock1);
        // drop 後は再取得可能
        let _lock2 = RunDirLock::acquire(dir.path(), false).unwrap();
    }

    #[test]
    fn run_dir_lock_force_unlock_removes_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".lock"), "stale").unwrap();
        // force_unlock なしでは衝突
        assert!(RunDirLock::acquire(dir.path(), false).is_err());
        // force_unlock 指定で取得成功
        let _lock = RunDirLock::acquire(dir.path(), true).unwrap();
    }

    #[test]
    fn run_dir_lock_drop_cleans_up_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _lock = RunDirLock::acquire(dir.path(), false).unwrap();
            assert!(dir.path().join(".lock").exists());
        }
        assert!(!dir.path().join(".lock").exists());
    }

    #[test]
    fn non_bail_action_from_bail_returns_none() {
        assert!(
            NonBailAction::from_init_action(&InitAction::Bail(InitError::NoInitNorExistingParams))
                .is_none()
        );
    }

    #[test]
    fn apply_force_init_overwrites_params_and_removes_meta() {
        // 順序バグ (params copy → meta 削除) の再発検知用 file-level テスト。
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("canonical.params");
        let target_params = dir.path().join("existing.params");
        let target_meta = dir.path().join("existing.params.meta.json");
        let stale_csv = dir.path().join("existing.params.values.csv");

        std::fs::write(&canonical, b"foo,int,100,0,1000,50,0.002\n").unwrap();
        std::fs::write(&target_params, b"foo,int,999,0,1000,50,0.002\n").unwrap();
        std::fs::write(&target_meta, b"{\"old\":\"meta\"}").unwrap();
        std::fs::write(&stale_csv, b"old,csv,content").unwrap();

        let action = InitAction::ForceInitOverwrite;
        let stale_csvs: &[&Path] = &[stale_csv.as_path()];
        let result = apply_init_action(
            &action,
            Some(canonical.as_path()),
            &target_params,
            &target_meta,
            stale_csvs,
        )
        .unwrap();
        assert_eq!(result, NonBailAction::ForceInitOverwrite);

        // params は canonical で上書きされている
        assert_eq!(std::fs::read(&target_params).unwrap(), b"foo,int,100,0,1000,50,0.002\n");
        // meta は削除されている (順序的に必ず消える)
        assert!(!target_meta.exists(), "meta should be removed by force-init");
        // stale CSV も削除されている (best-effort)
        assert!(!stale_csv.exists(), "stale CSV should be removed");
    }

    #[test]
    fn apply_force_init_bails_on_meta_remove_failure() {
        // meta が削除できない (= dir として存在) 場合に bail することを確認。
        // params は上書きされない (順序保証: meta 削除失敗 → そこで return)。
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("canonical.params");
        let target_params = dir.path().join("existing.params");
        // meta_path に「ディレクトリ」を置くと remove_file が失敗する
        let blocked_meta_dir = dir.path().join("existing.params.meta.json");
        std::fs::create_dir(&blocked_meta_dir).unwrap();

        std::fs::write(&canonical, b"new content\n").unwrap();
        std::fs::write(&target_params, b"old content\n").unwrap();

        let action = InitAction::ForceInitOverwrite;
        let result = apply_init_action(
            &action,
            Some(canonical.as_path()),
            &target_params,
            &blocked_meta_dir,
            &[],
        );
        assert!(result.is_err(), "should bail when meta removal fails");
        // params は触られていない (atomic copy が走らない)
        assert_eq!(std::fs::read(&target_params).unwrap(), b"old content\n");
    }

    #[test]
    fn init_mode_display_matches_serde_kebab_case() {
        // Display と serde 表現を一致させる契約 (startup summary の表記と
        // meta.json の値が同じ文字列で見えることを担保)
        assert_eq!(format!("{}", InitMode::FreshInitFrom), "fresh-init-from");
        assert_eq!(format!("{}", InitMode::FreshExisting), "fresh-existing");
        assert_eq!(format!("{}", InitMode::ForceInit), "force-init");
        assert_eq!(format!("{}", InitMode::Resume), "resume");

        // serde で round-trip して同じ文字列で出ることも確認
        let json = serde_json::to_string(&InitMode::FreshInitFrom).unwrap();
        assert_eq!(json, "\"fresh-init-from\"");
    }

    #[test]
    fn run_dir_path_helpers_use_consistent_layout() {
        let dir = Path::new("/tmp/some_run");
        assert_eq!(state_params_path(dir), dir.join("state.params"));
        assert_eq!(default_meta_path(dir), dir.join("meta.json"));
        assert_eq!(default_param_values_csv_path(dir), dir.join("values.csv"));
        assert_eq!(default_stats_csv_path(dir), dir.join("stats.csv"));
        assert_eq!(default_stats_aggregate_csv_path(dir), dir.join("stats_aggregate.csv"));
    }

    #[test]
    fn default_force_init_cleanup_paths_returns_run_dir_only() {
        // override 先 (--stats-csv で run-dir 外を指定する等) が混入しないことを担保。
        // force-init 時の削除対象は run-dir 直下の 3 派生 CSV のみで、
        // state.params / meta.json は apply_init_action 側で別管理。
        let dir = Path::new("/tmp/some_run");
        let paths = default_force_init_cleanup_paths(dir);
        assert_eq!(paths.len(), 3, "exactly 3 derived CSV paths");
        assert!(paths.contains(&dir.join("values.csv")));
        assert!(paths.contains(&dir.join("stats.csv")));
        assert!(paths.contains(&dir.join("stats_aggregate.csv")));
        // state.params と meta.json は含めない (apply_init_action が個別管理)
        assert!(!paths.contains(&dir.join("state.params")));
        assert!(!paths.contains(&dir.join("meta.json")));
    }

    #[test]
    fn save_and_load_meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.json");
        let meta = ResumeMetaData {
            format_version: META_FORMAT_VERSION,
            state_params_file: "state.params".to_owned(),
            completed_iterations: 5,
            total_games: 1000,
            last_raw_result_mean: -0.5,
            last_avg_abs_update: 1.2,
            updated_at_utc: "2026-01-01T00:00:00Z".to_owned(),
            schedule: ScheduleConfig {
                alpha: 0.602,
                gamma: 0.101,
                a_ratio: 0.1,
                mobility: 1.0,
                total_iterations: 200,
            },
            init_params_sha256: "abc123".to_owned(),
            init_from_sha256: Some("def456".to_owned()),
            init_from_path: Some("canonical.params".to_owned()),
            param_name_set_sha256: "names_hash".to_owned(),
            active_param_count: 100,
            engine_path: "/path/to/engine".to_owned(),
            engine_param_mapping_path: None,
            engine_param_mapping_sha256: None,
            init_mode: InitMode::FreshInitFrom,
            current_params_sha256: "abc123".to_owned(),
        };
        save_meta(&path, &meta).unwrap();
        let loaded = load_meta(&path).unwrap();
        assert_eq!(loaded.format_version, meta.format_version);
        assert_eq!(loaded.init_params_sha256, meta.init_params_sha256);
        assert_eq!(loaded.init_from_sha256, meta.init_from_sha256);
        assert_eq!(loaded.param_name_set_sha256, meta.param_name_set_sha256);
        assert_eq!(loaded.active_param_count, meta.active_param_count);
        assert_eq!(loaded.init_mode, meta.init_mode);
        assert_eq!(loaded.current_params_sha256, meta.current_params_sha256);
    }

    // ========================================================================
    // 既存テスト群
    // ========================================================================

    #[test]
    fn schedule_at_final_iteration_matches_end_values() {
        let c_end = 50.0;
        let r_end = 0.002;
        let n = 200u32;
        let a_ratio = 0.1;
        let alpha = 0.602;
        let gamma = 0.101;
        let big_a = a_ratio * n as f64;

        let sched = ParamScheduleConstants::compute(c_end, r_end, n, a_ratio, alpha, gamma);
        let (c_k, r_k) = sched.at_iteration(n - 1, big_a, alpha, gamma);

        assert!(
            (c_k - c_end).abs() < 1e-6,
            "c_k at final iter should equal c_end: got {c_k}, expected {c_end}"
        );
        assert!(
            (r_k - r_end).abs() < 1e-6,
            "R_k at final iter should equal r_end: got {r_k}, expected {r_end}"
        );
    }

    #[test]
    fn update_magnitude_is_nonzero_for_typical_params() {
        let c_end = 50.0;
        let r_end = 0.002;
        let n = 200u32;
        let a_ratio = 0.1;
        let alpha = 0.602;
        let gamma = 0.101;
        let big_a = a_ratio * n as f64;

        let sched = ParamScheduleConstants::compute(c_end, r_end, n, a_ratio, alpha, gamma);

        // 初期イテレーション (iter=0) での更新量
        let (c_k, r_k) = sched.at_iteration(0, big_a, alpha, gamma);
        let result = 8.0; // 64局で期待される |W-L| ≈ √64
        let update = r_k * c_k * result;
        assert!(update.abs() > 0.5, "update at iter 0 should be significant: got {update}");

        // 最終イテレーション (iter=199) での更新量
        let (c_k, r_k) = sched.at_iteration(n - 1, big_a, alpha, gamma);
        let update = r_k * c_k * result;
        assert!(update.abs() > 0.1, "update at final iter should still be nonzero: got {update}");
    }

    #[test]
    fn early_iterations_have_larger_perturbation() {
        let c_end = 50.0;
        let r_end = 0.002;
        let n = 200u32;
        let a_ratio = 0.1;
        let alpha = 0.602;
        let gamma = 0.101;
        let big_a = a_ratio * n as f64;

        let sched = ParamScheduleConstants::compute(c_end, r_end, n, a_ratio, alpha, gamma);
        let (c_0, _) = sched.at_iteration(0, big_a, alpha, gamma);
        let (c_last, _) = sched.at_iteration(n - 1, big_a, alpha, gamma);
        assert!(c_0 > c_last, "c_k should decrease over iterations: c_0={c_0}, c_last={c_last}");
    }

    fn make_param(name: &str, value: f64, c_end: f64) -> SpsaParam {
        SpsaParam {
            name: name.to_string(),
            type_name: "int".into(),
            is_int: true,
            value,
            min: 0.0,
            max: 100_000.0,
            c_end,
            r_end: 0.002,
            comment: String::new(),
            not_used: false,
        }
    }

    fn make_test_ctx<'a>(
        params: &'a [SpsaParam],
        schedules: &'a [ParamScheduleConstants],
        translator: &'a EngineNameTranslator,
        games_per_iteration: usize,
    ) -> SeedPrepCtx<'a> {
        SeedPrepCtx {
            big_a: 10.0,
            schedule: ScheduleConfig {
                alpha: 0.602,
                gamma: 0.101,
                a_ratio: 0.1,
                mobility: 1.0,
                total_iterations: 100,
            },
            params,
            param_schedules: schedules,
            active_only_regex: None,
            translator,
            start_positions_len: 1957,
            games_per_iteration,
            random_startpos: true,
        }
    }

    /// `compute_seed_prep` のスナップショットテスト。`ChaCha8Rng` は決定論的なため、
    /// 同じ `(base_seed, iter)` に対して flips / shifts / start_pos_indices が完全一致する。
    /// 並列化後も Phase A の事前計算結果がブレないことを保証。
    #[test]
    fn compute_seed_prep_is_deterministic_across_calls() {
        let params = vec![
            make_param("Search_a", 1000.0, 100.0),
            make_param("Search_b", 2000.0, 200.0),
        ];
        let schedules: Vec<ParamScheduleConstants> = params
            .iter()
            .map(|p| ParamScheduleConstants::compute(p.c_end, p.r_end, 100, 0.1, 0.602, 0.101))
            .collect();
        let translator = EngineNameTranslator::empty();
        let ctx = make_test_ctx(&params, &schedules, &translator, 8);

        let prep1 = compute_seed_prep(&ctx, 5, 42, 100).expect("prep1");
        let prep2 = compute_seed_prep(&ctx, 5, 42, 100).expect("prep2");

        assert_eq!(prep1.flips, prep2.flips, "flips must be deterministic from seed/iter");
        assert_eq!(prep1.plus_values, prep2.plus_values);
        assert_eq!(prep1.minus_values, prep2.minus_values);
        assert_eq!(prep1.start_pos_indices, prep2.start_pos_indices);
        assert_eq!(prep1.active_params, prep2.active_params);
    }

    /// 異なる `base_seed` が異なる flip パターンを生むことを保証（並列実行時の seed 間独立性）。
    /// `ChaCha8Rng` は決定論的なため、`(base_seed=1..4, iter=5)` の組み合わせで flip が
    /// 全一致にならないことをスナップショットテストとして確認する。パラメータ数を 6 に増やして
    /// 取り得る flip パターンを 2^6=64 通りに広げ、隣接 seed ペアの直接比較で意図を明確化。
    #[test]
    fn compute_seed_prep_seeds_produce_independent_flips() {
        let params: Vec<_> = (0..6)
            .map(|i| make_param(&format!("Search_p{i}"), 1000.0 + i as f64 * 100.0, 100.0))
            .collect();
        let schedules: Vec<ParamScheduleConstants> = params
            .iter()
            .map(|p| ParamScheduleConstants::compute(p.c_end, p.r_end, 100, 0.1, 0.602, 0.101))
            .collect();
        let translator = EngineNameTranslator::empty();
        let ctx = make_test_ctx(&params, &schedules, &translator, 32);

        let prep1 = compute_seed_prep(&ctx, 5, 1, 100).expect("prep1");
        let prep2 = compute_seed_prep(&ctx, 5, 2, 100).expect("prep2");
        let prep3 = compute_seed_prep(&ctx, 5, 3, 100).expect("prep3");
        let prep4 = compute_seed_prep(&ctx, 5, 4, 100).expect("prep4");

        // 隣接 seed ペアそれぞれで flip パターンが異なることを直接確認
        assert_ne!(prep1.flips, prep2.flips, "seed=1 vs seed=2 flips must differ");
        assert_ne!(prep2.flips, prep3.flips, "seed=2 vs seed=3 flips must differ");
        assert_ne!(prep3.flips, prep4.flips, "seed=3 vs seed=4 flips must differ");
    }
}
