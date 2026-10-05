//! csa_client が per-game に吐く JSONL(`meta`/`move`/`result` 行)から、ある
//! エンジンの戦績を集計する CLI。floodgate 連続対局のように相手が毎局変わる
//! ログ群を、先後別・相手別・非勝一覧・実戦 NPS で要約する。
//!
//! JSONL は両対局者の指し手を `engine` 名付きで記録するので、手番・相手は move 行から
//! 判定できる(ply1 の `engine` が先手)。後手が 1 手も指さず負けた局だけは move に相手が
//! 現れないため、ファイル名 `..._vs_{gote}` から相手を補完する(取りこぼし防止)。
//!
//! # 例
//! ```text
//! # ~/floodgate/records/jsonl を集計(対象エンジンは自動判定)
//! floodgate_record --dir ~/floodgate/records/jsonl
//!
//! # csa_client と同じ config から dir / --me / キャッシュ既定を導出(明示引数が優先)
//! floodgate_record --config ~/floodgate/active.toml --fetch-ratings
//!
//! # 対象エンジンを明示し、注目相手を指定
//! floodgate_record --dir ./jsonl --me RAMU_TF --watch Suisho,dlshogi,nshogi
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rshogi_csa_client::config::CsaClientConfig;
use rshogi_csa_client::jsonl::sanitize_for_filename;
use serde::Deserialize;
use tools::common::floodgate as fg;
use tools::common::io::write_atomic;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Aggregate an engine's win/loss record from csa_client per-game JSONL"
)]
struct Cli {
    /// csa_client と同一の TOML 設定ファイル。指定時は `record` 設定から集計 dir を、
    /// `server.id` から --me を、`record.dir` からレートキャッシュ/履歴の既定パスを導出する
    /// (対応する明示引数が常に優先)。省略時は環境変数 `CSA_CLIENT_CONFIG` を参照する。
    /// TOML 内の相対パスは config ファイルのあるディレクトリ基準で解決する。
    #[arg(long)]
    config: Option<PathBuf>,

    /// 集計対象 JSONL(`*.jsonl`)のあるディレクトリ(再帰なし)。省略時は --config の
    /// `record` 設定から導出し、それも無ければカレントディレクトリ。
    #[arg(long)]
    dir: Option<PathBuf>,

    /// 集計対象エンジン名。省略時は --config の `server.id`、それも無ければ
    /// 全 JSONL に最も多く出現するエンジンを自動判定。
    #[arg(long)]
    me: Option<String>,

    /// 注目相手(カンマ区切り・部分一致)。指定時のみ「注目相手との対戦」節を出力。
    #[arg(long, value_delimiter = ',')]
    watch: Vec<String>,

    /// 指定すると wdoor floodgate の現在レートを取得し、自分/相手に ` (R<rate>)` を併記する。
    /// per-game でなく現在値。ネットワークが要る(取得・解析は `tools::common::floodgate`)。
    #[arg(long)]
    fetch_ratings: bool,

    /// `--fetch-ratings` と併用するキャッシュファイル(`name<TAB>rate`)。fetch 成功時はここへ
    /// 書き出し、失敗時はここを読み戻してフォールバック併記する(一時障害でも直近値を維持)。
    /// --config 指定時の既定は `<record.dir>/ratings_cache.tsv`。
    #[arg(long, requires = "fetch_ratings")]
    ratings_cache: Option<PathBuf>,

    /// キャッシュがこの秒数以内に更新済みならネットワーク取得をスキップして
    /// キャッシュを直接使う(0 = 常に取得)。レートページは日次生成なので
    /// 数時間 (例: 21600 = 6h) で十分。履歴併用時、履歴に当日分が無ければ
    /// 鮮度内でも取得する(履歴を欠かさないため)。
    #[arg(long, default_value_t = 0, requires = "fetch_ratings")]
    ratings_max_age: u64,

    /// fetch 成功時に自分のレートを `ページ日付<TAB>名前<TAB>レート` で追記する履歴
    /// ファイル。同一 (日付, 名前) は再追記しない(その日最初の観測値を記録)。
    /// --config 指定時の既定は `<record.dir>/ratings_history.tsv`。
    #[arg(long, requires = "fetch_ratings")]
    ratings_history: Option<PathBuf>,
}

/// CLI と config から解決した実効入力。優先順位は常に 明示 CLI > config 由来 > 既定。
struct Effective {
    dir: PathBuf,
    /// 対象エンジン名。`None` は最頻出エンジンの自動判定に委ねる。
    me: Option<String>,
    ratings_cache: Option<PathBuf>,
    ratings_history: Option<PathBuf>,
}

