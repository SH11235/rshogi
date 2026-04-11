---
description: NNUE モデルの棋力評価。指定エンジン間の総当たり自己対局を実行し、結果を集計する。「評価して」「対局させて」等の棋力比較リクエストに使用する。
user-invocable: true
---

# 自己対局評価スキル

以下の指示に従い、指定されたエンジン間の総当たり自己対局を実行し、結果を集計する。

## 入力パラメータ

ユーザーから以下の情報を `$ARGUMENTS` として受け取る。
情報が不足している場合は質問して補完すること。

### 必須情報
- **対象エンジン一覧**: 各エンジンの commit ハッシュ（短縮可）、バイナリパス、説明
- **確認ポイント**: 特に注目する比較（例: "E vs D: TT 16bit の棋力効果"）

### デフォルト値（指定がなければ以下を使用）
- **開始局面**: `--startpos-file start_sfens_ply32.txt`（**必須**。平手からの対局は序盤の偏りで正確な棋力を測れないため、必ず開始局面集を使用すること）
- 秒読み: 1000ms
- スレッド: 1
- ハッシュ: 256MB
- 各方向の対局数: 100（双方向で200局/カード）
- 並列数: 20
- NNUE: エンジンごとに `--engine-usi-option` で個別指定

## ビルドの注意

### Build profile

NPS 比較を含む評価では、**全エンジンを同一の build profile でビルドすること**。

- `--release` (release profile): `lto=thin`, `codegen-units=4`, `overflow-checks=true`
- `--profile production` (production profile): `lto=fat`, `codegen-units=1`, `overflow-checks=false`

production は release より約 1.5% 低い instruction/node を達成する。異なる profile のバイナリを比較すると、コード変更に起因しない NPS 差が発生し、評価結果にバイアスが生じる。

**`/tmp/` に保存された過去のバイナリを基準に使う場合は、どの profile でビルドされたかを必ず確認すること。** 不明なら再ビルドして揃える。

### Cargo feature（NNUE モデル別）

各モデルに対して**必要十分な feature を指定**してビルドすること。feature が不足するとモデル読み込み失敗、過剰だと不要なコードパスや accumulator フィールドが残り NPS にバイアスが生じる。

**モデル → feature 対応表**:

feature は以下の4カテゴリの組み合わせで構成する:

1. **dispatch 除去**: `layerstack-only`（LayerStack モデルでは常に指定）
2. **L1 サイズ**: `layerstacks-1536` / `layerstacks-768` / `layerstacks-512`（**必ず1つだけ**。複数同時有効は cycles +5.5% 退行）
3. **アーキテクチャ拡張**: `nnue-psqt`, `nnue-threat`（モデルに応じて）
4. **最適化**: `nnue-progress-diff`（L1=1536 で有効。L1=768 では cache pressure 増加により cycles +2〜6% 退行するため指定しない）

| モデル種別 | 例 | 必須 feature |
|---|---|---|
| LayerStack 1536 | v87 | `layerstack-only,layerstacks-1536,nnue-progress-diff` |
| LayerStack 1536 + PSQT | v88 | `layerstack-only,layerstacks-1536,nnue-psqt,nnue-progress-diff` |
| LayerStack 1536 + Threat | v89, v91-1536 | `layerstack-only,layerstacks-1536,nnue-threat` |
| LayerStack 1536 + PSQT + Threat | v90 | `layerstack-only,layerstacks-1536,nnue-psqt,nnue-threat` |
| LayerStack 768 + Threat | v91-768 系 | `layerstack-only,layerstacks-768,nnue-threat` |
| LayerStack 512 | | `layerstack-only,layerstacks-512` |
| HalfKA_HM | danbo-v20 等 | (feature 指定なし、デフォルトで可) |

**注意**:
- `layerstacks-1536` はデフォルト feature に含まれる。768/512 モデル用にビルドする際は
  `--no-default-features` で外す。その場合 `search-no-pass-rules`（デフォルトに含まれる）も
  明示的に再指定すること。
