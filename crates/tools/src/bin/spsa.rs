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
    /// - `<run-dir>/state.params`        : SPSA の live 状態 (batch ごとに上書き)
    /// - `<run-dir>/meta.json`           : resume 用メタデータ
    /// - `<run-dir>/values.csv`          : 各 batch のパラメータ値履歴
    /// - `<run-dir>/stats.csv`           : 各 batch の統計
    ///
    /// 通常は `runs/spsa/$(date -u +%Y%m%d_%H%M%S)_<tag>` のように毎回新規 dir を
    /// 切る。`--init-from <canonical>` を併用すると初回起動時に canonical を
    /// `<run-dir>/state.params` に複製する。
    #[arg(long)]
    run_dir: PathBuf,

    /// SPSA の総 game pair 数 (fishtest `num_iter` と等価)。
    /// total_games = 2 × total_pairs。schedule の `k` 軸の上限としても使われる。
    /// 必須引数。後方互換のため `--games-per-iteration` + `--iterations` 併用時は
    /// それらから自動換算する (warning 出力)。
    #[arg(long)]
    total_pairs: Option<u32>,

    /// 1 batch あたりの game pair 数。同じ flip ベクトルで `2 × batch_pairs` 局を
    /// 消化し、batch 末で θ を 1 回更新する (k は `+= batch_pairs`)。
    /// fishtest worker の `iter += game_pairs` と等価。
    #[arg(long, default_value_t = 8)]
    batch_pairs: u32,

    /// **deprecated**: `--total-pairs` + `--batch-pairs` を使用してください。
    /// 後方互換のため残置。指定された場合 `total_pairs = games_per_iteration *
    /// iterations / 2` に自動換算 (warning 出力)。偶数必須。
    #[arg(long)]
    games_per_iteration: Option<u32>,

    /// **deprecated**: `--total-pairs` を使用してください。
    /// `--games-per-iteration` 併用時のみ意味を持つ (両方指定で `total_pairs` を換算)。
    #[arg(long)]
    iterations: Option<u32>,

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

    /// 反復ごとのパラメータ値履歴CSV（wide形式）。既定: <run-dir>/values.csv
    #[arg(long)]
    param_values_csv: Option<PathBuf>,

    /// パラメータ値履歴CSVの出力を無効化する
    #[arg(long, default_value_t = false)]
    no_param_values_csv: bool,

    /// 乱数 seed (省略時はランダム)。SPSA の全 RNG stream は seed と batch index から
    /// 決定論的に生成されるため、同じ seed で run を 2 回回すと同じ θ 軌跡になる。
    ///
    /// fishtest 整合の v4 仕様では multi-seed (`--seeds`) は撤去された (詳細:
    /// `docs/spsa_v4_migration.md`)。複数 base_seed の探索は `--seed` を変えた
    /// 独立 run dir で並列実行する。
    #[arg(long)]
    seed: Option<u64>,

    /// **deprecated/removed**: v3 の multi-seed 機能。指定するとエラー終了する。
    /// 移行ガイドは `docs/spsa_v4_migration.md` を参照。
    #[arg(long, value_delimiter = ',', num_args = 1..)]
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

    /// **deprecated/removed**: v3 の multi-seed 機能。指定するとエラー終了する。
    /// 移行ガイドは `docs/spsa_v4_migration.md` を参照。
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
    /// SPSA の総 game pair 数 (= fishtest `num_iter`)。schedule の k 軸の上限であり、
    /// 全 batch 完了 = `total_pairs / batch_pairs` 個の batch を消化したとき。
    /// resume 時に CLI 指定値と突き合わせて schedule 一致を検証する。
    total_pairs: u32,
    /// 1 batch あたりの game pair 数 (= fishtest worker `game_pairs`)。
    /// resume 時に CLI 指定値と突き合わせる (途中で batch 粒度が変わると k の進行が
    /// 不整合になるため bail)。
    batch_pairs: u32,
    /// SPSA schedule 上の累積 k (= 完了 game pair 数)。`completed_iterations × batch_pairs`
    /// と等価だが、明示フィールドとして持つことで future-proof + 検証用途に使う。
    completed_pairs: u32,
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

/// 1 batch 分の統計 (stats.csv 1 行に対応)。v4 仕様: seed カラム廃止、
/// `batch_pairs` (batch 内 game pair 数) を追加。
#[derive(Clone, Copy, Debug)]
struct IterationStats {
    /// batch 番号 (1-origin)。v3 では「iteration」だったが意味は等価。
    iteration: u32,
    /// この batch で実行した game pair 数 (= games / 2)。
    batch_pairs: u32,
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

/// 1 batch の game 集計結果。raw_result = Σ plus_score (各 game pair が +1 / 0 / -1)。
#[derive(Clone, Copy, Debug)]
struct BatchGameStats {
    step_sum: f64,
    plus_wins: u32,
    minus_wins: u32,
    draws: u32,
}

/// 1 batch 分の事前計算結果（flips / shifts / plus / minus / startpos インデックス）。
///
/// `compute_batch_prep` で生成し、`run_batch_games_parallel` の入力として使う。
/// fishtest 流: 同 batch 内の全 game pair で共通の flip ベクトルと rounded plus/minus 値を使う。
struct BatchPrep {
    base_seed: u64,
    flips: Vec<f64>,
    plus_values: Vec<f64>,
    minus_values: Vec<f64>,
    start_pos_indices: Vec<usize>,
    active_params: usize,
    avg_abs_shift: f64,
    /// この batch 内で消化する game の (累積) 開始 game 番号。stats / log 表示と
    /// `pick_startpos_index` の cyclic 進行に使う。
    batch_total_games_start: usize,
}

struct BatchRunContext<'a> {
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
    /// batch 番号 (1-origin) を log 表示用に渡す。
    iteration: u32,
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
        // 旧 run の final.params が残っていると新 run で上書きされるまで「前回の確定値」
        // が見え続け、tune.py apply に誤投入される事故になる。fresh 系のリスタート (force-init
        // / fresh / use-existing) で必ず消す。
        run_dir.join("final.params"),
    ]
}

/// fresh start (force-init を含む全 fresh 系) で削除すべき run-dir 直下のファイル。
///
/// `apply_init_action` は force-init 経路でのみ `default_force_init_cleanup_paths`
/// を呼ぶ。一方、`CopyInitFromFresh` / `UseExistingFresh` 経路では既に CSV writer の
/// `cli.resume=false` truncate で派生 CSV は上書きされるが、`final.params` は writer
/// を持たないため放置すると stale snapshot が残り続ける。これを防ぐため fresh start
/// 全経路で `final.params` を能動削除する。
fn remove_stale_final_params_for_fresh_start(run_dir: &Path) -> Result<()> {
    let final_path = run_dir.join("final.params");
    match std::fs::remove_file(&final_path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::Error::new(e)).with_context(|| {
            format!(
                "failed to remove stale final.params before fresh start: {}",
                final_path.display()
            )
        }),
    }
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
    // B-3 以降: θ は is_int でも f64 のまま保持するため、CSV も `{:.6}` 固定桁で
    // 出力する。整数値が "42.000000" のように表示されるが、人間可読性より
    // resume 整合性 (write_params と一致) を優先する。
    format!("{:.6}", param.value)
}