fn resolve_effective(cli: &Cli, config: Option<(&Path, &CsaClientConfig)>) -> Result<Effective> {
    // TOML 内の相対パスは csa_client 実行時の cwd ではなく config ファイル基準で解決する
    // (集計は別 cwd から叩かれるため。運用 config は絶対パス推奨)。csa_client の CLI
    // 上書き (--record-dir 等) はここからは見えず、TOML の値のみを使う。
    fn resolve(base: &Path, p: PathBuf) -> PathBuf {
        if p.is_absolute() { p } else { base.join(p) }
    }
    let based = config.map(|(path, cfg)| (path, path.parent().unwrap_or(Path::new(".")), cfg));

    let dir = match (&cli.dir, based) {
        (Some(d), _) => d.clone(),
        (None, Some((path, base, cfg))) => match cfg.record.jsonl_dir() {
            Some(d) => resolve(base, d),
            None => bail!(
                "config {} は JSONL 出力が無効 (record.enabled / record.save_jsonl を確認)。\
                 集計元が導出できないため --dir で明示すること",
                path.display()
            ),
        },
        (None, None) => PathBuf::from("."),
    };

    // 集計キーは sanitize 名に統一している(parse_game 参照)ので、CLI / server.id
    // 由来の me も同じ正規化を通す(英数字と `-` `_` には no-op)。
    let me = cli
        .me
        .as_deref()
        .or_else(|| based.map(|(_, _, c)| c.server.id.as_str()).filter(|id| !id.is_empty()))
        .map(sanitize_for_filename);

    // キャッシュ / 履歴の config 既定は --fetch-ratings 時のみ意味を持つ(clap の
    // `requires` と整合。fetch しないのに既定パスを作らない)。
    let record_dir = cli
        .fetch_ratings
        .then(|| based.map(|(_, base, cfg)| resolve(base, cfg.record.dir.clone())))
        .flatten();
    let ratings_cache = cli
        .ratings_cache
        .clone()
        .or_else(|| record_dir.as_ref().map(|d| d.join("ratings_cache.tsv")));
    let ratings_history = cli
        .ratings_history
        .clone()
        .or_else(|| record_dir.as_ref().map(|d| d.join("ratings_history.tsv")));

    Ok(Effective {
        dir,
        me,
        ratings_cache,
        ratings_history,
    })
}

/// JSONL 1 行の必要フィールドだけを拾う(未使用 type 行は無視)。
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: String,
    ply: Option<u32>,
    engine: Option<String>,
    eval: Option<Eval>,
    winner: Option<String>,
    reason: Option<String>,
    plies: Option<u32>,
}

#[derive(Deserialize)]
struct Eval {
    nps: Option<f64>,
    time_ms: Option<u64>,
}

/// 1 局分の集計素材。
struct Game {
    /// ファイル名(datetime prefix を含む。表示・整列用)
    stem: String,
    sente: String,
    gote: String,
    /// 勝者名。引き分けは `None`。
    winner: Option<String>,
    reason: String,
    plies: u32,
    /// engine 名 -> その engine の nps サンプル(time_ms>=500 の本探索のみ)
    nps: BTreeMap<String, Vec<f64>>,
}

/// me から見た 1 局の結果。
#[derive(Clone, Copy, PartialEq)]
enum Res {
    Win,
    Loss,
    Draw,
}

fn parse_game(path: &std::path::Path) -> Result<Option<Game>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let mut sente_ply1: Option<String> = None;
    let mut engines: Vec<String> = Vec::new(); // 出現順の distinct engine
    let mut nps: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut winner: Option<String> = None;
    let mut reason = String::new();
    let mut plies = 0u32;
    let mut has_result = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(l) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        match l.kind.as_str() {
            "move" => {
                if let Some(eng) = l.engine {
                    // JSONL の move.engine / result.winner は raw の CSA 名、ファイル名と
                    // meta ラベルは sanitize 済みという混在なので、集計キーは入口で
                    // sanitize に統一する(`.` 等を含む名前でも突き合わせが揃う)。
                    let eng = sanitize_for_filename(&eng);
                    if l.ply == Some(1) {
                        sente_ply1 = Some(eng.clone());
                    }
                    if !engines.contains(&eng) {
                        engines.push(eng.clone());
                    }
                    if let Some(ev) = l.eval
                        && let Some(n) = ev.nps
                        && ev.time_ms.unwrap_or(0) >= 500
                    {
                        nps.entry(eng).or_default().push(n);
                    }
                }
            }
            "result" => {
                has_result = true;
                winner = l.winner.filter(|w| !w.is_empty()).map(|w| sanitize_for_filename(&w));
                reason = l.reason.unwrap_or_default();
                plies = l.plies.unwrap_or(0);
            }
            _ => {}
        }
    }

    if !has_result {
        return Ok(None); // 進行中 / 未完了の局はスキップ
    }
    let sente = match sente_ply1 {
        Some(s) => s,
        None => return Ok(None), // 手が 1 手も無い異常局
    };
    // 相手(後手)は move から判定するが、後手が 1 手も指さず負けた局(初手前に time_up 等)は
    // move に現れず取りこぼす。その場合はファイル名 `..._vs_{gote}` から補完する。
    let gote = match engines.iter().find(|e| **e != sente) {
        Some(g) => g.clone(),
        None => stem.rsplit_once("_vs_").map(|(_, g)| g.to_owned()).unwrap_or_default(),
    };

    Ok(Some(Game {
        stem,
        sente,
        gote,
        winner,
        reason,
        plies,
        nps,
    }))
}

