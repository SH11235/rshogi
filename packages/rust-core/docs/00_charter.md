# Charter (横断契約)

本ドキュメントは、学習パイプラインとエンジン改良（運用改善/棋力向上）の両輪で共通に従う“契約”を定義します。測定条件・昇格Gate・データ/ログ契約を固定し、誰が回しても再現できる基盤を提供します。

## 測定条件（固定）
- 時間設定: `0/1+0.1`
- Threads: `1` / Hash: `256MB`（CLI: `--hash-mb 256`） / MultiPV: `3`
- 開幕ブック: `fixed-100.epd`（固定）
- 対局数: `200`（昇格判定の最小）

### 実行環境の固定（必須）
- CPU: `model`, `uarch`, `SIMD flags(AVX2/AVX512/NEON)` を記録
- OS: `name`, `version`, `kernel`
- Rust toolchain: `stable-1.xx` 固定（`rustc -V` を記録）
- 並列: `RAYON_NUM_THREADS=1`（明示）
- 乱数Seed: `seed.model_init` と `seed.data_shuffle` を分離して記録（manifest 必須項目）

## Gate（昇格条件）
- 勝率 +5%pt 以上 かつ NPS ±3% 以内
- 統計判定: Wilson区間（95%）の下限が 50% を超える場合を準合格条件として扱う
- SPRT: ±10 Elo 相当で合格（必要時に実施）
- PV安定: `pv_spread_cp = P90( max_i s_i - min_i s_i )`（i は MultiPV=3 の候補、s は root cp）
  - Gate: ベースラインの `pv_spread_cp + 30cp` を超えない

### NPS 測定方法（固定スイート）
- 固定 100 局面スイート（リポジトリ管理）で `nodes/s` を測定し平均する
- 判定の ±3% は「同一環境・同条件・同スイート」での相対比較

## データ契約（manifest v2）
必須フィールド：
- `teacher`, `usi_opts`, `seed`
- `output_sha256`, `output_bytes`
- `summary`（runスコープの統計・aggregated.multipv 等を含む）

生成・分割規則：
- 分割前dedup → cross-dedup を必須とし、`leak_report` を出力
- WDL `scale=600` を標準（変更時は構造化ログへ記録）
- ライセンス: `license_scope`（`internal`/`research`/`release`）を記録し、下流でフィルタ可能にする
- 相反注釈の解決優先度: `深さ > 時間 > 新しさ` を採用し、manifest に由来情報を保持

## ログ契約（structured v1）
- 共通キー: `global_step`, `epoch`, `wall_time`
- 学習: `lr`, `train_loss`, `examples_sec`, `loader_ratio`
- 検証: `val_loss`, `val_auc`, `exact_rate`, `ambiguous_rate`, `depth_hist[]`, `timeout_rate`

### スキーマ（機械可読）
- 学習ログ: `docs/schemas/structured_v1.schema.json`
- Gauntlet 出力: `docs/schemas/gauntlet_out.schema.json`
- Manifest v2: `docs/schemas/manifest_v2.schema.json`

depth_hist ビン定義（固定）
- `[0..3], [4..7], [8..11], [12..15], [16..19], [20..23], [24+]`

運用上の取り決め：
- すべての実験は JSONL で構造化ログを出力する（可視化はダッシュボード側で生成）
- Gate 判定・ガントレット集計は固定条件を使用し、レポートを `docs/reports/` に保存する
