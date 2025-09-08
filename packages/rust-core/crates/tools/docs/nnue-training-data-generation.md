# NNUE学習データ生成

このドキュメントでは、NNUE（Efficiently Updatable Neural Network）評価関数用の学習データを生成する方法について説明します。生成ツールはエンジン種別・NNUE重み・ラベル種別（CP/WDL/Hybrid）を指定できるようになりました。

## 概要

`generate_nnue_training_data` ツールは、SFEN局面ファイルを処理し、各局面に対して探索を実行して評価データを生成します。UnifiedSearcher フレームワーク上で動作し、以下のエンジンから選択できます：`material` / `enhanced` / `nnue` / `enhanced-nnue`。効率的なデータ生成のために並列処理を実装しています。

### 主な特徴

- **エンジン選択**: `--engine material|enhanced|nnue|enhanced-nnue`
- **NNUE重み読み込み**: `--nnue-weights <path>` でNNUE系に外部重みをロード
- **ラベル選択**: `--label cp|wdl|hybrid`、WDLスケール `--wdl-scale`、Hybrid切替 `--hybrid-ply-cutoff`
- **段階的な探索深度設定**: 初期は浅い探索（深度2）、品質向上では深い探索（深度4以上）
- **時間制限の上書き**: `--time-limit-ms <ms>`
- **TTサイズの指定**: `--hash-mb <MB>`（バッチ並列時のメモリ制御）
- **レジューム機能**: 中断しても自動的に続きから再開
- **スキップ局面の保存**: タイムアウト局面を別ファイルに保存し後で再処理
- **進捗追跡**: `.progress` で実試行数を管理（スキップ含む）
- **並列処理**: Rayonで全CPUコアを活用
- **構造化ログ（JSONL）**: `--structured-log <PATH|->` でバッチ/最終サマリを機械可読に記録
- **manifest 要約（summary）**: 実行のサマリ（throughput/rates/曖昧率/深さヒスト/件数）を manifest v2 に埋め込み
- **分割出力**: `--split <N>` / `--compress <gz|zst>` に対応（各 part manifest + 親の集約 manifest を自動出力）

### エンジン選択の指針

- 初回の大量収集には `material` または `enhanced` が高速で安定。
- 品質重視やWDL/Hybrid用途には `enhanced` か `nnue`/`enhanced-nnue` を推奨（重み指定可）。
- NNUEを選ぶ場合は `--nnue-weights` で学習済み重みを指定するか、未指定時はゼロ重み（精度は低い）。

## 前提条件

1. 最適なパフォーマンスのために、リリースモードでツールをビルドします：
```bash
cd packages/rust-core
cargo build --release --bin generate_nnue_training_data
```

## 使用方法

### 実行ディレクトリ

`packages/rust-core` ディレクトリから実行してください：

```bash
cd packages/rust-core
```

### 基本コマンドとオプション

```bash
./target/release/generate_nnue_training_data \
  <入力SFENファイル> <出力学習データファイル> [深度] [バッチサイズ] [再開位置] \
  [--engine material|enhanced|nnue|enhanced-nnue] \
  [--nnue-weights <path>] \
  [--label cp|wdl|hybrid] [--wdl-scale <float>] [--hybrid-ply-cutoff <u32>] \
  [--time-limit-ms <u64>] [--hash-mb <usize>]
```

- `深度`: 探索深度（デフォルト: 2、範囲: 1–10）
- `バッチサイズ`: 並列処理する局面数（デフォルト: 50）
- `再開位置`: 行番号で再開（デフォルト: 0 = 自動検出）
- `--engine`: 使用エンジン（デフォルト: `material`）
- `--nnue-weights`: NNUE重みファイル（`nnue`/`enhanced-nnue` 選択時に任意）
- `--label`: ラベル種別（`cp`=評価回帰、`wdl`=勝率、`hybrid`=手数で切替）
- `--wdl-scale`: CP→WDL写像のスケール（デフォルト: 600.0）
- `--hybrid-ply-cutoff`: `ply <= cutoff` でWDL、それ以降CP（デフォルト: 100）
- `--time-limit-ms`: 深度ごとの既定値を上書き
- `--hash-mb`: TTサイズ（MB、デフォルト: 16）

#### 追加オプション（抜粋）

- 出力形式/分割/圧縮:
  - `--output-format text|jsonl`（既定: text）
  - `--split <N>`（N行ごとに `<stem>.part-0001.*` などへローテーション）
  - `--compress gz|zst`（分割出力時に圧縮。`zst`は `--features zstd` が必要）
  - `--structured-log <PATH|->` 構造化ログ（JSONL）をファイル or STDOUT（`-`）へ出力
