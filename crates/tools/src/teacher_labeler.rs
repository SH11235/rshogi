//! 教師ラベリングの共有コア。
//!
//! held-out / 教師プールの各局面を「固定 depth の NNUE 探索」で決定的にラベル付けする中核を
//! 集約する。`yardstick_label`（ラベル品質の物差し・JSONL 出力）と `rescore_hcpe`（教師生成・
//! hcpe 出力）が**同一の評価器構成・同一の fresh-per-position 探索**をこのモジュール経由で使う
//! ことで、両者のラベルが bit 一致することを構造的に保証する（「測った config = 回す config」）。
//!
//! 設計上の不変条件:
//! - 局面ごとに `Search` を作り直し 1 スレッド固定で探索する。これにより 1 局面の評価は
//!   他局面・処理順・スレッド数・シャード分割から独立し、同一入力なら出力は bit 一致する。
//!   （`Search::go` は TT 世代を進めるだけで TT/history をクリアしないため、Search を局面間で
//!   使い回すとラベルが処理順に依存してしまう。それを避けるための fresh-per-position。）
//! - 符号規約は手番側視点（side-to-move view）cp で統一（hcpe 保存 eval・dlshogi value 目標と同じ）。

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use rshogi_core::nnue::{
    LayerStackBucketMode, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, get_layer_stack_bucket_mode,
    init_nnue, is_layer_stacks_loaded, parse_layer_stack_bucket_mode, set_fv_scale_override,
    set_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search, SearchInfo};

/// hcpe（cshogi HuffmanCodedPosAndEval）1 レコードのバイト長。
pub const HCPE_RECORD_SIZE: usize = 38;

/// 探索用スタックサイズ（64MB）。深い探索で再帰スタックを使うため main 同等を確保する。
/// worker スレッドの `stack_size` に使う。
pub const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

/// 評価器（NNUE + LayerStacks bucket 設定）の構成パラメータ。
pub struct LabelerEvalConfig<'a> {
    /// labeler の NNUE モデルファイル。
    pub nnue: &'a Path,
    /// FV_SCALE オーバーライド（0=ヘッダ自動判定、1 以上=指定値。threat/none LayerStacks 系は 28）。
    pub fv_scale: i32,
    /// LayerStacks の bucket mode（例 `progress8kpabs`）。LS ビルドでは既定なので通常 None。
    pub ls_bucket_mode: Option<&'a str>,
    /// progress8kpabs 用の進行度係数ファイル（USI `LS_PROGRESS_COEFF` と同じ）。
    pub ls_progress_coeff: Option<&'a Path>,
}

/// 評価器（NNUE + LayerStacks bucket 設定）を USI エンジンと同じ手順で構成する。
/// `label_bench_positions::configure_eval` と同じく progress8kpabs で係数未指定なら弾く。
///
/// # 注意（グローバル状態）
/// `set_fv_scale_override` / `set_layer_stack_bucket_mode` /
/// `set_layer_stack_progress_kpabs_weights` / `init_nnue` はいずれもプロセスグローバルな状態を
/// 書き換える。**1 プロセスにつき起動時に 1 回だけ呼ぶこと。** 同一プロセス内で複数の設定を
/// 切り替えるとグローバル状態が競合するため、その用途では別途排他制御が必要になる。
pub fn configure_eval(cfg: &LabelerEvalConfig) -> Result<()> {
    if !cfg.nnue.exists() {
        bail!("NNUE model file not found: {}", cfg.nnue.display());
    }
    if cfg.fv_scale != 0 {
        set_fv_scale_override(cfg.fv_scale);
        eprintln!("FV_SCALE: {}", cfg.fv_scale);
    } else {
        eprintln!("FV_SCALE: auto-detect (header)");
    }
    if let Some(mode_str) = cfg.ls_bucket_mode {
        let mode = parse_layer_stack_bucket_mode(mode_str)
            .with_context(|| format!("invalid --ls-bucket-mode '{mode_str}'"))?;
        set_layer_stack_bucket_mode(mode);
        eprintln!("LS_BUCKET_MODE: {}", mode.as_str());
    }
    let mut coeff_loaded = false;
    if let Some(path) = cfg.ls_progress_coeff {
        let weights = load_progress_coeff_kpabs(path)?;
        set_layer_stack_progress_kpabs_weights(weights)
            .map_err(|e| anyhow::anyhow!("failed to set progress coeff weights: {e}"))?;
        coeff_loaded = true;
        eprintln!("LS_PROGRESS_COEFF: {}", path.display());
    }
    init_nnue(cfg.nnue).context("Failed to load NNUE model")?;
    eprintln!("NNUE model loaded: {}", cfg.nnue.display());
    if is_layer_stacks_loaded()
        && get_layer_stack_bucket_mode() == LayerStackBucketMode::Progress8KPAbs
        && !coeff_loaded
    {
        bail!(
            "LS_BUCKET_MODE=progress8kpabs requires --ls-progress-coeff. \
             Without it the progress bucket selection diverges from training and labels are wrong."
        );
    }
    Ok(())
}