/// me から見た結果。`None` は集計対象外(中断/検閲/error など未完了局)。
///
/// winner なしの局は reason で判別する: `sennichite` / `max_moves` / `jishogi` は正当な
/// 引き分け、それ以外(csa_client が中断・検閲を `outcome="draw"` + `reason="interrupted"`
/// で書く等)は勝率を歪めないよう除外する。
fn result_for(g: &Game, me: &str) -> Option<Res> {
    match &g.winner {
        Some(w) if w == me => Some(Res::Win),
        Some(_) => Some(Res::Loss),
        None => match g.reason.as_str() {
            "sennichite" | "max_moves" | "jishogi" => Some(Res::Draw),
            _ => None,
        },
    }
}

fn median(xs: &mut [f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

/// wdoor floodgate の現在レート表を取得し (ページ日付 YYYYMMDD, name -> rating) を作る。
/// 取得・解析は `tools::common::floodgate`(reqwest, in-repo)を再利用。キーは集計側と
/// 同じ sanitize 名に正規化する(raw 表示名が `.` 等を含んでも引ける。衝突は後勝ち)。
fn fetch_ratings_map() -> Result<(String, BTreeMap<String, f64>)> {
    let client = fg::http_client_builder().build().context("reqwest client 生成失敗")?;
    let (url, date, html) = fg::fetch_latest_rating_page(&client)?;
    eprintln!("レート取得: {url}");
    let map = fg::parse_rating_page(&html)
        .into_iter()
        .map(|(name, rate)| (sanitize_for_filename(&name), rate))
        .collect();
    Ok((date, map))
}

/// キャッシュの mtime が `max_age_sec` 以内か。0 は常に false(= 常に fetch)。
/// mtime が取得できない・未来時刻の場合は鮮度不明として false に倒す。
fn cache_is_fresh(path: &Path, max_age_sec: u64) -> bool {
    if max_age_sec == 0 {
        return false;
    }
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() <= max_age_sec)
}

/// 履歴 (`date<TAB>name<TAB>rate` 行) に (date, name) のエントリが既にあるか。
fn history_has_entry(text: &str, date: &str, name: &str) -> bool {
    let prefix = format!("{date}\t{name}\t");
    text.lines().any(|l| l.starts_with(&prefix))
}

/// 履歴が当日 (JST。レートページの日付は floodgate サーバ基準 = JST) の (date, me) を
/// 記録済みか。履歴未設定なら true(鮮度スキップを妨げない)。当日ページ未生成の
/// 早朝帯は fetch が前日ページに解決されて追記なしになるため、当日分が載るまで
/// 鮮度内でも毎回 fetch になる(1 GET/実行なので許容)。
fn history_recorded_today(history: Option<&Path>, me: &str) -> bool {
    let Some(path) = history else { return true };
    let jst = chrono::FixedOffset::east_opt(9 * 3600).expect("JST は有効なオフセット");
    let today = chrono::Utc::now().with_timezone(&jst).format("%Y%m%d").to_string();
    std::fs::read_to_string(path).is_ok_and(|text| history_has_entry(&text, &today, me))
}

/// 履歴ファイルに `date<TAB>name<TAB>rate` を 1 行追記する。同一 (date, name) の行が
/// 既にあれば何もしない(レートページは日次生成のため、同日の再実行で重複させない)。
/// 全体を tmp に書いて rename で置き換えるので、並行実行の追記競合や過去の尻切れ行に
/// 新行が連結される破損が起きない。追記したら `Ok(true)`。
fn append_ratings_history(path: &Path, date: &str, name: &str, rate: f64) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    if history_has_entry(&text, date, name) {
        return Ok(false);
    }
    let mut lines: Vec<&str> = text.lines().collect();
    let new_line = format!("{date}\t{name}\t{rate}");
    lines.push(&new_line);
    write_atomic(path, &(lines.join("\n") + "\n"))?;
    Ok(true)
}