- 予算/下限/並列:
  - `--nodes <N>`（ノード固定。指定時は `--time-limit-ms` 無効）
  - `--min-depth <d>`（位置引数 `depth` と `max` を取る）
  - `--jobs <n>`（外側並列。各エンジンは単スレッド）
- キャリブレーション（ノード自動化）:
  - `--nodes-autocalibrate-ms <ms>` / `--calibrate-sample <k>`
  - `--no-recalib` / `--force-recalib`
- 曖昧/K=3:
  - `--amb-gap2-threshold <cp>`（既定 25）
  - `--amb-allow-inexact`（既定はExactのみを曖昧条件に採用）
  - `--entropy-mate-mode exclude|saturate`（K=3エントロピーの詰みスコア扱い）

### 段階的なデータ生成（推奨）

```bash
# Stage 1: 大量の浅い探索データ（深度2）
./target/release/generate_nnue_training_data input.sfen output_d2.txt 2 100 --engine material --hash-mb 16

# Stage 2: 中程度の探索データ（深度3）
./target/release/generate_nnue_training_data input.sfen output_d3.txt 3 50 --engine enhanced --hash-mb 16

# Stage 3: 高品質データ（深度4以上は慎重に）
./target/release/generate_nnue_training_data input.sfen output_d4.txt 4 25 --engine enhanced --hash-mb 16
```

### レジューム機能の使用

```bash
# 初回実行
./target/release/generate_nnue_training_data input.sfen output.txt 3 50

# 中断後の再開（自動的に続きから処理）
./target/release/generate_nnue_training_data input.sfen output.txt 3 50

# 明示的に再開位置を指定
./target/release/generate_nnue_training_data input.sfen output.txt 3 50 10000
```

### スキップされた局面の再処理

```bash
# スキップされた局面は自動的に別ファイルに保存される
# output_skipped.txt - スキップされた局面
# output.progress - 進捗追跡ファイル

# スキップされた局面を再処理（深度を上げて、バッチサイズを小さく）
./target/release/generate_nnue_training_data output_skipped.txt output_retry.txt 4 10
```

## 入力形式

入力ファイルは、1行に1つのSFEN局面を含む必要があります：
```
sfen +Bn1g2s1l/2skg2r1/ppppp1n1p/5bpp1/5p1P1/2P6/PP1PP1P1P/1SK2S1R1/LN1G1G1NL w Lp 24
sfen +R1G4nl/1g4+Ss1/1kspp2p1/ppp2pS1p/4n4/P4Gp1P/1P1PP1P2/1+n2K2R1/7NL w G2P2b2lp 24
```

## 出力形式

### メインの出力ファイル
評価値付きのSFEN局面が含まれます：
```
... w ... 24 eval -385 # d4 label:cp
... w ... 24 eval 160 wdl 0.623451 # d3 label:wdl
... w ... 24 eval 45 # timeout_d3 label:hybrid
```

出力要素:
- `eval <cp>`: サイド・トゥ・ムーブ視点の評価値（常に出力）
- `wdl <p>`: WDL確率（`--label wdl|hybrid` の場合のみ）
- `# d<depth>` / `# timeout_d<depth>`: 到達深さ/タイムアウト深さ
- `label:<cp|wdl|hybrid>`: ラベル種別のメタ情報
- `mate:<distance>`: 詰み検出時に距離を付加

### スキップファイル（_skipped.txt）
タイムアウトした局面の詳細情報：
```
sfen l1s1k2nl/1r1g2g2/2npppsp1/ppp3p1p/9/P1P2P1PP/1PSPPSP2/2G4R1/LN2KG1NL w Bb 24 # position 30 timeout 1.3s depth_reached 3
```

### 進捗ファイル（.progress）
実際に処理を試みた局面数（成功・スキップ両方含む）：
```
50
```

### 構造化ログ（JSONL, 任意）

`--structured-log <PATH|->` を指定すると、バッチ終了ごとに `kind="batch"`、終了時に `kind="final"` の JSON が1行ずつ出力されます。

例（抜粋）:
```
{"kind":"batch","batch_index":3,"size":4,"success":3,
 "elapsed_sec":1.21,"sps":3.30,"attempted_sps":3.30,
 "processed_total":12,"attempted_total":16,"percent":40.0}
{"kind":"final","summary":{ ... manifest summary と同内容 ... }}
```