/// progress8kpabs 用の進行度係数ファイル（f64 配列）を読み f32 重みへ変換する。
fn load_progress_coeff_kpabs(path: &Path) -> Result<Box<[f32]>> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read --ls-progress-coeff {}", path.display()))?;
    let expected = SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * std::mem::size_of::<f64>();
    if bytes.len() != expected {
        bail!("progress coeff size mismatch: got {} bytes, expected {}", bytes.len(), expected);
    }
    let weights: Vec<f32> = bytes
        .chunks_exact(std::mem::size_of::<f64>())
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("chunk size is checked")) as f32)
        .collect();
    Ok(weights.into_boxed_slice())
}

/// `--capture-depths` の "9,12,15" を昇順・重複排除した正の depth 列に。
pub fn parse_capture_depths(s: &str) -> Result<Vec<i32>> {
    let mut depths: Vec<i32> = Vec::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let d: i32 =
            tok.parse().with_context(|| format!("invalid --capture-depths entry '{tok}'"))?;
        if d <= 0 {
            bail!("--capture-depths entries must be > 0 (got {d})");
        }
        depths.push(d);
    }
    if depths.is_empty() {
        bail!("--capture-depths is empty");
    }
    depths.sort_unstable();
    depths.dedup();
    Ok(depths)
}

/// SPSA `.params`（USI `SPSAParamsFile` と同形式）を `(USI 名, 値)` の列に読み込む。
/// 行形式: `name,type,value[,min,max,c_end,r_end] [// comment] [[[NOT USED]]]`。空行・`#`
/// コメント・列不足・値パース不能の行は読み飛ばす（USI 側ローダと同方針）。適用は探索ごとに
/// `set_search_tune_option` で行うため、未知名は適用時に無視され、範囲外は clamp される。
pub fn parse_spsa_params(path: &Path) -> Result<Vec<(String, i32)>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read --spsa-params {}", path.display()))?;
    let params = parse_spsa_params_content(&content);
    if params.is_empty() {
        bail!("--spsa-params {} contained no applicable rows", path.display());
    }
    eprintln!("Loaded {} SPSA param(s) from {}", params.len(), path.display());
    Ok(params)
}

/// `.params` の本文を `(USI 名, 値)` の列にパースする（IO なし。`parse_spsa_params` の本体）。
/// クレート内専用（`parse_spsa_params` からのみ呼ぶ。テストは `super::*` で参照する）。
fn parse_spsa_params_content(content: &str) -> Vec<(String, i32)> {
    let mut params = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let val_part = trimmed
            .split_once("//")
            .map_or(trimmed, |(before, _)| before)
            .replace("[[NOT USED]]", "");
        let cols: Vec<&str> = val_part.split(',').map(str::trim).collect();
        if cols.len() < 3 {
            continue;
        }
        let (name, type_name, value_str) = (cols[0], cols[1], cols[2]);
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
        params.push((name.to_string(), parsed));
    }
    params
}

/// ロード時に 1 度だけ使い捨ての `Search` へ全 params を適用し、実際に適用される件数・clamp 件数・
/// 未知名を warn する。`.params` の名前 typo（例: net-mismatch なファイルの取り違え）を黙殺せず早期に
/// 気付けるようにするためで、USI ローダ（`maybe_load_spsa_params` の applied/clamped ログ）と挙動を揃える。
/// 実探索は局面ごとに同じ (name,value) を決定的に適用するので、この検証は決定性に影響しない。
pub fn warn_unapplied_tune_params(params: &[(String, i32)]) {
    let mut probe = Search::new(1);
    let mut applied = 0usize;
    let mut clamped = 0usize;
    let mut unknown: Vec<&str> = Vec::new();
    for (name, value) in params {
        match probe.set_search_tune_option(name, *value) {
            Some(result) => {
                applied += 1;
                if result.clamped {
                    clamped += 1;
                }
            }
            None => unknown.push(name),
        }
    }
    eprintln!("SPSA params applied: {applied} (clamped {clamped}, unknown {})", unknown.len());
    if !unknown.is_empty() {
        eprintln!("  warning: unknown SPSA param name(s) ignored: {}", unknown.join(", "));
    }
}