/// レートマップを用意する。鮮度内キャッシュがあれば fetch せず直接使う(ただし履歴が
/// 当日分を未記録なら、履歴を欠かさないため鮮度内でも fetch する)。fetch 成功時は
/// キャッシュ書き出し + 履歴追記、失敗時はキャッシュ読み戻しでフォールバック。
/// opt-in の補助機能なので、失敗はすべて警告にとどめ空マップで続行する。
fn obtain_ratings(eff: &Effective, max_age_sec: u64, me: &str) -> BTreeMap<String, f64> {
    if let Some(cache) = &eff.ratings_cache
        && cache_is_fresh(cache, max_age_sec)
        && history_recorded_today(eff.ratings_history.as_deref(), me)
    {
        match read_ratings_cache(cache) {
            Ok(map) if !map.is_empty() => {
                eprintln!("レート: キャッシュ利用 ({} が {max_age_sec} 秒以内)", cache.display());
                return map;
            }
            Ok(_) => eprintln!("⚠ 鮮度内キャッシュが空。fetch にフォールバック"),
            Err(e) => eprintln!("⚠ 鮮度内キャッシュ読み込み失敗: {e:#}。fetch にフォールバック"),
        }
    }
    // 取得失敗・空取得はキャッシュへフォールバック。書き出しは「取得成功かつ非空」の
    // ときのみ(空取得で last-known-good を潰さない)。
    let (date, map) = match fetch_ratings_map() {
        Ok((date, map)) if !map.is_empty() => (date, map),
        res => {
            match res {
                Ok(_) => eprintln!("⚠ レート表が空(取得 0 件)。キャッシュにフォールバック"),
                Err(e) => eprintln!("⚠ レート取得失敗: {e:#}"),
            }
            let Some(cache) = &eff.ratings_cache else {
                return BTreeMap::new();
            };
            return read_ratings_cache(cache).unwrap_or_else(|e| {
                eprintln!("⚠ キャッシュ読み戻しも失敗(注釈なしで続行): {e:#}");
                BTreeMap::new()
            });
        }
    };
    if let Some(cache) = &eff.ratings_cache
        && let Err(e) = write_ratings_cache(cache, &map)
    {
        eprintln!("⚠ レートキャッシュ書き出し失敗: {e:#}");
    }
    if let Some(history) = &eff.ratings_history {
        match map.get(me) {
            Some(rate) => match append_ratings_history(history, &date, me, *rate) {
                Ok(true) => eprintln!("レート履歴追記: {date} {me} R{rate}"),
                Ok(false) => {}
                Err(e) => eprintln!("⚠ レート履歴追記失敗: {e:#}"),
            },
            None => eprintln!("⚠ {me} がレート表に無いため履歴追記をスキップ"),
        }
    }
    map
}

/// レートマップを `name<TAB>rate` でキャッシュへ書き出す(BTreeMap 順で決定的)。
fn write_ratings_cache(path: &Path, map: &BTreeMap<String, f64>) -> Result<()> {
    use std::fmt::Write;
    let mut content = String::new();
    for (name, rate) in map {
        writeln!(content, "{name}\t{rate}").expect("String への write は失敗しない");
    }
    write_atomic(path, &content)
}

