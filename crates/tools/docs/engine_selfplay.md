# engine_selfplay

USIエンジン同士の自己対局ツール。対局ログの記録と、NNUE学習用データ（PackedSfenValue形式）の生成を同時に行える。

## 基本的な使い方

```bash
# 10局対局（デフォルトで学習データも出力）
cargo run -p tools --bin engine_selfplay --release -- \
  --games 100 --byoyomi 1000 --threads 2 --hash-mb 256 \
  --usi-option-black "MaterialLevel=9" \
  --usi-option-white "EvalFile=./eval/halfkp_256x2-32-32_crelu/suisho5.bin"
```

## 出力ファイル

デフォルトで `runs/selfplay/` に以下が出力される：

| ファイル | 説明 |
|----------|------|
| `<timestamp>-selfplay.jsonl` | 対局ログ（各手の情報） |
| `<timestamp>-selfplay.pack` | 学習データ（PackedSfenValue形式） |
| `<timestamp>-selfplay.kif` | KIF形式の棋譜 |
| `<timestamp>-selfplay.summary.jsonl` | 対局結果サマリー |

## 主要オプション

### 対局設定

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `--games <N>` | 対局数 | 1 |
| `--max-moves <N>` | 最大手数（超えると引き分け） | 512 |
| `--byoyomi <MS>` | 秒読み（ミリ秒） | 0 |
| `--btime / --wtime` | 先手/後手の持ち時間（ミリ秒） | 0 |
| `--binc / --winc` | 先手/後手の加算時間（ミリ秒） | 0 |

### エンジン設定

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `--engine-path <PATH>` | エンジンバイナリパス | 自動検出 |
| `--engine-path-black / --engine-path-white` | 先手/後手別のエンジン | - |
| `--threads <N>` | スレッド数 | 1 |
| `--hash-mb <MB>` | ハッシュサイズ | - |

### 学習データ設定

| オプション | 説明 | デフォルト |
|------------|------|------------|
| `--output-training-data <PATH>` | 出力先パス | `<output>.pack` |
| `--no-training-data` | 学習データ出力を無効化 | false |
| `--skip-initial-ply <N>` | 序盤N手をスキップ | 8 |
| `--skip-in-check` | 王手局面をスキップ | true |

## 学習データについて

### 形式

やねうら王互換の PackedSfenValue 形式（40バイト/局面）：
- 局面（PackedSfen）
- 探索スコア（センチポーン）
- 最善手
- 手数
- 勝敗結果（1=勝ち, 0=引き分け, -1=負け）

### 従来との違い

従来のパイプライン：
1. 自己対局で棋譜生成
2. `rescore_pack` で探索スコアを付与

新しいパイプライン：
1. 自己対局しながらスコアを同時記録（探索1回で済む）

### スキップ設定

- `--skip-initial-ply 8`: 序盤8手は定跡の影響が大きいためスキップ
- `--skip-in-check`: 王手局面は特殊なためスキップ（学習データの質向上）

## 使用例

### 基本（学習データ自動出力）

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 100 --byoyomi 1000
```

### 学習データなしで対局のみ

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 10 --byoyomi 1000 --no-training-data
```

### 異なるエンジン同士の対局

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 50 --byoyomi 5000 \
  --engine-path-black ./engine_v1 \
  --engine-path-white ./engine_v2
```

### 長い持ち時間での対局

```bash
cargo run -p tools --release --bin engine_selfplay -- \
  --games 10 \
  --btime 300000 --wtime 300000 \
  --binc 5000 --winc 5000
```

### 特定局面から開始

```bash
# sfen.txt に開始局面を記載
cargo run -p tools --release --bin engine_selfplay -- \
  --games 10 --byoyomi 1000 \
  --startpos-file sfen.txt
```

### YaneuraOuエンジンを使用した評価

YaneuraOuエンジンを使用する場合は、`--engine-path-*` でバイナリを指定し、`--usi-option-*` で `EvalDir` を設定します。

**注意**: YaneuraOuは `EvalFile` ではなく `EvalDir` オプションを使用し、指定ディレクトリ内の `nn.bin` を読み込みます。

```bash
# 自作モデル vs 外部モデル（YaneuraOu同士）
cargo run -p tools --bin engine_selfplay --release -- \
  --games 50 --byoyomi 1000 --threads 2 --hash-mb 256 \
  --engine-path-black /path/to/yaneuraou/YaneuraOu-halfkahm_512x2-8-64 \
  --engine-path-white /path/to/yaneuraou/YaneuraOu-halfkp_768x2-16-64 \
  --usi-option-black "EvalDir=eval/my_model" \
  --usi-option-white "EvalDir=eval/AobaNNUE"
```

```bash
# rshogi vs YaneuraOu（エンジン実装の比較）
cargo run -p tools --bin engine_selfplay --release -- \
  --games 50 --byoyomi 1000 --threads 2 --hash-mb 256 \
  --engine-path-black /path/to/rshogi-usi \
  --engine-path-white /path/to/yaneuraou/YaneuraOu-halfkp_768x2-16-64 \
  --usi-option-black "EvalFile=/path/to/my_model.bin" \
  --usi-option-white "EvalDir=eval/AobaNNUE"
```

**ディレクトリ構造例（YaneuraOu用）:**

```
yaneuraou/
├── YaneuraOu-halfkahm_512x2-8-64   # バイナリ
├── YaneuraOu-halfkp_768x2-16-64    # バイナリ
└── eval/
    ├── my_model/nn.bin             # 自作モデル
    ├── AobaNNUE/nn.bin             # 外部モデル
    └── suisho5/nn.bin              # 外部モデル
```

## 出力例

```
using engine binary: target/release/engine-usi (auto:release)
threads: 1
game 1/100: black_win (resign) - black 1 / white 0 / draw 0
game 2/100: white_win (resign) - black 1 / white 1 / draw 0
...

=== Result Summary ===
Total: 100 games | Black wins: 52 | White wins: 45 | Draws: 3
Win rate: Black 52.0% | White 45.0% | Draw 3.0%

--- Training Data ---
Total positions written: 8234
Skipped (initial ply ≤ 8): 800
Skipped (in check): 156
Output: runs/selfplay/20260119-120000-selfplay.pack
---------------------
```

## 関連ツール

- `shuffle_pack` - 生成した学習データをシャッフル
- `pack_to_jsonl` - 学習データの内容を確認