/// fresh-per-position の固定 depth NNUE 探索で 1 局面をラベル付けし、手番側視点 cp の
/// `(eval, is_mate)` を返す。
///
/// - `targets` 指定時は反復深化の中間 depth を 1 回の探索で捕捉し、各 target depth の
///   `(eval, is_mate)` を `targets` と同順で返す（target に到達すれば exact、早期終了（詰み等）
///   なら最深 depth の値で埋める）。未指定時は `depth` 単発で 1 要素のみ返す。
/// - `tune_params` は SPSA 等の探索パラメータ（USI 名, 値）。空なら engine 既定値。
/// - 局面ごとに空の `Search` を作るため TT/history は持ち越さず、処理順・スレッド数・シャード
///   分割に依存しない決定的ラベルになる。
pub fn label_position(
    pos: &mut Position,
    depth: i32,
    nodes: u64,
    hash_mb: usize,
    tune_params: &[(String, i32)],
    targets: Option<&[i32]>,
) -> Vec<(i32, bool)> {
    let mut search = Search::new(hash_mb);
    search.set_num_threads(1);
    // SPSA 探索パラメータを setoption 相当で適用（未指定なら空 = engine 既定値）。
    for (name, value) in tune_params {
        search.set_search_tune_option(name, *value);
    }
    let mut limits = LimitsType::default();
    limits.depth = depth;
    if nodes > 0 {
        limits.nodes = nodes;
    }
    limits.set_start_time();

    match targets {
        // capture mode: 1 回の探索で各 target depth の反復深化中間スコアを捕捉する。
        Some(targets) => {
            let mut captured: Vec<Option<(i32, bool)>> = vec![None; targets.len()];
            let result = {
                let cap = &mut captured;
                let on_info = |info: &SearchInfo| {
                    if info.multi_pv != 1 {
                        return;
                    }
                    for (slot, &td) in cap.iter_mut().zip(targets) {
                        if info.depth <= td {
                            *slot = Some((info.score.to_cp(), info.score.is_mate_score()));
                        }
                    }
                };
                search.go(pos, limits, Some(on_info))
            };
            let fallback = (result.score.to_cp(), result.score.is_mate_score());
            captured.into_iter().map(|c| c.unwrap_or(fallback)).collect()
        }
        None => {
            let result = search.go(pos, limits, None::<fn(&SearchInfo)>);
            vec![(result.score.to_cp(), result.score.is_mate_score())]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spsa_params_content_parses_rounds_and_skips() {
        // int は f64 として読んで round、空行/コメント/列不足/値不正は読み飛ばす。
        let content = "\
# comment\n\
\n\
SPSA_IIR_SHALLOW,int,1,0,8,1,0.4\n\
SPSA_LMR_DELTA_SCALE,int,933.7,0,4096,20,204.8 // trailing comment\n\
SPSA_DRAW_JITTER_OFFSET,int,-1,-16,16 [[NOT USED]]\n\
NON_INT_SPIN,spin,7,0,100\n\
NON_INT_FRACTION,spin,3.5,0,100\n\
TOO_FEW,int\n\
BAD_VALUE,int,xyz,0,8\n";
        let params = parse_spsa_params_content(content);
        assert_eq!(
            params,
            vec![
                ("SPSA_IIR_SHALLOW".to_string(), 1),
                ("SPSA_LMR_DELTA_SCALE".to_string(), 934),
                ("SPSA_DRAW_JITTER_OFFSET".to_string(), -1),
                // 非 int 型は i32 直読み（round しない）。小数は parse 失敗で読み飛ばす。
                ("NON_INT_SPIN".to_string(), 7),
            ]
        );
        assert!(parse_spsa_params_content("# only a comment\n").is_empty());
    }

    #[test]
    fn parse_capture_depths_sorts_dedups_validates() {
        assert_eq!(parse_capture_depths("15,9,12,9").unwrap(), vec![9, 12, 15]);
        assert_eq!(parse_capture_depths(" 9 , 12 ").unwrap(), vec![9, 12]);
        assert!(parse_capture_depths("").is_err());
        assert!(parse_capture_depths("9,0").is_err());
        assert!(parse_capture_depths("9,x").is_err());
    }
}
