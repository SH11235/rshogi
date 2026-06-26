//! ラベル品質「物差し」ステージ 2: labeler のラベルを WDL ground truth で採点する。
//!
//! `yardstick_label` が出した採点用 jsonl（手番側視点で `wdl`=実対局結果・`eval_ref`=保存
//! 教師 eval・`eval_label`=labeler の探索値を持つ）を読み、engine ごとに勝率スケールを較正
//! してから class 別に以下を出す:
//!
//! - **主指標: WDL logloss** = mean[ CE(sigmoid(eval/a), wdl) ]。a は labeler ごとに logloss
//!   最小化で較正（NNUE の FV_SCALE と DL の winrate を混ぜても scale 差を精度と誤認しない）。
//! - **参照天井**: 保存 eval（教師）の符号一致率。labeler が超えるべき model 非依存の上限
//!   （datagap の `diag_strict.py` の `evalvs_result` と同義）。
//! - **副指標: リファレンス一致**: 較正後 win-prob 空間で labeler と保存 eval の MAE、および
//!   eval の Spearman 順位相関。
//!
//! 符号規約はすべて手番側視点（`yardstick_label` と同じ）。詰み（|eval| >= 30000）は較正・
//! logloss・一致から除外する（飽和域は勝率較正を歪めるため）。
//!
//! held-out は設計上 5〜20 万局面で bounded なので全件を読み込んでから採点する
//! （億規模の教師プールを load-all する系ツールとは別）。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};

/// 詰みとみなす絶対 cp 閾値（`yardstick_label` と一致させる）。
const MATE_ABS: i32 = 30000;

/// logloss の確率クランプ（log(0) 回避）。
const PROB_EPS: f64 = 1e-7;

/// eval_band を |cp| 昇順で表示するための固定順（mate は採点対象外なので含めない）。
const EVAL_BAND_ORDER: [&str; 4] = ["0-150", "151-600", "601-1500", "1501+"];

#[derive(Parser, Debug)]
#[command(
    name = "yardstick_score",
    version,
    about = "labeler の jsonl を WDL で採点し per-class の logloss/参照天井/一致を出す"
)]
struct Cli {
    /// 採点する jsonl（`yardstick_label` 出力）。複数指定で labeler/depth を並べて比較。
    #[arg(required = true)]
    labeled: Vec<PathBuf>,

    /// 結果 JSON の出力先（任意）。指定すると per-file/per-group の数値を機械可読で残す。
    #[arg(long)]
    out: Option<PathBuf>,
}

/// `yardstick_label` の 1 行レコード。
#[derive(Deserialize)]
struct ScoreRecord {
    wdl: f64,
    eval_ref: i32,
    eval_label: i32,
    eval_band: String,
    nyugyoku: String,
    in_check: bool,
    mate_ref: bool,
    mate_label: bool,
    #[serde(default)]
    source: Option<String>,
}

/// 1 グループの集計結果。
#[derive(Serialize, Clone)]
struct GroupMetrics {
    group: String,
    /// 採点対象（詰み除外）の局面数。
    n: usize,
    /// labeler の WDL logloss（較正後）。
    label_logloss: f64,
    /// 保存 eval（教師）の WDL logloss（較正後）。
    ref_logloss: f64,
    /// labeler の符号一致率。
    label_sign_acc: f64,
    /// 保存 eval の符号一致率（= 参照天井）。
    ref_sign_acc: f64,
    /// 較正後 win-prob 空間での labeler vs 保存 eval の MAE。
    winprob_mae: f64,
    /// eval_label vs eval_ref の Spearman 順位相関。
    spearman: f64,
}

/// 1 ファイル（= 1 labeler config）の採点結果。
#[derive(Serialize)]
struct FileReport {
    file: String,
    n_total: usize,
    n_scored: usize,
    /// labeler eval の較正スケール（cp、win-prob = sigmoid(eval/a_label)）。
    a_label: f64,
    /// 保存 eval の較正スケール。
    a_ref: f64,
    /// 全レコード（詰み含む）での参照天井符号一致率（datagap の diag_strict と比較可能）。
    ref_ceiling_all: f64,
    groups: Vec<GroupMetrics>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut reports = Vec::new();
    for path in &cli.labeled {
        let report = score_file(path)?;
        print_report(&report);
        reports.push(report);
    }
    if let Some(out) = &cli.out {
        let f = File::create(out).with_context(|| format!("Failed to create {}", out.display()))?;
        let mut w = BufWriter::new(f);
        serde_json::to_writer_pretty(&mut w, &reports)?;
        w.write_all(b"\n")?;
        w.flush()?;
        eprintln!("wrote results json: {}", out.display());
    }
    Ok(())
}