- `nnue-progress-diff` は L1=1536 限定の最適化。L1=768 では `StackEntryLayerStacks` の cache pressure 増加で退行する。

```bash
# 768 + Threat モデル用の例
cargo build --profile production -p rshogi-usi \
  --no-default-features \
  --features search-no-pass-rules,layerstack-only,layerstacks-768,nnue-threat
```

**`layerstack-only` の効果**: HalfKP/HalfKA/HalfKA_HM のコードを除去し、`evaluate_dispatch` を直接呼び出しにバイパスする。LayerStack モデル同士の比較では常に指定すべき。

**ビルド例**:
```bash
# v87 用（LayerStack 1536, PSQT なし）
cargo build --profile production -p rshogi-usi --features layerstack-only
cp target/production/rshogi-usi target/production/rshogi-usi-ls1536

# v88 用（LayerStack 1536 + PSQT）
cargo build --profile production -p rshogi-usi --features layerstack-only,nnue-psqt
cp target/production/rshogi-usi target/production/rshogi-usi-ls1536-psqt
```

**重要**: `cargo build` は同一 profile で feature が異なっても同じ出力パスに書き出す。異なる feature のバイナリが必要な場合は、ビルド直後に別名にコピーすること。2つ目のビルドで1つ目が上書きされる。

## 実行手順

### 1. ビルド条件の決定とバイナリ準備

#### 1a. 各エンジンの必要 feature を決定

NNUE モデル比較の場合、モデルごとに必要な Cargo feature が異なる。
上記「モデル → feature 対応表」を参照し、各エンジンに必要十分な feature セットを決定する。

**判断基準**: モデルのアーキテクチャ（LayerStack サイズ、PSQT 有無、Threat 有無）から feature を特定する。不明な場合はモデルの実験ドキュメント（`bullet-shogi/docs/experiments/`）を参照。

#### 1b. ビルドと退避

feature が異なるバイナリが複数必要な場合、**ビルド → 即座に別名コピー** を繰り返す。
`cargo build` は同一 profile・同一 crate で feature が異なっても同じ出力パスに書き出すため、
コピーしないと次のビルドで上書きされる。

```bash
# 例: v87 用と v88 用を順番にビルド
cargo build --profile production -p rshogi-usi --features layerstack-only,layerstacks-1536,nnue-progress-diff
cp target/production/rshogi-usi target/production/rshogi-usi-ls1536

cargo build --profile production -p rshogi-usi --features layerstack-only,layerstacks-1536,nnue-psqt,nnue-progress-diff
cp target/production/rshogi-usi target/production/rshogi-usi-ls1536-psqt
```

#### 1c. ビルド後の検証（必須）

1. **ビルドコマンドの feature 確認**: 各バイナリが対応表どおりの feature でビルドされたことを、ビルドログ（`cargo build` の出力）で確認する。feature が不足していればモデル読み込み時にエラーになるが、**過剰な feature は readyok を通過してしまい検出できない**。ビルドコマンド自体が正しいことを確認するのが唯一の手段。

2. **モデル読み込み確認**: 各バイナリに対象モデルを読み込ませて `readyok` を確認する（feature 不足の検出）。
   ```bash
   echo -e "usi\nsetoption name EvalFile value {MODEL_PATH}\n{OTHER_OPTIONS}\nisready\nquit" \
     | timeout 10 {BINARY_PATH} 2>&1 | grep -E 'readyok|Error|panic'
   ```

#### 1d. 既存バイナリの利用

事前ビルド済みバイナリを使う場合は、以下を確認する:
- どの profile (`release` / `production`) でビルドされたか
- どの feature でビルドされたか
- 不明なら再ビルドして揃える

### 2. 出力ディレクトリの作成

実験ごとに個別のディレクトリを作成し、ログファイルの混入を防ぐ。