/// キャッシュ(`name<TAB>rate`)を name -> rating に読み戻す。壊れた行・非有限値は捨てる。
/// キーは書き出し時と同じ sanitize 名に正規化する(raw 名で書かれた古いキャッシュも引ける)。
fn read_ratings_cache(path: &std::path::Path) -> Result<BTreeMap<String, f64>> {
    Ok(fg::read_ratings_tsv(path)?
        .into_iter()
        .map(|(n, r)| (sanitize_for_filename(&n), r))
        .collect())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // config は --config > 環境変数 CSA_CLIENT_CONFIG(空値は未設定扱い)。指定が
    // あるのに読めないのは設定ミスなので fail-fast(config 無しで集計したい場合は
    // 引数/env を外す)。
    let (config_path, config_source) = match cli.config.clone() {
        Some(p) => (Some(p), "--config"),
        None => (
            std::env::var_os("CSA_CLIENT_CONFIG")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            "env CSA_CLIENT_CONFIG",
        ),
    };
    let config = config_path
        .as_deref()
        .map(|path| {
            CsaClientConfig::from_file(path).with_context(|| {
                format!("config 読み込み失敗: {} ({config_source})", path.display())
            })
        })
        .transpose()?;
    let eff = resolve_effective(&cli, config_path.as_deref().zip(config.as_ref()))?;
    if let Some(path) = &config_path {
        // env 経由の暗黙適用でも「どの config から何が導出されたか」を可視化する。
        eprintln!(
            "config 適用: {} ({config_source}) → dir={}, me={}",
            path.display(),
            eff.dir.display(),
            eff.me.as_deref().unwrap_or("(自動判定)")
        );
    }

    let pattern = eff.dir.join("*.jsonl");
    let pattern = pattern.to_string_lossy();
    let mut games: Vec<Game> = Vec::new();
    for entry in glob::glob(&pattern).with_context(|| format!("glob {pattern}"))? {
        let path = entry?;
        if let Some(g) = parse_game(&path)? {
            games.push(g);
        }
    }
    if games.is_empty() {
        bail!("完了局の JSONL が {} に見つからない", eff.dir.display());
    }
    games.sort_by(|a, b| a.stem.cmp(&b.stem));

    // 対象エンジン: 明示 CLI / config の server.id、無ければ「最も多くの局に出現する
    // エンジン」を自動判定。
    let me = match eff.me.clone() {
        Some(m) => m,
        None => {
            let mut count: BTreeMap<&str, usize> = BTreeMap::new();
            for g in &games {
                *count.entry(g.sente.as_str()).or_default() += 1;
                if !g.gote.is_empty() {
                    *count.entry(g.gote.as_str()).or_default() += 1;
                }
            }
            count
                .into_iter()
                .max_by_key(|(_, c)| *c)
                .map(|(name, _)| name.to_string())
                .context("対象エンジンを自動判定できない")?
        }
    };

    // 注目相手パターンも sanitize 名の集計キーに部分一致させる(`.` 入りパターンでも当たる)。
    let watch: Vec<String> = cli.watch.iter().map(|w| sanitize_for_filename(w)).collect();

    // レートは現在値(対局時点ではない)。取得失敗・空取得はレート併記だけ諦め、集計は続行する
    // (opt-in の補助機能のために primary output を落とさない)。
    let ratings: BTreeMap<String, f64> = if cli.fetch_ratings {
        obtain_ratings(&eff, cli.ratings_max_age, &me)
    } else {
        BTreeMap::new()
    };
    let rlabel = |name: &str| -> String {
        ratings
            .get(name)
            .map(|r| format!(" (R{})", r.round() as i64))
            .unwrap_or_default()
    };

    // 集計
    let mut w = 0usize;
    let mut l = 0usize;
    let mut d = 0usize;
    // 先後別 [先, 後] それぞれ (W, L, D)
    let mut by_color = [[0usize; 3]; 2];
    // 相手別 (W, L, D)
    let mut by_opp: BTreeMap<String, [usize; 3]> = BTreeMap::new();
    let mut my_nps: Vec<f64> = Vec::new();
    let mut skipped_foreign = 0usize; // me が参加していない局
    let mut skipped_incomplete = 0usize; // 中断/検閲/error など未完了局

    for g in &games {
        if g.sente != me && g.gote != me {
            skipped_foreign += 1;
            continue;
        }
        let Some(res) = result_for(g, &me) else {
            skipped_incomplete += 1;
            continue;
        };
        let is_sente = g.sente == me;
        let opp = if is_sente { &g.gote } else { &g.sente };
        let idx = match res {
            Res::Win => 0,
            Res::Loss => 1,
            Res::Draw => 2,
        };
        match res {
            Res::Win => w += 1,
            Res::Loss => l += 1,
            Res::Draw => d += 1,
        }
        by_color[if is_sente { 0 } else { 1 }][idx] += 1;
        by_opp.entry(opp.clone()).or_default()[idx] += 1;
        if let Some(samples) = g.nps.get(&me) {
            my_nps.extend_from_slice(samples);
        }
    }

    let total = games.len();
    let n = w + l + d;
    let pct = |num: usize, den: usize| {
        if den == 0 {
            0.0
        } else {
            num as f64 / den as f64 * 100.0
        }
    };
    println!("集計対象: {me}{}   完了局 {total} 局 → 集計 {n} 局", rlabel(&me));
    if skipped_foreign > 0 || skipped_incomplete > 0 {
        println!(
            "  (除外: 対象不参加 {skipped_foreign} 局 / 中断・未完了 {skipped_incomplete} 局)"
        );
    }
    if n == 0 {
        println!("  ⚠ 集計対象が 0 局。--me の名前を確認(全局が対象不参加/未完了)。");
    }
    println!(
        "=== 通算: {w}勝 {l}敗 {d}分  ({n}局)  勝率 {:.0}% (引分除 {:.0}%) ===",
        pct(w, n),
        pct(w, w + l)
    );

    let names = ["先手番", "後手番"];
    for (i, name) in names.iter().enumerate() {
        let [cw, cl, cd] = by_color[i];
        let games_c = cw + cl + cd;
        if games_c == 0 {
            println!("  {name}: (0局)");
            continue;
        }
        // 全引分だと「引分除」の母数が 0 なので N/A(`-`)で 0% と区別する。
        let ex_draw = if cw + cl == 0 {
            "-".to_owned()
        } else {
            format!("{:.0}%", pct(cw, cw + cl))
        };
        println!(
            "  {name}: {cw}勝 {cl}敗 {cd}分 ({games_c}局)  勝率 {:.0}% (引分除 {ex_draw})",
            pct(cw, games_c)
        );
    }

    if !my_nps.is_empty() {
        let n_nps = my_nps.len();
        let med = median(&mut my_nps) / 1e6;
        println!("  実戦NPS(time_ms>=500 の median): {med:.2}M  (n={n_nps})");
    }

    println!("\n=== 相手別 ===");
    for (opp, [ow, ol, od]) in &by_opp {
        println!("  {ow}勝 {ol}敗 {od}分  vs {opp}{}", rlabel(opp));
    }

    // 後手勝ちは最上位帯では希少で価値が高いので、相手名付きで individually 列挙する。
    println!("\n=== 後手勝ち(価値大) ===");
    let mut any_gw = false;
    for g in &games {
        if g.gote != me {
            continue; // 後手が me の局のみ
        }
        if result_for(g, &me) != Some(Res::Win) {
            continue;
        }
        any_gw = true;
        let opp = &g.sente;
        let star = if watch.iter().any(|w| opp.contains(w.as_str())) {
            "  ★上位AI"
        } else {
            ""
        };
        println!("  {}  vs {opp}{}  ({}, {}手){star}", g.stem, rlabel(opp), g.reason, g.plies);
    }
    if !any_gw {
        println!("  (なし)");
    }

    for (title, target) in [("負け", Res::Loss), ("引分", Res::Draw)] {
        println!("\n=== {title} ===");
        let mut any = false;
        for g in &games {
            if g.sente != me && g.gote != me {
                continue;
            }
            if result_for(g, &me) != Some(target) {
                continue;
            }
            any = true;
            let is_sente = g.sente == me;
            let opp = if is_sente { &g.gote } else { &g.sente };
            let col = if is_sente { "先" } else { "後" };
            println!(
                "  {}  {col}手  vs {opp}{}  ({}, {}手)",
                g.stem,
                rlabel(opp),
                g.reason,
                g.plies
            );
        }
        if !any {
            println!("  (なし)");
        }
    }

    if !watch.is_empty() {
        println!("\n=== 注目相手との対戦 ===");
        for g in &games {
            if g.sente != me && g.gote != me {
                continue;
            }
            let is_sente = g.sente == me;
            let opp = if is_sente { &g.gote } else { &g.sente };
            if !watch.iter().any(|w| opp.contains(w.as_str())) {
                continue;
            }
            let Some(res) = result_for(g, &me) else {
                continue;
            };
            let col = if is_sente { "先" } else { "後" };
            let mark = match res {
                Res::Win => "○",
                Res::Loss => "●",
                Res::Draw => "△",
            };
            println!("  {}  {col}手  {mark}  vs {opp}{}  ({})", g.stem, rlabel(opp), g.reason);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(sente: &str, gote: &str, winner: Option<&str>, reason: &str) -> Game {
        Game {
            stem: "t".to_owned(),
            sente: sente.to_owned(),
            gote: gote.to_owned(),
            winner: winner.map(str::to_owned),
            reason: reason.to_owned(),
            plies: 100,
            nps: BTreeMap::new(),
        }
    }

    #[test]
    fn result_classification() {
        let me = "ME";
        assert!(matches!(result_for(&game("ME", "X", Some("ME"), "win"), me), Some(Res::Win)));
        assert!(matches!(result_for(&game("X", "ME", Some("X"), "resign"), me), Some(Res::Loss)));
        // winner なし + 正当な引き分け reason → Draw
        assert!(matches!(result_for(&game("X", "ME", None, "sennichite"), me), Some(Res::Draw)));
        assert!(matches!(result_for(&game("X", "ME", None, "max_moves"), me), Some(Res::Draw)));
        // winner なし + 中断系 reason → 集計対象外(None)
        assert!(result_for(&game("X", "ME", None, "interrupted"), me).is_none());
    }

    #[test]
    fn median_odd_even() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&mut []), 0.0);
    }

    #[test]
    fn parse_game_reads_players_and_result() {
        // 先手=OPP, 後手=ME, ME(白)勝ちの最小 JSONL を temp file に書いて解析。
        let dir = std::env::temp_dir();
        let path = dir.join("fg_record_test_g1.jsonl");
        let body = concat!(
            r#"{"type":"meta","timestamp":"t"}"#,
            "\n",
            r#"{"type":"move","game_id":1,"ply":1,"engine":"OPP","eval":{"nps":1000000,"time_ms":600}}"#,
            "\n",
            r#"{"type":"move","game_id":1,"ply":2,"engine":"ME","eval":{"nps":2000000,"time_ms":600}}"#,
            "\n",
            r#"{"type":"result","game_id":1,"outcome":"white_win","reason":"win","plies":2,"winner":"ME"}"#,
            "\n",
        );
        std::fs::write(&path, body).unwrap();
        let g = parse_game(&path).unwrap().expect("完了局");
        assert_eq!(g.sente, "OPP");
        assert_eq!(g.gote, "ME");
        assert_eq!(g.winner.as_deref(), Some("ME"));
        assert_eq!(g.plies, 2);
        // time_ms>=500 の nps が両者ぶん取れている
        assert_eq!(g.nps.get("ME").map(Vec::len), Some(1));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gote_recovered_from_filename_when_never_moved() {
        // 後手(ME)が 1 手も指さず負けた局: move は先手(OPP)のみ。gote はファイル名
        // ..._vs_{gote} から補完され、参加判定で取りこぼさない(#878 Codex 指摘)。
        let path = std::env::temp_dir().join("20260706_010003_OPP_vs_ME.jsonl");
        let body = concat!(
            r#"{"type":"move","ply":1,"engine":"OPP"}"#,
            "\n",
            r#"{"type":"result","outcome":"black_win","reason":"time_up","plies":1,"winner":"OPP"}"#,
            "\n",
        );
        std::fs::write(&path, body).unwrap();
        let g = parse_game(&path).unwrap().expect("完了局");
        assert_eq!(g.sente, "OPP");
        assert_eq!(g.gote, "ME");
        assert!(matches!(result_for(&g, "ME"), Some(Res::Loss)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ratings_cache_round_trip() {
        let path = std::env::temp_dir().join("fg_record_ratings_cache_test.tsv");
        let mut m = BTreeMap::new();
        m.insert("RAMU_TF".to_owned(), 3427.0);
        m.insert("Foo-Bar_1".to_owned(), -12.0); // 負レートも保持
        write_ratings_cache(&path, &m).unwrap();
        let back = read_ratings_cache(&path).unwrap();
        assert_eq!(back.get("RAMU_TF"), Some(&3427.0));
        assert_eq!(back.get("Foo-Bar_1"), Some(&-12.0));
        assert_eq!(back.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    /// clap 定義そのものから既定値を得る(struct literal の複製だと default_value_t と
    /// 乖離しうる)。
    fn cli_default() -> Cli {
        Cli::try_parse_from(["floodgate_record"]).unwrap()
    }

    fn config_from(toml_text: &str) -> CsaClientConfig {
        toml::from_str(toml_text).unwrap()
    }

    #[test]
    fn resolve_effective_no_config_defaults() {
        let eff = resolve_effective(&cli_default(), None).unwrap();
        assert_eq!(eff.dir, PathBuf::from("."));
        assert_eq!(eff.me, None);
        assert_eq!(eff.ratings_cache, None);
        assert_eq!(eff.ratings_history, None);
    }

    #[test]
    fn resolve_effective_derives_from_config() {
        let cfg = config_from("[server]\nid = \"RAMU_TF\"\n[record]\ndir = \"/data/records\"\n");
        let cli = Cli {
            fetch_ratings: true,
            ..cli_default()
        };
        let eff = resolve_effective(&cli, Some((Path::new("/etc/fg/active.toml"), &cfg))).unwrap();
        assert_eq!(eff.dir, PathBuf::from("/data/records/jsonl"));
        assert_eq!(eff.me.as_deref(), Some("RAMU_TF"));
        assert_eq!(eff.ratings_cache, Some(PathBuf::from("/data/records/ratings_cache.tsv")));
        assert_eq!(eff.ratings_history, Some(PathBuf::from("/data/records/ratings_history.tsv")));
    }

    #[test]
    fn resolve_effective_cli_overrides_config() {
        let cfg = config_from("[server]\nid = \"RAMU_TF\"\n[record]\ndir = \"/data/records\"\n");
        let cli = Cli {
            dir: Some(PathBuf::from("/explicit/jsonl")),
            me: Some("OTHER".to_owned()),
            fetch_ratings: true,
            ratings_cache: Some(PathBuf::from("/explicit/cache.tsv")),
            ratings_history: Some(PathBuf::from("/explicit/hist.tsv")),
            ..cli_default()
        };
        let eff = resolve_effective(&cli, Some((Path::new("/etc/fg/active.toml"), &cfg))).unwrap();
        assert_eq!(eff.dir, PathBuf::from("/explicit/jsonl"));
        assert_eq!(eff.me.as_deref(), Some("OTHER"));
        assert_eq!(eff.ratings_cache, Some(PathBuf::from("/explicit/cache.tsv")));
        assert_eq!(eff.ratings_history, Some(PathBuf::from("/explicit/hist.tsv")));
    }

    #[test]
    fn resolve_effective_relative_paths_use_config_dir() {
        // TOML 内相対パスは config ファイルのディレクトリ基準。jsonl_out 上書きも尊重。
        let cfg = config_from("[record]\ndir = \"records\"\n");
        let eff = resolve_effective(
            &cli_default(),
            Some((Path::new("/home/u/floodgate/active.toml"), &cfg)),
        )
        .unwrap();
        assert_eq!(eff.dir, PathBuf::from("/home/u/floodgate/records/jsonl"));
        // --fetch-ratings なしでは config 由来のキャッシュ/履歴既定も作らない
        assert_eq!(eff.ratings_cache, None);
        assert_eq!(eff.ratings_history, None);

        let cfg = config_from("[record]\ndir = \"records\"\njsonl_out = \"jl\"\n");
        let eff = resolve_effective(
            &cli_default(),
            Some((Path::new("/home/u/floodgate/active.toml"), &cfg)),
        )
        .unwrap();
        assert_eq!(eff.dir, PathBuf::from("/home/u/floodgate/jl"));
    }

    #[test]
    fn resolve_effective_rejects_config_without_jsonl() {
        let cfg = config_from("[record]\nenabled = false\n");
        let err = resolve_effective(&cli_default(), Some((Path::new("/x/a.toml"), &cfg)));
        assert!(err.is_err());
        // --dir 明示があれば config の JSONL 無効は問題にならない
        let cli = Cli {
            dir: Some(PathBuf::from("/y")),
            ..cli_default()
        };
        let eff = resolve_effective(&cli, Some((Path::new("/x/a.toml"), &cfg))).unwrap();
        assert_eq!(eff.dir, PathBuf::from("/y"));
    }

    #[test]
    fn resolve_effective_empty_server_id_falls_back_to_autodetect() {
        let cfg = config_from("[record]\ndir = \"/d\"\n");
        let eff = resolve_effective(&cli_default(), Some((Path::new("/x/a.toml"), &cfg))).unwrap();
        assert_eq!(eff.me, None);
    }

    #[test]
    fn resolve_effective_sanitizes_config_me_like_jsonl_labels() {
        // 集計キーは sanitize 名に統一しているので `.` を含む server.id は `_` で比較。
        let cfg = config_from("[server]\nid = \"ramu.v2\"\n[record]\ndir = \"/d\"\n");
        let eff = resolve_effective(&cli_default(), Some((Path::new("/x/a.toml"), &cfg))).unwrap();
        assert_eq!(eff.me.as_deref(), Some("ramu_v2"));
    }

    #[test]
    fn parse_game_sanitizes_raw_names() {
        // move.engine / result.winner は raw の CSA 名で書かれるため、集計キーは
        // sanitize 名に揃えて読み込む。
        let path = std::env::temp_dir().join("fg_record_test_sanitize.jsonl");
        let body = concat!(
            r#"{"type":"move","ply":1,"engine":"ramu.v2"}"#,
            "\n",
            r#"{"type":"move","ply":2,"engine":"opp@x"}"#,
            "\n",
            r#"{"type":"result","outcome":"black_win","reason":"resign","plies":2,"winner":"ramu.v2"}"#,
            "\n",
        );
        std::fs::write(&path, body).unwrap();
        let g = parse_game(&path).unwrap().expect("完了局");
        assert_eq!(g.sente, "ramu_v2");
        assert_eq!(g.gote, "opp_x");
        assert!(matches!(result_for(&g, "ramu_v2"), Some(Res::Win)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ratings_cache_normalizes_names() {
        // raw 名で書かれた古いキャッシュも sanitize キーで引ける。
        let path = std::env::temp_dir().join("fg_record_cache_sanitize.tsv");
        std::fs::write(&path, "ramu.v2\t3400\n").unwrap();
        let m = read_ratings_cache(&path).unwrap();
        assert_eq!(m.get("ramu_v2"), Some(&3400.0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ratings_history_creates_parent_dir_and_heals_unterminated_line() {
        let dir = std::env::temp_dir().join("fg_record_history_nested_test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sub").join("hist.tsv");
        // 親 dir が無くても追記できる(config 由来の record.dir が未作成のケース)
        assert!(append_ratings_history(&path, "20260707", "ME", 3400.0).unwrap());
        // 尻切れ行(改行なし)が残っていても新行が連結されない
        std::fs::write(&path, "20260706\tME\t33").unwrap();
        assert!(append_ratings_history(&path, "20260707", "ME", 3400.0).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "20260706\tME\t33\n20260707\tME\t3400\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ratings_history_append_is_idempotent_per_date_and_name() {
        let path = std::env::temp_dir().join("fg_record_ratings_history_test.tsv");
        let _ = std::fs::remove_file(&path);
        assert!(append_ratings_history(&path, "20260707", "RAMU_TF", 3427.0).unwrap());
        // 同一 (日付, 名前) は再追記しない(値が違っても最初の観測値を保持)
        assert!(!append_ratings_history(&path, "20260707", "RAMU_TF", 3500.0).unwrap());
        // 別日付・別名は追記。名前が prefix 一致するだけの別エンジンも混同しない
        assert!(append_ratings_history(&path, "20260708", "RAMU_TF", 3440.0).unwrap());
        assert!(append_ratings_history(&path, "20260707", "RAMU", 100.0).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            vec![
                "20260707\tRAMU_TF\t3427",
                "20260708\tRAMU_TF\t3440",
                "20260707\tRAMU\t100"
            ]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cache_freshness() {
        let path = std::env::temp_dir().join("fg_record_cache_fresh_test.tsv");
        std::fs::write(&path, "A\t1\n").unwrap();
        assert!(cache_is_fresh(&path, 3600)); // 直前に書いたので鮮度内
        assert!(!cache_is_fresh(&path, 0)); // 0 = 常に fetch
        let _ = std::fs::remove_file(&path);
        assert!(!cache_is_fresh(&path, 3600)); // 存在しないファイルは stale 扱い
    }

    #[test]
    fn read_ratings_cache_drops_non_finite_and_corrupt() {
        let path = std::env::temp_dir().join("fg_record_ratings_cache_corrupt.tsv");
        std::fs::write(&path, "Good\t3400\nBadNaN\tNaN\nBadInf\tinf\nNoTab 1\nEmpty\t\n").unwrap();
        let m = read_ratings_cache(&path).unwrap();
        assert_eq!(m.get("Good"), Some(&3400.0));
        assert!(!m.contains_key("BadNaN")); // NaN は is_finite で除外
        assert!(!m.contains_key("BadInf")); // inf も除外
        assert_eq!(m.len(), 1); // tab 無し・空 rate も落ちる
        let _ = std::fs::remove_file(&path);
    }
}