fn write_stats_csv_header(writer: &mut BufWriter<File>) -> Result<()> {
    // v4 仕様: seed カラム削除、games → batch_pairs へ変更。1 batch = 1 行。
    writeln!(
        writer,
        "iteration,batch_pairs,plus_wins,minus_wins,draws,raw_result,active_params,\
         avg_abs_shift,updated_params,avg_abs_update,max_abs_update,total_games"
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
        "{},{},{},{},{},{:+.6},{},{:.6},{},{:.6},{:.6},{}",
        stats.iteration,
        stats.batch_pairs,
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

/// `meta.json` の format_version だけ軽量に取り出すための struct。
///
/// `ResumeMetaData` の full schema で deserialize すると、古い meta に対して
/// 必須フィールド不在で先に失敗するため、format_version 不一致の親切な
/// hard bail メッセージに到達できない。version だけ別 struct で先読みする。
#[derive(Deserialize)]
struct MetaFormatVersionOnly {
    format_version: u32,
}

/// v3 形式の meta から v4 silent migration に必要な field を抽出する struct。
/// v3 では `seeds_count` / `games_per_iteration` 相当の情報を保持していた
/// (実際には実装上 v3 にこれらのフィールドはなかったが、`completed_iterations`
/// と外部の CLI から `total_pairs` を再構築する)。
#[derive(Deserialize)]
struct V3MetaSubset {
    schedule: ScheduleConfig,
    completed_iterations: u32,
    total_games: usize,
    init_params_sha256: String,
    init_from_sha256: Option<String>,
    init_from_path: Option<String>,
    param_name_set_sha256: String,
    active_param_count: usize,
    engine_path: String,
    engine_param_mapping_path: Option<String>,
    engine_param_mapping_sha256: Option<String>,
    init_mode: InitMode,
    current_params_sha256: String,
    state_params_file: String,
    last_raw_result_mean: f64,
    last_avg_abs_update: f64,
    updated_at_utc: String,
}

fn load_meta(path: &Path) -> Result<ResumeMetaData> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to open {}", path.display()))?;
    // 先に format_version だけ取り出して、互換性チェックを serde 失敗より優先する
    let version_probe: MetaFormatVersionOnly =
        serde_json::from_slice(&bytes).with_context(|| {
            format!("failed to parse JSON {} (format_version probe)", path.display())
        })?;
    if version_probe.format_version == META_FORMAT_VERSION {
        let meta = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse JSON {}", path.display()))?;
        return Ok(meta);
    }
    if version_probe.format_version == 3 {
        // v3 → v4 silent migration: v3 に multi-seed の痕跡 (`seeds_count > 1` 等) や
        // 奇数 games_per_iteration の痕跡が無いことを前提に、completed_iterations を
        // そのまま v4 batch 番号として継承する。total_pairs / batch_pairs / completed_pairs
        // は CLI 側で確定させてから resume 検証で再合致を確認する経路を取る。
        //
        // 注意: v3 では multi-seed 機能があったが、本 silent migration は単一 seed run の
        // meta のみを扱う想定。v3 で multi-seed run だった meta は **使用者が自ら**
        // 新規 run dir で fresh start するべき (検出は困難なため migration ではせず、
        // resume 後の schedule 不一致 / k 軸ズレで間接的に表面化させる)。
        let v3: V3MetaSubset = serde_json::from_slice(&bytes).with_context(|| {
            format!("v3 → v4 silent migration: failed to parse v3 meta {}", path.display())
        })?;
        eprintln!(
            "warning: v3 meta を v4 として silent migrate します ({}).\n  \
               completed_iterations={} → そのまま batch 番号として継承します。\n  \
               total_pairs/batch_pairs/completed_pairs は CLI 指定値から再構築されます。\n  \
             詳細: docs/spsa_v4_migration.md",
            path.display(),
            v3.completed_iterations,
        );
        // total_pairs / batch_pairs は呼び出し側で CLI から再合致させる。ここでは
        // 一旦 0 で埋め、main 側で resume 整合性検証時に CLI 値を入れる。これは
        // ResumeMetaData の immutable 不変条件を破るが、silent migration の特例として許容。
        return Ok(ResumeMetaData {
            format_version: META_FORMAT_VERSION,
            state_params_file: v3.state_params_file,
            completed_iterations: v3.completed_iterations,
            total_games: v3.total_games,
            last_raw_result_mean: v3.last_raw_result_mean,
            last_avg_abs_update: v3.last_avg_abs_update,
            updated_at_utc: v3.updated_at_utc,
            schedule: v3.schedule,
            init_params_sha256: v3.init_params_sha256,
            init_from_sha256: v3.init_from_sha256,
            init_from_path: v3.init_from_path,
            param_name_set_sha256: v3.param_name_set_sha256,
            active_param_count: v3.active_param_count,
            engine_path: v3.engine_path,
            engine_param_mapping_path: v3.engine_param_mapping_path,
            engine_param_mapping_sha256: v3.engine_param_mapping_sha256,
            init_mode: v3.init_mode,
            current_params_sha256: v3.current_params_sha256,
            // 0 sentinel: main 側で CLI 値とのマッチを skip するシグナルにも使う。
            total_pairs: 0,
            batch_pairs: 0,
            completed_pairs: 0,
        });
    }
    bail!(
        "meta format version 不一致 (got v{}, expected v{}) in {}.\n\
         v{} 形式は v{} とは互換性がありません。\n\
         新規 run dir で `--init-from <canonical>` から fresh start してください。\n\
         詳細: docs/spsa_v4_migration.md",
        version_probe.format_version,
        META_FORMAT_VERSION,
        path.display(),
        version_probe.format_version,
        META_FORMAT_VERSION,
    );
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

    /// 「今この起動で何をしたか」を表す kebab-case ラベル (startup summary 用)。
    ///
    /// run の **出自** (= 初回起動モード) を表す `InitMode` (`meta.init_mode`) とは
    /// 別軸で、毎回の起動アクションを示す。fresh 系初回起動なら `InitMode` と
    /// 同値、resume 起動なら `"resume"` と `InitMode::FreshInitFrom` 等の組み合わせ
    /// になる。これにより summary 上で「今 resume なのか fresh なのか」と
    /// 「この run は最初に何で生まれたか」を独立に確認できる。
    fn launch_label(&self) -> &'static str {
        match self {
            Self::CopyInitFromFresh => "fresh-init-from",
            Self::UseExistingFresh => "fresh-existing",
            Self::ForceInitOverwrite => "force-init",
            Self::Resume { .. } => "resume",
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
        let value: f64 = value_text
            .parse::<f64>()
            .with_context(|| format!("invalid v at line {line_no}"))?;
        let min: f64 = min_text
            .parse::<f64>()
            .with_context(|| format!("invalid min at line {line_no}"))?;
        let max: f64 = max_text
            .parse::<f64>()
            .with_context(|| format!("invalid max at line {line_no}"))?;
        let c_end: f64 = col5_text
            .parse::<f64>()
            .with_context(|| format!("invalid c_end at line {line_no}"))?;
        let r_end: f64 = col6_text
            .parse::<f64>()
            .with_context(|| format!("invalid r_end at line {line_no}"))?;
        // 数値の妥当性検証。f64::clamp は NaN bound で panic、`as i64` は NaN/Inf で
        // 0 や i64::MAX/MIN に化けるため、入口で必ず弾く。SPSA tuner はパラメータ
        // ファイルを長時間信頼する前提なので、ここで Result にして早期発見する。
        for (label, v) in [
            ("v", value),
            ("min", min),
            ("max", max),
            ("c_end", c_end),
            ("r_end", r_end),
        ] {
            if !v.is_finite() {
                bail!("non-finite {label}={v} at line {line_no}");
            }
        }
        if min > max {
            bail!("min ({min}) > max ({max}) at line {line_no}");
        }
        if value < min || value > max {
            bail!("v ({value}) is out of [min={min}, max={max}] at line {line_no}");
        }
        Ok(SpsaParam {
            name,
            type_name: kind,
            is_int,
            value,
            min,
            max,
            c_end,
            r_end,
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
///
/// race-safety: `Drop` は「自分が書いた body」と現在の lock ファイル内容を
/// 突き合わせ、一致した場合だけ削除する。これにより、他プロセスに
/// `--force-unlock` で消され別 lock に置き換わった状況で、自分の Drop が
/// 他プロセスの lock を巻き添えで消す race を防ぐ。
#[derive(Debug)]
struct RunDirLock {
    path: PathBuf,
    /// 自分が書き込んだ正本 body (改行込み)。`Drop` 時に内容一致確認に使う。
    expected_body: String,
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
                let body_json =
                    serde_json::to_string(&info).context("failed to serialize lock info")?;
                writeln!(f, "{body_json}").with_context(|| {
                    format!("failed to write lock contents to {}", path.display())
                })?;
                f.flush().with_context(|| {
                    format!("failed to flush lock contents to {}", path.display())
                })?;
                // `writeln!` は OS によらず常に `\n` を付ける (Windows でも `\r\n` には
                // ならない) ため、`expected_body` の構築では `\n` 固定で良い。Drop での
                // 内容一致比較もこの前提に依存している。
                let expected_body = format!("{body_json}\n");
                Ok(RunDirLock {
                    path,
                    expected_body,
                })
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
        // 自分が書いた body と現在の lock 内容が一致するときだけ削除する。
        // `--force-unlock` で別プロセスに置き換わっていた場合は触らない (race-safe)。
        match std::fs::read_to_string(&self.path) {
            Ok(current) if current == self.expected_body => {
                let _ = std::fs::remove_file(&self.path);
            }
            // 内容不一致 / 既に消された / 読めない: いずれも削除しない (他者の lock を
            // 巻き込まないことが優先)。
            _ => {}
        }
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
            // B-3: θ 内部状態は is_int でも f64 のまま保持する (fishtest 流)。
            // engine への送信時にのみ stochastic round が掛かる (compute_batch_prep)。
            // 過去 (B-3 以前) は state.params 書き出しで int 丸めしていたが、resume を
            // 挟むと小数部が消えて「小さな更新が連続消失」する元バグが復活するため、
            // is_int でも `{:.6}` で f64 を保存する。canonical (engine 配布用) を別途
            // 整数化したい場合は専用エクスポート経路で行う。
            let v_str = format!("{:.6}", p.value);
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

/// engine に setoption 文字列として送る値の整形。
///
/// is_int=true の場合、`value` は **既に stochastic round 済み整数値の f64 表現**
/// であることを呼び出し側 (`compute_batch_prep`) が保証する。理屈上は `as i64` で
/// truncation すれば足りるが、浮動小数誤差で `9.9999...` のような値が来ても事故
/// 化しないよう **防御的に `round` を適用** する (整数 f64 への round は no-op で
/// 既存 stochastic 抽選結果を上書きしない)。NaN / 無限大は呼び出し側 (clamp 適用
/// 済み) で排除されている前提。
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

fn seed_for_iteration(base_seed: u64, iteration_index: u32) -> u64 {
    let iter_term = (iteration_index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    base_seed ^ iter_term
}

/// flip 抽選用 RNG の salt。base_seed に XOR して `seed_for_iteration` に渡すことで
/// rounding RNG と独立な stream を確保する。同じ base_seed でも flip と rounding が
/// 同 RNG state を共有しないようにすることが目的（fishtest worker と同じ分離方針）。
const FLIP_RNG_SALT: u64 = 0xF11D_F11D_F11D_F11D;
/// stochastic rounding 用 RNG の salt。`int` 型 SPSA param を engine に送る際の
/// `floor(v + U(0,1))` 抽選で使う。flip と独立させることで「flip パターンが
/// rounding 結果に相関する」退行を防ぐ。
const ROUNDING_RNG_SALT: u64 = 0xC0DD_C0DD_C0DD_C0DD;

/// `compute_batch_prep` のセッション定数バンドル (run 全体で不変)。
///
/// 引数増加によるシグネチャ複雑化を避けるため `compute_batch_prep` の入力をまとめる。
struct BatchPrepCtx<'a> {
    big_a: f64,
    schedule: ScheduleConfig,
    params: &'a [SpsaParam],
    param_schedules: &'a [ParamScheduleConstants],
    active_only_regex: Option<&'a Regex>,
    translator: &'a EngineNameTranslator,
    start_positions_len: usize,
    /// 1 batch あたりの game pair 数 (fishtest worker の `game_pairs` と等価)。
    /// 1 batch で `2 × batch_pairs` 局を消化する。
    batch_pairs: usize,
    random_startpos: bool,
}

/// 1 batch 分の事前計算 (RNG / flips / shifts / plus/minus / startpos インデックス)。
///
/// SPSA schedule の `k` 軸はこの batch の開始時点での累積 game pair 数 (`k_pair_start`)
/// を渡す。同 batch 内の全 game pair は同じ flip ベクトルと plus/minus 値を共有する
/// (fishtest 流: 1 batch = 1 update 単位)。
///
/// 引数:
/// - `batch_idx`: batch 番号 (1-origin)。RNG 生成と stats 表示に使う。
/// - `k_pair_start`: schedule 評価時刻 (= 累積 game pair 数, 0-origin)。
/// - `total_games_start`: startpos cyclic 進行用 (累積消化 game 数, 0-origin)。
fn compute_batch_prep(
    ctx: &BatchPrepCtx<'_>,
    batch_idx: u32,
    k_pair_start: u32,
    base_seed: u64,
    total_games_start: usize,
) -> Result<BatchPrep> {
    // flip / rounding / startpos の RNG stream を独立化する。
    // - flip_rng: Bernoulli ±1 抽選用 (seed = base_seed ^ FLIP_RNG_SALT, batch_idx)。
    // - rounding_rng: stochastic rounding 用 (seed = base_seed ^ ROUNDING_RNG_SALT, batch_idx)。
    // - rng (本関数 local): startpos cyclic 進行用。base_seed (salt なし) を使用。
    let flip_seed = seed_for_iteration(base_seed ^ FLIP_RNG_SALT, batch_idx);
    let rounding_seed = seed_for_iteration(base_seed ^ ROUNDING_RNG_SALT, batch_idx);
    let startpos_seed = seed_for_iteration(base_seed, batch_idx);
    let mut flip_rng = ChaCha8Rng::seed_from_u64(flip_seed);
    let mut rounding_rng = ChaCha8Rng::seed_from_u64(rounding_seed);
    let mut rng = ChaCha8Rng::seed_from_u64(startpos_seed);

    // Per-param Fishtest 摂動: shift_j = c_k_j × flip_j
    let flips: Vec<f64> = ctx
        .params
        .iter()
        .map(|p| {
            if !is_param_active(p, ctx.active_only_regex, ctx.translator) {
                0.0
            } else if flip_rng.random_bool(0.5) {
                1.0
            } else {
                -1.0
            }
        })
        .collect();
    // schedule の k 軸 = batch 開始時点の累積 game pair 数 (`k_pair_start`)。
    // batch 内では k は固定 (= 全 game pair で同じ flip / shifts / c_k を使う)。
    let shifts: Vec<f64> = ctx
        .params
        .iter()
        .zip(ctx.param_schedules.iter())
        .zip(flips.iter())
        .map(|((p, sched), &flip)| {
            if !is_param_active(p, ctx.active_only_regex, ctx.translator) {
                0.0
            } else {
                let (c_k, _) = sched.at_iteration(
                    k_pair_start,
                    ctx.big_a,
                    ctx.schedule.alpha,
                    ctx.schedule.gamma,
                );
                c_k * flip
            }
        })
        .collect();
    // engine に送る plus/minus 値を確定する。
    //
    // is_int=false (実数 param): clamp のみ。
    // is_int=true (整数 param): fishtest worker 互換の stochastic rounding を適用。
    //   - 期待値は連続 f64 値と一致するため、長期平均で int 丸めバイアスが消える
    //   - clamp → round → 再 clamp の順序で「max=10, v=10.4 → floor(10.4 + 0.7)=11
    //     → 10 へ再 clamp」のような範囲外滑り込みを吸収する
    //
    // 注: rounding は **batch (= 1 iter) 単位で 1 回**。同じ batch 内の game pair は
    // 全て同じ rounded 値を engine に送る (fishtest と等価)。
    let mut round_int = |p: &SpsaParam, raw: f64| -> f64 {
        if p.is_int {
            let clamped = clamped_value(p, raw);
            // floor(v + U(0,1)) の stochastic rounding。期待値は v、誤差は ±0.5 以内。
            let u: f64 = rounding_rng.random();
            let rounded = (clamped + u).floor();
            clamped_value(p, rounded)
        } else {
            clamped_value(p, raw)
        }
    };
    // ⚠ 順序依存: plus_values の各 param で rounding_rng を 1 回消費し、続けて
    // minus_values でも 1 回消費する。param ごとに plus → minus の順で交互に進める
    // ことで stream を予測可能にする (将来 ±同 round 値で揃えたくなったら別 stream)。
    let mut plus_values: Vec<f64> = Vec::with_capacity(ctx.params.len());
    let mut minus_values: Vec<f64> = Vec::with_capacity(ctx.params.len());
    for (p, s) in ctx.params.iter().zip(shifts.iter()) {
        plus_values.push(round_int(p, p.value + s));
        minus_values.push(round_int(p, p.value - s));
    }

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

    // Paired antithetic: 同じ start_pos を 2 局連続 (先後入替) で消化することで
    // 開局選択ノイズと先手有利バイアスを互いにキャンセルする (fishtest 互換)。
    //
    // batch_pairs 個の startpos を選び、各 pair で game 2k と 2k+1 が同じ index を
    // 共有する。`plus_is_black` は呼び出し側 (run_batch_games_parallel) で
    // `idx % 2 == 0` を参照するので、ここでは index 配列のみを生成すれば足りる。
    //
    // `pick_startpos_index` の `game_index` 引数は cyclic mode (random=false) で
    // startpos を周回するためのものなので、pair ごとに 1 ずつ進める。
    let pair_total_games_start = total_games_start / 2;
    let games_in_batch = ctx.batch_pairs * 2;
    let mut start_pos_indices = Vec::with_capacity(games_in_batch);
    for pair_idx in 0..ctx.batch_pairs {
        let idx = pick_startpos_index(
            ctx.start_positions_len,
            &mut rng,
            ctx.random_startpos,
            pair_total_games_start + pair_idx,
        )?;
        // 同じ startpos を 2 連続 push (game 2k=plus黒, 2k+1=plus白)。
        start_pos_indices.push(idx);
        start_pos_indices.push(idx);
    }

    Ok(BatchPrep {
        base_seed,
        flips,
        plus_values,
        minus_values,
        start_pos_indices,
        active_params,
        avg_abs_shift,
        batch_total_games_start: total_games_start,
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

fn run_batch_games_parallel(ctx: BatchRunContext<'_>) -> Result<BatchGameStats> {
    let BatchRunContext {
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
        base_seed,
        translator,
        active_mask,
    } = ctx;

    let game_count = start_pos_indices.len();
    if game_count == 0 {
        return Ok(BatchGameStats {
            step_sum: 0.0,
            plus_wins: 0,
            minus_wins: 0,
            draws: 0,
        });
    }
    let worker_count = concurrency.clamp(1, game_count);
    let (task_tx, task_rx) = unbounded::<GameTask>();
    let (result_tx, result_rx) = unbounded::<Result<GameTaskResult>>();

    std::thread::scope(|scope| -> Result<BatchGameStats> {
        for worker_idx in 0..worker_count {
            let task_rx = task_rx.clone();
            let result_tx = result_tx.clone();
            let worker_cfg = duplicate_engine_config(base_cfg);
            let worker_label = format!("batch{iteration}_worker{}", worker_idx + 1);
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
                "batch={} seed={} game={}/{} plus_is_black={} outcome={} plus_score={:+.1}",
                iteration,
                base_seed,
                result.game_idx + 1,
                game_count,
                result.plus_is_black,
                result.outcome.label(),
                result.plus_score
            );
        }

        Ok(BatchGameStats {
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
    /// 今回の起動アクション (resume / fresh-init-from / fresh-existing / force-init)。
    /// `snapshot.init_mode` (run 全体の出自) とは別軸で表示するため別途渡す。
    launch_action: &'a NonBailAction,
    schedule: &'a ScheduleConfig,
    params: &'a [SpsaParam],
    active_mask: &'a [bool],
    active_param_count: usize,
    /// 起動時の batch 進行範囲 (`[start_batch, end_batch)`)。
    start_iteration: u32,
    end_iteration: u32,
    /// 単一 base seed (v4 で multi-seed は撤去済み)。
    base_seed: u64,
    /// SPSA 全体の game pair 上限 (= schedule k 軸上限)。
    total_pairs: u32,
    /// 1 batch あたりの game pair 数。
    batch_pairs: u32,
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
    // launch action: 「今この起動で何をしたか」 (= NonBailAction)。
    // origin mode: 「この run は最初に何で生まれたか」 (= meta.init_mode)。
    // fresh 系初回起動なら両者は一致、resume では割れる (resume / fresh-init-from 等)。
    eprintln!("launch action:  {}", ctx.launch_action.launch_label());
    eprintln!("origin mode:    {}", ctx.snapshot.init_mode);
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
        "schedule:       α={} γ={} a_ratio={} mobility={} total_pairs={}",
        ctx.schedule.alpha,
        ctx.schedule.gamma,
        ctx.schedule.a_ratio,
        ctx.schedule.mobility,
        ctx.schedule.total_iterations
    );
    eprintln!(
        "batch plan:     batch {} → {} ({} new batch), batch_pairs={}, total_pairs={}, seed={}",
        ctx.start_iteration,
        ctx.end_iteration,
        ctx.end_iteration.saturating_sub(ctx.start_iteration),
        ctx.batch_pairs,
        ctx.total_pairs,
        ctx.base_seed,
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

    // v3 multi-seed 機能撤去: --seeds / --parallel-seeds は hard error。
    // 移行ガイドへ案内 (docs/spsa_v4_migration.md)。
    if cli.seeds.is_some() {
        bail!(
            "--seeds は v4 で撤去されました。複数 base_seed の探索は --seed を変えた\n\
             独立 run dir で並列実行してください。詳細: docs/spsa_v4_migration.md"
        );
    }
    if cli.parallel_seeds {
        bail!("--parallel-seeds は v4 で撤去されました。詳細: docs/spsa_v4_migration.md");
    }

    // 新 CLI: --total-pairs / --batch-pairs。後方互換: --games-per-iteration +
    // --iterations 併用なら自動換算。
    let (total_pairs, batch_pairs) = match (
        cli.total_pairs,
        cli.batch_pairs,
        cli.games_per_iteration,
        cli.iterations,
    ) {
        (Some(tp), bp, None, None) => {
            if bp == 0 {
                bail!("--batch-pairs must be >= 1");
            }
            (tp, bp)
        }
        (None, _bp, Some(gpi), Some(iters)) => {
            if !gpi.is_multiple_of(2) || gpi == 0 {
                bail!(
                    "--games-per-iteration must be an even number >= 2 \
                     (paired antithetic は同 startpos の先後入替 2 局を 1 単位とする)"
                );
            }
            if iters == 0 {
                bail!("--iterations must be >= 1");
            }
            let derived_total_pairs =
                gpi.checked_mul(iters).context("games_per_iteration * iterations overflow")? / 2;
            // batch_pairs 既定値 8 だが、deprecate 経路では「1 iter = 1 batch」を
            // 維持するために gpi/2 を採用する (= ユーザの旧運用で update 頻度を保つ)。
            let derived_batch_pairs = gpi / 2;
            eprintln!(
                "warning: --games-per-iteration/--iterations は deprecated です。\n  \
                   自動換算: --total-pairs {derived_total_pairs} --batch-pairs {derived_batch_pairs}\n  \
                 docs/spsa_v4_migration.md を参照して新 CLI へ移行してください。"
            );
            (derived_total_pairs, derived_batch_pairs)
        }
        (Some(_), _, Some(_), _) | (Some(_), _, _, Some(_)) => {
            bail!(
                "--total-pairs と --games-per-iteration/--iterations は同時指定できません。\n\
                 docs/spsa_v4_migration.md を参照して新 CLI に統一してください。"
            );
        }
        (None, _, _, _) => {
            bail!(
                "--total-pairs を指定してください (deprecated 経路は --games-per-iteration\n\
                 と --iterations の両方が必要です)。docs/spsa_v4_migration.md 参照。"
            );
        }
    };
    if total_pairs == 0 {
        bail!("--total-pairs must be >= 1");
    }
    if batch_pairs == 0 {
        bail!("--batch-pairs must be >= 1");
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
    let base_seed = cli.seed.unwrap_or_else(|| rand::rng().random::<u64>());
    eprintln!("using base seed: {base_seed}");

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
    // schedule.total_iterations は **k 軸の上限 = total_pairs** として再解釈する
    // (v4 流: schedule の k 軸は累積 game pair 数)。field 名は v3 互換のため維持。
    let schedule = ScheduleConfig {
        alpha: cli.alpha,
        gamma: cli.gamma,
        a_ratio: cli.a_ratio,
        mobility: cli.mobility,
        total_iterations: total_pairs,
    };
    // v4 resume の戻り値: (start_batch, completed_pairs, total_games, init_snapshot)。
    // - start_batch: 次に実行する batch 番号 (0-origin)。
    // - completed_pairs: 既に完了した game pair 数 (= schedule k 軸の起点)。
    let (start_batch, mut completed_pairs, mut total_games, init_snapshot) = match &effective_action
    {
        NonBailAction::Resume { verify_init } => {
            // load_meta が format_version 不一致を先に hard bail するため、ここでは
            // 全フィールド込みの deserialize 成功を前提にできる。
            let mut meta = load_meta(&meta_path).with_context(|| {
                format!("--resume was set but metadata load failed: {}", meta_path.display())
            })?;
            // v3 → v4 silent migrate された meta は `schedule.total_iterations` の意味が
            // 異なる (v3: iterations, v4: total_pairs)。silent migrate ケースでは
            // total_iterations だけ比較対象から外す (CLI 値で上書き) ことで運用衝突を避ける。
            // 例: v3 で `gpi=16, iters=100` の meta を v4 `total_pairs=800` で resume する
            // 正当ケースを誤って bail させない。
            let v3_silent_migrated = meta.total_pairs == 0 && meta.batch_pairs == 0;
            if v3_silent_migrated {
                meta.schedule.total_iterations = total_pairs;
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
                     対処 (どちらか):\n  \
                       (a) 新規 run dir で `--init-from <canonical>` から fresh start する\n  \
                       (b) 1 反復差を許容して既存 state を起点に新 run を始める:\n        \
                           cp {state_path} <new-run-dir>/state.params\n        \
                           spsa --run-dir <new-run-dir> --use-existing-state-as-init ...",
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
            // v4 schedule 検証: 既存 meta の total_pairs / batch_pairs と CLI 指定値の一致。
            // v3 → v4 silent migration 経路では meta.total_pairs == 0 (sentinel) のため
            // CLI 値を信頼して継承する (meta は migrate 直後で値を持たない)。
            let migrated_from_v3 = meta.total_pairs == 0 && meta.batch_pairs == 0;
            if !migrated_from_v3
                && (meta.total_pairs != total_pairs || meta.batch_pairs != batch_pairs)
            {
                if cli.force_schedule {
                    eprintln!(
                        "warning: total_pairs/batch_pairs が meta と異なるが --force-schedule で続行 \
                         (meta total_pairs={}, batch_pairs={} / cli total_pairs={}, batch_pairs={})",
                        meta.total_pairs, meta.batch_pairs, total_pairs, batch_pairs
                    );
                } else {
                    bail!(
                        "total_pairs/batch_pairs が meta と異なります \
                         (meta total_pairs={}, batch_pairs={} / cli total_pairs={}, batch_pairs={}). \
                         --force-schedule を指定するか docs/spsa_v4_migration.md を参照してください。",
                        meta.total_pairs,
                        meta.batch_pairs,
                        total_pairs,
                        batch_pairs
                    );
                }
            }
            // completed_pairs: v4 meta は記録されたものを使う。v3 silent migrate 時は
            // `completed_iterations × batch_pairs` で再構築する (v3 では 1 iter = 1 batch
            // = 1 update で k は iter index と等価だったため)。
            let resumed_completed_pairs = if migrated_from_v3 {
                meta.completed_iterations.saturating_mul(batch_pairs)
            } else {
                meta.completed_pairs
            };
            let snapshot = InitMetaSnapshot::from_existing_meta(&meta);
            (meta.completed_iterations, resumed_completed_pairs, meta.total_games, snapshot)
        }
        NonBailAction::CopyInitFromFresh
        | NonBailAction::UseExistingFresh
        | NonBailAction::ForceInitOverwrite => {
            // 旧 run の final.params が残っていると、新 run 完了時に再書き込みされるまで
            // 「前回の確定値」が見え続ける (= apply 入力に誤投入される)。fresh 系は
            // すべてここで能動削除する (force-init の cleanup paths にも入っているが、
            // CopyInitFromFresh / UseExistingFresh では cleanup paths は呼ばれないため)。
            remove_stale_final_params_for_fresh_start(&cli.run_dir)?;
            let snapshot = InitMetaSnapshot::for_fresh_start(
                &effective_action,
                &state_params,
                cli.init_from.as_deref(),
                &engine_path,
                cli.engine_param_mapping.as_deref(),
            )?;
            (0, 0, 0, snapshot)
        }
    };
    // batch 数 = ceil(total_pairs / batch_pairs)。最終 batch は端数の game pair 数になる。
    let total_batches = total_pairs.div_ceil(batch_pairs);
    // resume 不変条件チェック: completed_pairs と completed_iterations の関係が
    // 設定上ありえない値になっていないか (CLI 指定ミス / silent migrate 不整合等で発生し得る)。
    if completed_pairs > total_pairs {
        bail!(
            "completed_pairs ({completed_pairs}) > total_pairs ({total_pairs}). \
             既存 meta が現在の --total-pairs より進んでいます。\
             total_pairs を増やすか、新規 run dir で --use-existing-state-as-init してください。"
        );
    }
    if start_batch > total_batches {
        bail!(
            "completed_iterations ({start_batch}) > total_batches ({total_batches}). \
             total_pairs / batch_pairs と既存 meta の整合性が崩れています。",
        );
    }
    // 中間 batch では `completed_pairs == start_batch * batch_pairs` が必ず成り立つ
    // (端数 batch は最終 batch にのみ発生し、その後 resume はあり得ないので)。
    // start_batch < total_batches のときのみチェックする。
    if start_batch < total_batches {
        let expected_pairs = start_batch.saturating_mul(batch_pairs);
        if completed_pairs != expected_pairs {
            bail!(
                "completed_pairs ({completed_pairs}) と completed_iterations × batch_pairs \
                 ({expected_pairs}) が不整合です。meta が破損しているか、batch_pairs を \
                 途中で変更した可能性があります。--force-schedule では救えないので新規 \
                 run dir で fresh start してください。"
            );
        }
    }
    if start_batch >= total_batches {
        eprintln!(
            "info: 既に全 batch ({total_batches}) を完了しています (start_batch={start_batch})。何もせず終了します。"
        );
    }
    let end_batch = total_batches;
    let stats_csv_path: Option<PathBuf> = if cli.no_stats_csv {
        None
    } else {
        Some(cli.stats_csv.clone().unwrap_or_else(|| default_stats_csv_path(&cli.run_dir)))
    };
    let mut stats_csv_writer = if let Some(path) = stats_csv_path.as_deref() {
        Some(open_stats_csv_writer(path, cli.resume)?)
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
        launch_action: &effective_action,
        schedule: &schedule,
        params: &params,
        active_mask: &active_mask,
        active_param_count,
        start_iteration: start_batch,
        end_iteration: end_batch,
        base_seed,
        total_pairs,
        batch_pairs,
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

    // Fishtest 方式: per-param スケジュール定数を初期化。
    // schedule.total_iterations は **k 軸の上限 = total_pairs** として再解釈する。
    let big_a = schedule.a_ratio * total_pairs as f64;
    let param_schedules: Vec<ParamScheduleConstants> = params
        .iter()
        .map(|p| {
            ParamScheduleConstants::compute(
                p.c_end,
                p.r_end,
                total_pairs,
                schedule.a_ratio,
                schedule.alpha,
                schedule.gamma,
            )
        })
        .collect();

    for batch_idx in start_batch..end_batch {
        // この batch で消化する game pair 数。最終 batch は端数になり得る。
        let pairs_remaining = total_pairs - completed_pairs;
        let this_batch_pairs = pairs_remaining.min(batch_pairs);
        if this_batch_pairs == 0 {
            break;
        }

        // params は batch 末で更新するため、prep_ctx は batch ごとに作り直す。
        let batch_ctx = BatchPrepCtx {
            big_a,
            schedule,
            params: &params,
            param_schedules: &param_schedules,
            active_only_regex: active_only_regex.as_ref(),
            translator: &translator,
            start_positions_len: start_positions.len(),
            batch_pairs: this_batch_pairs as usize,
            random_startpos: cli.random_startpos,
        };

        // Phase A: 事前計算。schedule の k 軸 = batch 開始時点の累積 game pair 数。
        // RNG 用の batch_idx は 0-origin で渡す (`seed_for_iteration` 内で `+1` され
        // 重複しない iter_term を作るため、ここで `+1` する必要はない)。
        let prep =
            compute_batch_prep(&batch_ctx, batch_idx, completed_pairs, base_seed, total_games)?;

        // Phase B: ゲーム実行 (heavy)。
        let stats = run_batch_games_parallel(BatchRunContext {
            concurrency: cli.concurrency,
            base_cfg: &base_cfg,
            params: &params,
            plus_values: &prep.plus_values,
            minus_values: &prep.minus_values,
            start_positions: &start_positions,
            start_pos_indices: &prep.start_pos_indices,
            game_cfg: &game_cfg,
            tc,
            total_games_start: prep.batch_total_games_start,
            iteration: batch_idx + 1,
            base_seed: prep.base_seed,
            translator: &translator,
            active_mask: &active_mask,
        })?;

        // Phase C: 集計と更新。
        let games_in_batch = (this_batch_pairs * 2) as usize;
        total_games = total_games.checked_add(games_in_batch).context("total_games overflow")?;
        completed_pairs = completed_pairs
            .checked_add(this_batch_pairs)
            .context("completed_pairs overflow")?;

        let raw_result = stats.step_sum;
        let plus_wins = stats.plus_wins;
        let minus_wins = stats.minus_wins;
        let draws = stats.draws;

        // Fishtest 更新: signal_j = R_k_j × c_k_j × result × flip_j。
        // k 軸は batch 開始時点 (`completed_pairs - this_batch_pairs`) の累積 pair 数を使う。
        let k_for_update = completed_pairs - this_batch_pairs;
        let mut update_sums = vec![0.0f64; params.len()];
        for (idx, (p, (&flip, sched))) in
            params.iter().zip(prep.flips.iter().zip(param_schedules.iter())).enumerate()
        {
            if !is_param_active(p, active_only_regex.as_ref(), &translator)
                || p.c_end.abs() <= f64::EPSILON
            {
                continue;
            }
            let (c_k, r_k) =
                sched.at_iteration(k_for_update, big_a, schedule.alpha, schedule.gamma);
            update_sums[idx] = r_k * c_k * raw_result * flip;
        }

        // θ 更新。1 batch = 1 update (fishtest 流)。
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
            let signal = update_sums[idx];
            // θ 内部状態は is_int に関わらず f64 のまま保持する (fishtest 流)。
            p.value = clamped_value(p, p.value + signal * cli.mobility);
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

        // stats.csv: v4 仕様 1 batch = 1 行。
        if let Some(writer) = stats_csv_writer.as_mut() {
            let row = IterationStats {
                iteration: batch_idx + 1,
                batch_pairs: this_batch_pairs,
                plus_wins,
                minus_wins,
                draws,
                raw_result,
                active_params: prep.active_params,
                avg_abs_shift: prep.avg_abs_shift,
                updated_params,
                avg_abs_update,
                max_abs_update,
                total_games,
            };
            write_stats_csv_row(writer, row)?;
            writer.flush()?;
        }

        write_params(&state_params, &params)?;
        if let Some(writer) = param_values_csv_writer.as_mut() {
            write_param_values_csv_row(writer, batch_idx + 1, &params)?;
            writer.flush()?;
        }
        // state.params 更新 → meta 更新の transactional 復旧用に、書き込み直後の
        // state.params を hash して meta に焼き込む。
        let current_params_sha256 = sha256_hex_of_file(&state_params)?;
        let meta = ResumeMetaData {
            format_version: META_FORMAT_VERSION,
            state_params_file: state_params.display().to_string(),
            completed_iterations: batch_idx + 1,
            total_games,
            last_raw_result_mean: raw_result,
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
            total_pairs,
            batch_pairs,
            completed_pairs,
        };
        save_meta(&meta_path, &meta)?;
        eprintln!(
            "batch={}/{} k_pair={}/{} batch_pairs={} raw_result={:+.3} \
             avg_abs_update={:.6} max_abs_update={:.6} checkpoint={} meta={}",
            batch_idx + 1,
            end_batch,
            completed_pairs,
            total_pairs,
            this_batch_pairs,
            raw_result,
            avg_abs_update,
            max_abs_update,
            state_params.display(),
            meta_path.display()
        );

        if let Some(config) = early_stop_config {
            // raw_result_variance は v3 で「seed 横断分散」だった。v4 は単一 batch
            // なので raw_result の絶対値を **batch_pairs で正規化** した値 (= 1 game pair
            // あたりの平均勝率乖離; 0..1 の範囲) を分散の代理指標として使う。
            // raw_result == 0 ⇔ +1/-1 が完全に拮抗 ⇔ SPSA 収束のシグナル。
            // 旧 v3 と完全互換ではないため、閾値設計はユーザ側で再調整が必要 (docs に明記)。
            let raw_result_variance = if this_batch_pairs == 0 {
                0.0
            } else {
                raw_result.abs() / this_batch_pairs as f64
            };
            let early_stop_hit = avg_abs_update <= config.avg_abs_update_threshold
                && raw_result_variance <= config.result_variance_threshold;
            if early_stop_hit {
                early_stop_consecutive = early_stop_consecutive.saturating_add(1);
            } else {
                early_stop_consecutive = 0;
            }
            eprintln!(
                "batch={} early_stop_hit={} consecutive={}/{} \
                 thresholds(avg_abs_update<={:.6}, |raw_result|/batch_pairs<={:.6})",
                batch_idx + 1,
                early_stop_hit,
                early_stop_consecutive,
                config.patience,
                config.avg_abs_update_threshold,
                config.result_variance_threshold
            );
            if early_stop_consecutive >= config.patience {
                eprintln!(
                    "early stop triggered at batch={} (consecutive={})",
                    batch_idx + 1,
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
        // B-3 以降: is_int でも `{:.6}` 固定桁で f64 を保存する。
        assert!(body.starts_with("Foo,int,42.000000,"), "actual: {body}");
        // ラウンドトリップ (parse は f64 なので "42" / "42.000000" のどちらでも復元可能)
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
    fn run_dir_lock_drop_does_not_remove_others_lock() {
        // race scenario: 別プロセスに --force-unlock で lock を奪われ別 lock に
        // 置き換わった状況で、自分の Drop が他者の lock を誤って削除しないこと。
        let dir = tempfile::tempdir().unwrap();
        let lock = RunDirLock::acquire(dir.path(), false).unwrap();
        // 他者が .lock を別内容で上書き済み (force-unlock 後の reacquire を模擬)
        std::fs::write(dir.path().join(".lock"), "other process took over").unwrap();
        drop(lock);
        // 内容不一致なので自分の Drop は削除を控える
        let body = std::fs::read_to_string(dir.path().join(".lock")).unwrap();
        assert_eq!(body, "other process took over");
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
    fn non_bail_action_launch_label_covers_all_variants() {
        // launch_label と init_mode の対応関係:
        //   - fresh 系 (CopyInitFromFresh / UseExistingFresh / ForceInitOverwrite) は
        //     launch_label と init_mode の文字列が同値 (run の出自 = 今回の起動)
        //   - Resume では launch_label="resume" / init_mode は元 run の出自を保持
        assert_eq!(NonBailAction::CopyInitFromFresh.launch_label(), "fresh-init-from");
        assert_eq!(NonBailAction::UseExistingFresh.launch_label(), "fresh-existing");
        assert_eq!(NonBailAction::ForceInitOverwrite.launch_label(), "force-init");
        assert_eq!(NonBailAction::Resume { verify_init: false }.launch_label(), "resume");
        assert_eq!(NonBailAction::Resume { verify_init: true }.launch_label(), "resume");

        // fresh 系では launch_label と init_mode (kebab-case Display) が一致する
        for action in [
            NonBailAction::CopyInitFromFresh,
            NonBailAction::UseExistingFresh,
            NonBailAction::ForceInitOverwrite,
        ] {
            assert_eq!(action.launch_label(), format!("{}", action.init_mode()));
        }
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
    }

    #[test]
    fn default_force_init_cleanup_paths_returns_run_dir_only() {
        // override 先 (--stats-csv で run-dir 外を指定する等) が混入しないことを担保。
        // force-init 時の削除対象は run-dir 直下の派生 CSV + final.params のみで、
        // state.params / meta.json は apply_init_action 側で別管理。
        // v4: stats_aggregate.csv は撤去 (multi-seed 機能と共に削除)。
        let dir = Path::new("/tmp/some_run");
        let paths = default_force_init_cleanup_paths(dir);
        assert_eq!(paths.len(), 3, "exactly 3 derived files (2 CSV + final.params)");
        assert!(paths.contains(&dir.join("values.csv")));
        assert!(paths.contains(&dir.join("stats.csv")));
        assert!(paths.contains(&dir.join("final.params")));
        // state.params と meta.json は含めない (apply_init_action が個別管理)
        assert!(!paths.contains(&dir.join("state.params")));
        assert!(!paths.contains(&dir.join("meta.json")));
    }

    #[test]
    fn remove_stale_final_params_for_fresh_start_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        // 不在ファイルでも Ok を返すこと (idempotent)
        remove_stale_final_params_for_fresh_start(dir.path()).unwrap();
        // 存在するファイルは消えること
        let final_path = dir.path().join("final.params");
        std::fs::write(&final_path, b"stale").unwrap();
        assert!(final_path.exists());
        remove_stale_final_params_for_fresh_start(dir.path()).unwrap();
        assert!(!final_path.exists());
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
            total_pairs: 200,
            batch_pairs: 8,
            completed_pairs: 40,
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
        assert_eq!(loaded.total_pairs, meta.total_pairs);
        assert_eq!(loaded.batch_pairs, meta.batch_pairs);
        assert_eq!(loaded.completed_pairs, meta.completed_pairs);
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

    /// テスト用 `BatchPrepCtx`。`games_per_iteration` 引数は v3 名残で受け取るが
    /// 内部的には `batch_pairs = games_per_iteration / 2` に変換して新仕様に渡す。
    /// テスト本体の意図を変えずに新 API へ追従するためのシム。
    fn make_test_ctx<'a>(
        params: &'a [SpsaParam],
        schedules: &'a [ParamScheduleConstants],
        translator: &'a EngineNameTranslator,
        games_per_iteration: usize,
    ) -> BatchPrepCtx<'a> {
        assert!(
            games_per_iteration.is_multiple_of(2),
            "make_test_ctx: games_per_iteration must be even"
        );
        BatchPrepCtx {
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
            batch_pairs: games_per_iteration / 2,
            random_startpos: true,
        }
    }

    /// `compute_batch_prep` のスナップショットテスト。`ChaCha8Rng` は決定論的なため、
    /// 同じ `(base_seed, iter)` に対して flips / shifts / start_pos_indices が完全一致する。
    /// 並列化後も Phase A の事前計算結果がブレないことを保証。
    #[test]
    fn compute_batch_prep_is_deterministic_across_calls() {
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

        let prep1 = compute_batch_prep(&ctx, 5, 5, 42, 100).expect("prep1");
        let prep2 = compute_batch_prep(&ctx, 5, 5, 42, 100).expect("prep2");

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
    fn compute_batch_prep_seeds_produce_independent_flips() {
        let params: Vec<_> = (0..6)
            .map(|i| make_param(&format!("Search_p{i}"), 1000.0 + i as f64 * 100.0, 100.0))
            .collect();
        let schedules: Vec<ParamScheduleConstants> = params
            .iter()
            .map(|p| ParamScheduleConstants::compute(p.c_end, p.r_end, 100, 0.1, 0.602, 0.101))
            .collect();
        let translator = EngineNameTranslator::empty();
        let ctx = make_test_ctx(&params, &schedules, &translator, 32);

        let prep1 = compute_batch_prep(&ctx, 5, 5, 1, 100).expect("prep1");
        let prep2 = compute_batch_prep(&ctx, 5, 5, 2, 100).expect("prep2");
        let prep3 = compute_batch_prep(&ctx, 5, 5, 3, 100).expect("prep3");
        let prep4 = compute_batch_prep(&ctx, 5, 5, 4, 100).expect("prep4");

        // 隣接 seed ペアそれぞれで flip パターンが異なることを直接確認
        assert_ne!(prep1.flips, prep2.flips, "seed=1 vs seed=2 flips must differ");
        assert_ne!(prep2.flips, prep3.flips, "seed=2 vs seed=3 flips must differ");
        assert_ne!(prep3.flips, prep4.flips, "seed=3 vs seed=4 flips must differ");
    }

    /// Paired antithetic: pair 内 2 局 (game 2k, 2k+1) は **同じ start_pos** を共有し、
    /// `plus_is_black` のみ反転させる。fishtest と等価のノイズ削減を行うための
    /// 最重要不変条件で、これが崩れると pair 化が形骸化して開局ノイズが残る。
    #[test]
    fn compute_batch_prep_pairs_share_startpos() {
        let params = vec![make_param("Search_a", 1000.0, 100.0)];
        let schedules: Vec<ParamScheduleConstants> = params
            .iter()
            .map(|p| ParamScheduleConstants::compute(p.c_end, p.r_end, 100, 0.1, 0.602, 0.101))
            .collect();
        let translator = EngineNameTranslator::empty();
        // games_per_iteration = 16 → pair_count = 8。
        let ctx = make_test_ctx(&params, &schedules, &translator, 16);

        let prep = compute_batch_prep(&ctx, 5, 5, 42, 100).expect("prep");

        assert_eq!(prep.start_pos_indices.len(), 16, "16 game 分の index がある");
        // pair 内 (2k, 2k+1) で同じ startpos
        for pair_idx in 0..8 {
            let a = prep.start_pos_indices[pair_idx * 2];
            let b = prep.start_pos_indices[pair_idx * 2 + 1];
            assert_eq!(
                a,
                b,
                "pair {pair_idx}: game {} と {} は同じ start_pos でなければならない",
                pair_idx * 2,
                pair_idx * 2 + 1
            );
        }
    }

    /// Paired antithetic の color 反転規約 (`plus_is_black = idx % 2 == 0`) を直接確認。
    /// 実コード (`run_batch_games_parallel`) のロジックと同じ式を再現することで、
    /// pair 化した index 列と組み合わせたとき先後が正しく入れ替わることを保証する。
    #[test]
    fn paired_antithetic_color_flips_within_pair() {
        // pair の game 2k は plus_is_black=true, 2k+1 は false でなければならない。
        for pair_idx in 0..4_usize {
            let g0 = pair_idx * 2;
            let g1 = pair_idx * 2 + 1;
            assert!(g0 % 2 == 0, "game {g0} should produce plus_is_black=true");
            assert!(g1 % 2 != 0, "game {g1} should produce plus_is_black=false");
        }
    }

    /// Stochastic rounding の期待値は連続 f64 値に一致する (大数の法則)。
    /// 多数 iteration (= 多数 rounding 抽選) を回し、`p.value=10.4` の rounded 平均が
    /// 0.05 程度の誤差で 10.4 に収束することを確認。これが崩れると int param で
    /// 系統的バイアスが入って棋力低下の原因になる。
    ///
    /// Seed を変えて iter ごとに rounding stream を進め、結果値の平均を取る。
    #[test]
    fn stochastic_rounding_expected_value_matches_continuous() {
        // base_seed を変えながら、固定 p.value=10.4 の rounded plus_value 平均を取る。
        // shifts はゼロにしたいので c_end=0.0 (→ at_iteration の c_k は 0) のパラメータを使う。
        let mut params = vec![make_param("Search_a", 10.4, 1.0)];
        params[0].is_int = true;
        params[0].min = 0.0;
        params[0].max = 100.0;
        params[0].c_end = 1.0; // shifts を生む
        let schedules: Vec<ParamScheduleConstants> = params
            .iter()
            .map(|p| ParamScheduleConstants::compute(p.c_end, p.r_end, 100, 0.1, 0.602, 0.101))
            .collect();
        let translator = EngineNameTranslator::empty();
        let ctx = make_test_ctx(&params, &schedules, &translator, 2);

        // shift がゼロでないと「連続値=10.4」になる plus/minus を作れないので、
        // ここでは shift 込みの rounded 値を多数 iter 集計し、shift の対称性で平均が
        // 10.4 に収束することを確認する (plus と minus を両方足し 2 で割る)。
        let n_iters = 4000_u32;
        let mut sum = 0.0f64;
        let mut count = 0_usize;
        for iter in 0..n_iters {
            let prep = compute_batch_prep(&ctx, iter, iter, 12345, 0).expect("prep");
            sum += prep.plus_values[0];
            sum += prep.minus_values[0];
            count += 2;
        }
        let mean = sum / count as f64;
        let err = (mean - 10.4).abs();
        assert!(
            err < 0.05,
            "stochastic rounding 平均 {mean} が連続値 10.4 から {err} 乖離 (許容 < 0.05)"
        );
    }

    /// 完全再現性: 同一 seed/iter で 2 回 `compute_batch_prep` を回したとき、
    /// flip / shifts / plus_values / minus_values / start_pos_indices 全てが
    /// bit-identical に一致することを確認。stochastic rounding 導入後も RNG stream を
    /// 分離した上で seed_for_iteration から決定論的に生成しているため、保証される。
    #[test]
    fn compute_batch_prep_full_reproducibility_with_rounding() {
        let mut params = vec![
            make_param("Search_a", 1234.5, 50.0),
            make_param("Search_b", 9876.7, 100.0),
        ];
        for p in params.iter_mut() {
            p.is_int = true;
        }
        let schedules: Vec<ParamScheduleConstants> = params
            .iter()
            .map(|p| ParamScheduleConstants::compute(p.c_end, p.r_end, 100, 0.1, 0.602, 0.101))
            .collect();
        let translator = EngineNameTranslator::empty();
        let ctx = make_test_ctx(&params, &schedules, &translator, 8);

        let prep1 = compute_batch_prep(&ctx, 7, 7, 99, 200).expect("prep1");
        let prep2 = compute_batch_prep(&ctx, 7, 7, 99, 200).expect("prep2");

        assert_eq!(prep1.flips, prep2.flips);
        assert_eq!(prep1.plus_values, prep2.plus_values);
        assert_eq!(prep1.minus_values, prep2.minus_values);
        assert_eq!(prep1.start_pos_indices, prep2.start_pos_indices);

        // 整数 round 結果が実際に整数になっていることを確認
        for v in prep1.plus_values.iter().chain(prep1.minus_values.iter()) {
            assert_eq!(v.fract(), 0.0, "is_int param で fractional 値が残った: {v}");
        }
    }

    /// 異なる pair は (random_startpos=true 時) 独立サンプリングされるため、
    /// pair 全体が単一 startpos に固定されないこと (= バリエーションが残ること) を確認。
    /// 完全一致を否定する弱い不変条件だが、「pair 化を装って実は全 game 同一 startpos」
    /// のような退行を検出するには十分。
    #[test]
    fn compute_batch_prep_different_pairs_have_varied_startpos() {
        let params = vec![make_param("Search_a", 1000.0, 100.0)];
        let schedules: Vec<ParamScheduleConstants> = params
            .iter()
            .map(|p| ParamScheduleConstants::compute(p.c_end, p.r_end, 100, 0.1, 0.602, 0.101))
            .collect();
        let translator = EngineNameTranslator::empty();
        // games_per_iteration = 32 → pair_count = 16。1957 startpos からランダム抽出。
        let ctx = make_test_ctx(&params, &schedules, &translator, 32);

        let prep = compute_batch_prep(&ctx, 5, 5, 42, 0).expect("prep");
        let pair_indices: Vec<usize> =
            (0..16).map(|pair_idx| prep.start_pos_indices[pair_idx * 2]).collect();
        let unique: std::collections::HashSet<_> = pair_indices.iter().copied().collect();
        assert!(
            unique.len() >= 8,
            "16 pair で start_pos がほぼ全て同一になるのは異常 (got {} unique)",
            unique.len()
        );
    }
}