fn score_file(path: &Path) -> Result<FileReport> {
    let records = load_records(path)?;
    if records.is_empty() {
        bail!("no records in {}", path.display());
    }
    let n_total = records.len();

    // 参照天井（全レコード、詰み含む）= 保存 eval 符号 vs 実結果。draw は wdl>=0.5 を
    // 「手番側勝ち」として扱う（diag_strict の value>=0.5 規約に合わせる）。
    let ref_ceiling_all = sign_acc(records.iter().map(|r| (r.eval_ref, r.wdl)));

    // 採点対象 = labeler・保存 eval どちらも非詰みの共通集合（飽和域は較正を歪めるため除外）。
    // `to_cp()` が詰みフラグ無しで飽和 cp を返す経路もあるので、フラグと |cp| 閾値の両方で弾く。
    let scored: Vec<&ScoreRecord> = records
        .iter()
        .filter(|r| {
            !r.mate_ref
                && !r.mate_label
                && r.eval_ref.abs() < MATE_ABS
                && r.eval_label.abs() < MATE_ABS
        })
        .collect();
    let n_scored = scored.len();
    if n_scored == 0 {
        bail!("no non-mate records to score in {}", path.display());
    }

    // engine ごとに 1 つの global scale を較正（class ごとには較正しない＝scale を class に
    // 過適合させない）。較正後の固定 scale で per-class の logloss を出す。
    let a_label = calibrate(scored.iter().map(|r| (r.eval_label as f64, r.wdl)));
    let a_ref = calibrate(scored.iter().map(|r| (r.eval_ref as f64, r.wdl)));

    // class は各次元の周辺スライス（marginal）で出す。eval_band×nyugyoku×in_check×source の
    // 直積セルは出さない。理由: bias の所在は周辺スライスで特定でき（「互角帯に集中」「入玉
    // class に集中」）、直積は Floodgate のように入玉局面がほぼ無い held-out で空セルが多発し
    // 可読性・統計的安定性を損なうため。直積が要るようになってから追加する（現状は不要）。
    let mut groups = Vec::new();
    groups.push(group_metrics("overall", &scored, a_label, a_ref));

    // eval_band 別（保存 eval 由来なので labeler 非依存に固定）。表示は |cp| 昇順に並べる
    // （lexicographic だと `1501+` が `151-600` より前に来て直感に反するため）。
    for band in EVAL_BAND_ORDER {
        let g: Vec<&ScoreRecord> = scored.iter().filter(|r| r.eval_band == band).copied().collect();
        if !g.is_empty() {
            groups.push(group_metrics(&format!("eval_band={band}"), &g, a_label, a_ref));
        }
    }
    // 入玉別。
    for ny in distinct(scored.iter().map(|r| r.nyugyoku.as_str())) {
        let g: Vec<&ScoreRecord> = scored.iter().filter(|r| r.nyugyoku == ny).copied().collect();
        groups.push(group_metrics(&format!("nyugyoku={ny}"), &g, a_label, a_ref));
    }
    // 王手別。
    for chk in [false, true] {
        let g: Vec<&ScoreRecord> = scored.iter().filter(|r| r.in_check == chk).copied().collect();
        if !g.is_empty() {
            groups.push(group_metrics(&format!("in_check={chk}"), &g, a_label, a_ref));
        }
    }
    // source 別（指定があれば）。
    let sources = distinct(scored.iter().filter_map(|r| r.source.as_deref()));
    if sources.len() > 1 {
        for src in sources {
            let g: Vec<&ScoreRecord> =
                scored.iter().filter(|r| r.source.as_deref() == Some(src)).copied().collect();
            groups.push(group_metrics(&format!("source={src}"), &g, a_label, a_ref));
        }
    }

    Ok(FileReport {
        file: path.display().to_string(),
        n_total,
        n_scored,
        a_label,
        a_ref,
        ref_ceiling_all,
        groups,
    })
}

fn load_records(path: &Path) -> Result<Vec<ScoreRecord>> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: ScoreRecord = serde_json::from_str(&line)
            .with_context(|| format!("{}:{}: json parse error", path.display(), i + 1))?;
        out.push(rec);
    }
    Ok(out)
}

