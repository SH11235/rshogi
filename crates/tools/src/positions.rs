//! ベンチマーク用局面定義と読み込み

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::BenchmarkConfig;

/// YaneuraOu準拠のデフォルトベンチマーク局面
pub const DEFAULT_POSITIONS: &[(&str, &str)] = &[
    // 1. 初期局面に近い局面
    (
        "hirate-like",
        "lnsgkgsnl/1r7/p1ppp1bpp/1p3pp2/7P1/2P6/PP1PPPP1P/1B3S1R1/LNSGKG1NL b - 9",
    ),
    // 2. 読めば読むほど後手悪いような局面
    (
        "complex-middle",
        "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
    ),
    // 3. 57同銀は詰み、みたいな。読めば読むほど先手が悪いことがわかってくる局面
    (
        "tactical",
        "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
    ),
    // 4. 指し手生成祭りの局面
    // cf. http://d.hatena.ne.jp/ak11/20110508/p1
    (
        "movegen-heavy",
        "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
    ),
];

/// 局面を読み込む
pub fn load_positions(config: &BenchmarkConfig) -> Result<Vec<(String, String)>> {
    if let Some(path) = &config.sfens {
        load_positions_from_file(path)
    } else {
        Ok(DEFAULT_POSITIONS
            .iter()
            .map(|(name, sfen)| (name.to_string(), sfen.to_string()))
            .collect())
    }
}

/// SFEN局面ファイルを読み込む
fn load_positions_from_file(path: &Path) -> Result<Vec<(String, String)>> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open positions file: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut positions = Vec::new();

    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        let line = line.trim();

        // コメント行と空行をスキップ
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // "name | sfen" 形式をパース
        if let Some((name, sfen)) = line.split_once('|') {
            positions.push((name.trim().to_string(), sfen.trim().to_string()));
        } else {
            // 区切り文字がない場合は、インデックスを名前として使用
            positions.push((format!("position_{}", idx + 1), line.to_string()));
        }
    }

    if positions.is_empty() {
        anyhow::bail!("No positions found in file: {}", path.display());
    }

    Ok(positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_positions() {
        assert!(!DEFAULT_POSITIONS.is_empty());
        assert_eq!(DEFAULT_POSITIONS.len(), 4); // YaneuraOu準拠で4局面

        for (name, sfen) in DEFAULT_POSITIONS {
            assert!(!name.is_empty());
            assert!(!sfen.is_empty());
        }
    }
}
