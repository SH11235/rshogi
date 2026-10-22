// 探索・評価の結果を変えない feature。これ以外の有効な feature は、同じソースでも読みの条件を
// 変えるものとしてソースの版に付ける (新しく足した feature は既定で条件を変える側に入る)。
// - default・edition-*・mode-*: 下の atomic feature を束ねるだけ
// - layerstack-arch / halfkx-arch / layerstacks-* / ft-* / halfkx-activation-* / nnue-psqt /
//   nnue-threat / nnue-runtime-dimensions: 読み込めるモデルの形式の選択。値はモデルの内容で決まる
// - search-stats / nnue-stats / tt-write-stats / allocation-stats / diagnostics: 計数と診断の出力だけ
// - move-features: アプリ向けの特徴量の API を足すだけ
// - wasm-threads: WebAssembly のスレッド実装
// - debug / simd_avx2 / search-no-pass-rules: 何もしない互換用
const RESULT_NEUTRAL_FEATURES: &[&str] = &[
    "default",
    "debug",
    "simd_avx2",
    "search-no-pass-rules",
    "search-stats",
    "nnue-stats",
    "tt-write-stats",
    "allocation-stats",
    "diagnostics",
    "move-features",
    "wasm-threads",
    "layerstack-arch",
    "halfkx-arch",
    "nnue-psqt",
    "nnue-threat",
    "nnue-runtime-dimensions",
];
const RESULT_NEUTRAL_PREFIXES: &[&str] =
    &["edition-", "mode-", "layerstacks-", "ft-", "halfkx-activation-"];

/// Cargo.toml の `[features]` の名前を記載順に返す。
fn manifest_features(manifest: &str) -> Vec<String> {
    let mut in_features = false;
    let mut names = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') && !line.starts_with("[\"") {
            in_features = line == "[features]";
            continue;
        }
        if !in_features {
            continue;
        }
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            names.push(name.to_owned());
        }
    }
    names
}

/// 有効な feature のうち、探索・評価の結果を変えうるものを名前順に返す。
fn result_features(manifest: &str, enabled: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut features: Vec<String> = manifest_features(manifest)
        .into_iter()
        .filter(|name| enabled(name))
        .filter(|name| {
            !RESULT_NEUTRAL_FEATURES.contains(&name.as_str())
                && !RESULT_NEUTRAL_PREFIXES.iter().any(|p| name.starts_with(p))
        })
        .collect();
    features.sort();
    features.dedup();
    features
}