```
mkdir -p runs/selfplay/{YYYYMMDD}-{HHMMSS}-{PURPOSE}
```

- `{PURPOSE}` はユーザーの実験目的を短く要約したもの（例: `tt-16bit`, `lmr-tuning`）
- このディレクトリパスを以降の `--out-dir` オプションで使用する

### 3. tournament バイナリで総当たり自己対局を実行

`tournament` バイナリ1コマンドで、全ペアの総当たり対局を並列実行する。
`--engine` を複数指定すると自動で C(N,2) ペアの対局を生成する。

```
cargo run -p tools --release --bin tournament -- \
  --engine {ENGINE_A} --engine {ENGINE_B} [--engine {ENGINE_C} ...] \
  --games {GAMES} --byoyomi {BYOYOMI} --hash-mb {HASH} --threads {THREADS} \
  --concurrency {CONCURRENCY} \
  --usi-option {NNUE} \
  --out-dir runs/selfplay/{DIR}
```

- `--concurrency`: 並列対局数（デフォルト1）。CPUコア数に応じて調整。
- `--report-interval`: N局ごとに進捗を表示（デフォルト10）。
- `--engine-usi-option "INDEX:Name=Value"`: エンジン個別の USI オプション（0始まりインデックス）。
  指定したエンジンは共通 `--usi-option` が**完全に置換**される（マージではない）。
- 出力は以下の2種類が `{out-dir}` に自動生成される:
  - `{label_i}-vs-{label_j}.jsonl`: ペア別の棋譜ログ（各対局の指し手・評価値・結果）
  - `meta.json`: 対局設定・エンジン情報をまとめたファイル。対局条件の確認・再現に利用可能。

**注意:** `run_in_background: true` で起動し、`TaskOutput` で完了を監視すること。

#### 外部エンジンとの対局例

rshogi と YaneuraOu のように異なるエンジンを対局させる場合、
エンジンごとに必要な USI オプションが異なるため `--engine-usi-option` を使う:

```
cargo run -p tools --release --bin tournament -- \
  --engine target/rshogi-usi-{HASH} \
  --engine /path/to/YaneuraOu-binary \
  --engine-usi-option "0:EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin" \
  --engine-usi-option "1:EvalDir=/path/to/eval" \
  --engine-usi-option "1:BookFile=no_book" \
  --games 100 --byoyomi 3000 --concurrency 5 \
  --out-dir runs/selfplay/{DIR}
```

### 4. 完了待ち・結果集計

Background task の完了を `TaskOutput` で検知する。
完了後、`analyze_selfplay` ツールで対局ログを集計しサマリを生成する:

```
cargo run -p tools --release --bin analyze_selfplay -- runs/selfplay/{DIR}/*.jsonl
```

`analyze_selfplay` は JSONL ファイルを読み込み、勝率・Elo差・手数分布などを集計して標準出力に表示する。
この出力を元に、以下の内容をマークダウンファイル（`docs/performance/` 配下）に出力する:

1. **対局条件**: 秒読み・スレッド・ハッシュ・対局数・NNUE
2. **総合結果表**: 各カードの勝敗・勝率・Elo差
3. **確認ポイントの評価**: ユーザーが指定した比較ポイントについての分析
4. **総括**: 全体的な傾向と推奨事項

## 入力例

```
/selfplay エンジン:
- A: 3526b075 target/rshogi-usi-3526b075 ベースライン
- B: 232d847d target/rshogi-usi-232d847d move ordering完了
- D: 4778e1c6 target/rshogi-usi-4778e1c6 LMR修正（TT変更前）
- E: 5806777e target/rshogi-usi-5806777e TT 16bit（最新）

確認ポイント:
1. E vs D: TT 16bit の棋力効果
2. E vs A: 全修正+TT の総合効果
3. B vs D: Step14+LMR が move ordering 完了時点より良いか悪いか
```