### Manifest と Summary（v2）

非分割時は `<stem>.manifest.json`、分割時は各 part に `*.part-0001.manifest.json` を出力し、さらに親 `<stem>.manifest.json` を集約版として出力します。

- 親 manifest には `summary` を含みます（part manifest は省略）。
- `summary` には以下が含まれます（要約）:
  - `elapsed_sec`: 実行時間（秒）
  - `throughput { attempted_sps, success_sps }`
  - `rates { timeout, top1_exact, both_exact }`
  - `ambiguous { threshold_cp, require_exact, count, denom, rate }`
  - `depth { histogram, min, max, p50, p90 }`
  - `counts { attempted, success, skipped_timeout, errors{parse, nonexact_top1, empty_or_missing_pv} }`
- 分割出力時:
  - 各 part manifest の `count` は当該 part の成功件数（`count_in_part` と同値）
  - 親 manifest の `count` は全体の成功件数、`part_count` に総パート数を含む

## パフォーマンス

### 探索深度別の時間制限
- 深度1: 50ms
- 深度2: 100ms
- 深度3: 200ms
- 深度4: 400ms
- 深度5以上: 800ms

上記は既定値であり、`--time-limit-ms` で上書き可能です。

### 処理速度の目安
- 深度2: 約50-100局面/秒
- 深度3: 約20-50局面/秒
- 深度4: 約10-20局面/秒

ヒント:
- 本番・検証いずれも **`--release` ビルドを推奨**（デバッグは 5〜20× 遅い）
- **`material` + 深度1** は PV/Exact が得られないことが多く、フィルタ（top1=Exact）により**全ドロップ**になりがちです。
  - 初回の動作確認・本番生成には **`--engine enhanced` + `--min-depth 2`** を推奨
  - 時間/ノード予算を適度に増やす（例: `--time-limit-ms 200` か `--nodes 150000`）

## トラブルシューティング

### 処理が遅い・タイムアウトが多い場合

1. **探索深度を下げる**：
```bash
# 深度2で高速処理
./target/release/generate_nnue_training_data input.sfen output.txt 2 100
```

2. **バッチサイズを小さくする**：
```bash
# 25局面ずつ処理（より安定）
./target/release/generate_nnue_training_data input.sfen output.txt 3 25
```

3. **スレッド数を制限する**：
```bash
# スレッド数を4に制限
RAYON_NUM_THREADS=4 ./target/release/generate_nnue_training_data input.sfen output.txt
```

4. **成功行が0件のとき**

- `errors.empty_or_missing_pv` が多い: **PVが出ていない**可能性。`--engine enhanced`、`--min-depth 2+`、時間/ノード増。
- `errors.nonexact_top1` が多い: **Exactに届いていない**可能性。時間/ノード増、`--teacher-profile safe` も検討。

### レジューム時の注意点

レジューム機能は以下の3つのファイルを使用します：
- **出力ファイル**: 成功した結果のみ
- **スキップファイル**: タイムアウトした局面
- **進捗ファイル**: 実際の処理進捗

これらのファイルの整合性が保たれるよう、処理中はファイルを手動で編集しないでください。

### デバッグログ

詳細なログを出力する場合：
```bash
RUST_LOG=debug ./target/release/generate_nnue_training_data input.sfen output.txt
```

## 技術詳細

### エンジン設定
- エンジンタイプ：`material` / `enhanced` / `nnue` / `enhanced-nnue`
- NNUE重み：`--nnue-weights <path>`（未指定時はゼロ重み）
- 探索アルゴリズム：UnifiedSearcher（強化設定はLMR/NullMove/Futility等）
- トランスポジションテーブル：`--hash-mb` で指定（推奨 16MB）

### 並列処理
- Rayonを使用した自動並列化
- バッチ単位での処理でメモリ効率を向上
- 各バッチの結果は即座にファイルに書き込み

### タイムアウト処理
- 設定時間の2倍を超えた場合はスキップ
- スキップされた局面は別ファイルに保存
- 後で異なる設定で再処理可能

## 次のステップ

学習データを生成した後、以下の作業が必要です：

1. スキップされた局面の再処理（必要に応じて）
2. 専用ツールを使用してデータをNNUEバイナリ形式に変換
3. NNUE学習ツールを使用してニューラルネットワークの重みを作成

## 関連ドキュメント

- [エンジンタイプ選択ガイド](../../engine-core/docs/engine-type-selection.md)
- [UnifiedSearcherフレームワーク](../../docs/unified-searcher-design.md)