/// 較正済みスケール `a` で 1 グループの指標を計算する。
fn group_metrics(name: &str, group: &[&ScoreRecord], a_label: f64, a_ref: f64) -> GroupMetrics {
    let n = group.len();
    let label_logloss = mean_logloss(group.iter().map(|r| (r.eval_label as f64, r.wdl)), a_label);
    let ref_logloss = mean_logloss(group.iter().map(|r| (r.eval_ref as f64, r.wdl)), a_ref);
    let label_sign_acc = sign_acc(group.iter().map(|r| (r.eval_label, r.wdl)));
    let ref_sign_acc = sign_acc(group.iter().map(|r| (r.eval_ref, r.wdl)));
    let winprob_mae = if n == 0 {
        0.0
    } else {
        group
            .iter()
            .map(|r| {
                (sigmoid(r.eval_label as f64 / a_label) - sigmoid(r.eval_ref as f64 / a_ref)).abs()
            })
            .sum::<f64>()
            / n as f64
    };
    let spearman = spearman_corr(
        &group.iter().map(|r| r.eval_label as f64).collect::<Vec<_>>(),
        &group.iter().map(|r| r.eval_ref as f64).collect::<Vec<_>>(),
    );
    GroupMetrics {
        group: name.to_string(),
        n,
        label_logloss,
        ref_logloss,
        label_sign_acc,
        ref_sign_acc,
        winprob_mae,
        spearman,
    }
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// 数値安定な softplus = ln(1 + e^x)。
fn softplus(x: f64) -> f64 {
    x.max(0.0) + (1.0 + (-x.abs()).exp()).ln()
}

/// 平均 logloss（cross-entropy）。target は wdl∈[0,1]（draw=0.5）、予測は sigmoid(eval/a)。
fn mean_logloss(samples: impl Iterator<Item = (f64, f64)>, a: f64) -> f64 {
    let mut sum = 0.0;
    let mut n = 0usize;
    for (eval, wdl) in samples {
        let p = sigmoid(eval / a).clamp(PROB_EPS, 1.0 - PROB_EPS);
        sum += -(wdl * p.ln() + (1.0 - wdl) * (1.0 - p).ln());
        n += 1;
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// 符号一致率。pred = (eval >= 0)、target = (wdl >= 0.5)（draw は手番側勝ち扱い）。
fn sign_acc(samples: impl Iterator<Item = (i32, f64)>) -> f64 {
    let mut agree = 0usize;
    let mut n = 0usize;
    for (eval, wdl) in samples {
        if (eval >= 0) == (wdl >= 0.5) {
            agree += 1;
        }
        n += 1;
    }
    if n == 0 { 0.0 } else { agree as f64 / n as f64 }
}

/// labeler ごとに 1 つの勝率スケール `a` を WDL logloss 最小化で較正する。
///
/// `win_prob = sigmoid(eval / a)`。NLL は k=1/a について凸なので、k を黄金分割探索で
/// 最小化する（決定的・乱数なし）。a の探索域は 10〜20000cp（k = 5e-5〜0.1）。
fn calibrate(samples: impl Iterator<Item = (f64, f64)>) -> f64 {
    let data: Vec<(f64, f64)> = samples.collect();
    // NLL(k) = Σ [ w·softplus(-k·e) + (1-w)·softplus(k·e) ]。
    let nll = |k: f64| -> f64 {
        data.iter()
            .map(|&(e, w)| {
                let z = k * e;
                w * softplus(-z) + (1.0 - w) * softplus(z)
            })
            .sum::<f64>()
    };
    // 黄金分割で凸 1 変数最小化（k ∈ [k_lo, k_hi]）。
    let (mut lo, mut hi) = (5e-5_f64, 0.1_f64);
    let inv_phi = (5.0_f64.sqrt() - 1.0) / 2.0;
    let mut c = hi - (hi - lo) * inv_phi;
    let mut d = lo + (hi - lo) * inv_phi;
    let mut fc = nll(c);
    let mut fd = nll(d);
    for _ in 0..200 {
        if fc < fd {
            hi = d;
            d = c;
            fd = fc;
            c = hi - (hi - lo) * inv_phi;
            fc = nll(c);
        } else {
            lo = c;
            c = d;
            fc = fd;
            d = lo + (hi - lo) * inv_phi;
            fd = nll(d);
        }
        if (hi - lo).abs() < 1e-9 {
            break;
        }
    }
    let k = 0.5 * (lo + hi);
    1.0 / k
}

/// Spearman 順位相関（同順位は平均順位）。スケール不変なので生 eval で取れる。
fn spearman_corr(xs: &[f64], ys: &[f64]) -> f64 {
    if xs.len() < 2 {
        return f64::NAN;
    }
    let rx = average_ranks(xs);
    let ry = average_ranks(ys);
    pearson(&rx, &ry)
}

/// 同順位を平均順位にした順位ベクトルを返す。
fn average_ranks(v: &[f64]) -> Vec<f64> {
    let n = v.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| v[a].partial_cmp(&v[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut ranks = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && v[idx[j]] == v[idx[i]] {
            j += 1;
        }
        // [i, j) が同値 → 平均順位 (1-based の平均)。
        let avg = ((i + 1 + j) as f64) / 2.0;
        for &k in &idx[i..j] {
            ranks[k] = avg;
        }
        i = j;
    }
    ranks
}

fn pearson(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for (&x, &y) in xs.iter().zip(ys) {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
    }
    if sxx <= 0.0 || syy <= 0.0 {
        return f64::NAN;
    }
    sxy / (sxx.sqrt() * syy.sqrt())
}

/// 出現順を安定させた distinct（BTreeMap で key ソート、決定的）。
fn distinct<'a>(it: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut m: BTreeMap<&'a str, ()> = BTreeMap::new();
    for x in it {
        m.insert(x, ());
    }
    m.into_keys().collect()
}

fn print_report(r: &FileReport) {
    println!("\n=== {} ===", r.file);
    println!(
        "n_total={}  n_scored(non-mate)={}  a_label={:.1}cp  a_ref={:.1}cp  ref_ceiling(all,sign)={:.4}",
        r.n_total, r.n_scored, r.a_label, r.a_ref, r.ref_ceiling_all
    );
    println!(
        "{:<22} {:>7} {:>10} {:>10} {:>9} {:>9} {:>9} {:>9}",
        "group", "n", "lbl_loss", "ref_loss", "lbl_sgn", "ref_sgn", "wp_mae", "spearman"
    );
    for g in &r.groups {
        println!(
            "{:<22} {:>7} {:>10.4} {:>10.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4}",
            g.group,
            g.n,
            g.label_logloss,
            g.ref_logloss,
            g.label_sign_acc,
            g.ref_sign_acc,
            g.winprob_mae,
            g.spearman
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_and_softplus_basic() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-12);
        assert!((softplus(0.0) - 2.0_f64.ln()).abs() < 1e-12);
        // softplus(x) - softplus(-x) == x
        for x in [-5.0, -1.0, 0.0, 1.0, 5.0] {
            assert!((softplus(x) - softplus(-x) - x).abs() < 1e-9);
        }
    }

    #[test]
    fn sign_acc_counts_agreement() {
        // eval>=0 ↔ wdl>=0.5 が一致する割合。
        let s = vec![(100, 1.0), (-100, 0.0), (50, 0.0), (-50, 1.0)];
        assert!((sign_acc(s.into_iter()) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn calibrate_recovers_known_scale() {
        // win_prob = sigmoid(eval/a_true) で生成した soft target を入れ、a_true を復元できるか。
        let a_true = 300.0;
        let samples: Vec<(f64, f64)> = (-2000..=2000)
            .step_by(10)
            .map(|e| (e as f64, sigmoid(e as f64 / a_true)))
            .collect();
        let a = calibrate(samples.into_iter());
        assert!((a - a_true).abs() < 5.0, "recovered a={a}, expected {a_true}");
    }

    #[test]
    fn spearman_monotone_is_one() {
        let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let ys = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        assert!((spearman_corr(&xs, &ys) - 1.0).abs() < 1e-9);
        let yr = vec![50.0, 40.0, 30.0, 20.0, 10.0];
        assert!((spearman_corr(&xs, &yr) + 1.0).abs() < 1e-9);
    }

    #[test]
    fn average_ranks_handles_ties() {
        // 同値は平均順位。
        let r = average_ranks(&[10.0, 10.0, 20.0]);
        assert_eq!(r, vec![1.5, 1.5, 3.0]);
    }
}
