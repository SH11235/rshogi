# rshogi tools

将棋エンジンの対局評価、教師データの生成・加工、性能計測、定跡整備のツール群です。

| やりたいこと | 入口 |
|---|---|
| 教師データを作る・加工する | [gensfen](docs/gensfen.md) / [データ処理の選び方](docs/pack_tools.md) |
| エンジンを対局で比較する | [tournament](docs/tournament.md) → [analyze_selfplay](docs/analyze_selfplay.md) |
| 探索速度を測る | [benchmark](docs/benchmark.md) / [search_only_ab](docs/search_only_ab.md) |
| その他のツールを探す | [目的別の全ツール一覧](docs/tools-reference.md) |

## ビルドと実行

リポジトリのルートで実行します。必要な Rust 環境は [ビルド手順](../../docs/build.md) を参照してください。
以下は Bash / Git Bash で実行する例です。Windows native のエンジンの実行ファイルには `.exe` を付けます。

```bash
cargo build --release -p tools --bin shuffle_psv
cargo run --release -p tools --bin shuffle_psv -- --help
```

通常は必要なバイナリだけを `--bin` で選びます。`kifu_player` と `dlshogi-onnx` は
既定で有効です。TUI・ONNX 依存を外す場合は
`--no-default-features --features nnue-arch` をビルド引数に追加します。
`prepack_nnue` は追加の `--features prepacked-nnue` が必要です。
1 つの NNUE 構成だけを入れた速い build (`rescore_psv` 等) は
[rescore_psv の単一構成の build](docs/rescore_psv.md#単一構成の-build) を参照してください。
静的 LayerStacks を直接読む診断ツール (`bench_nnue_eval` / `eval_sfens` / `nnue_saturation` /
`verify_nnue_accumulator`) には、tools の `layerstack-arch` feature (`nnue-arch` に含まれる) が必要です。

## クイックスタート

教師データ・NNUE モデル・進行度係数は repo 外の共有 root に置き、
環境変数 `SHOGI_DATA` に設定してください。以下の入力ファイルはあらかじめ用意します。
出力は実行ごとに別名にすると前回の結果を保持できます。

### 教師データの生成とシャッフル

```bash
# model.bin は使用する評価関数に置き換える。
RUN="$SHOGI_DATA/teachers/generated-$(date +%Y%m%d_%H%M%S)"
cargo run --release -p tools --bin gensfen -- \
  --eval-file "$SHOGI_DATA/nnue/model.bin" \
  --games 100 --nodes 80000 --threads 1 --hash-mb 256 \
  --out-dir "$RUN"

cargo run --release -p tools --bin shuffle_psv -- \
  --input "$RUN/gensfen.psv" --output "$RUN/shuffled.psv" \
  --seed 42 --chunk-size 1000000
```

`gensfen` は終了時に局面数を表示し、指定先へ PSV と結果 JSONL を保存します。
LayerStacks の routing・進行度係数が必要なモデルは
[起動時の設定条件](docs/nnue-routing.md#起動時の設定条件)に従って指定してください。
`shuffle_psv` のチャンク指定は全件のメモリ保持を避けるためです。メモリ・一時ディスク容量と
中断時の扱いは [shuffle_psv](docs/shuffle_psv.md)、生成の drain・再開は
[gensfen](docs/gensfen.md) を参照してください。学習自体は外部トレーナーで行います。
[bullet-shogi](https://github.com/SH11235/bullet-shogi/tree/shogi-support) を使う場合の学習手順は、そちらの文書を参照してください。

### 2 エンジンの対局と集計

評価関数などの初期設定が済んだ USI エンジンを用意します。
同じバイナリでモデルを比較する例は [tournament](docs/tournament.md) を参照してください。

```bash
RUN="runs/selfplay/$(date +%Y%m%d_%H%M%S)-compare"
cargo run --release -p tools --bin tournament -- \
  --engine /path/to/candidate --engine-label candidate \
  --engine /path/to/base --engine-label base \
  --games 10 --byoyomi 1000 --threads 1 --hash-mb 256 \
  --concurrency 2 --seed 42 --out-dir "$RUN"

cargo run --release -p tools --bin analyze_selfplay -- "$RUN"
```

`--games 10` は各方向 10 局なので、この例は先後を交換して計 20 局です。
開始局面集を使う比較では `--startpos-file` を追加します。
Fischer 時計の NPS による補正（`--nps-scaling`）と Linux の NUMA 固定（`--cpu-affinity numa`）は
[tournament の時計補正と CPU 割り当て](docs/tournament.md#機械の速さによる時計の補正---nps-scaling)を参照してください。
中断・動的な局数変更・SPRT は [tournament](docs/tournament.md)、
不完全入力の扱いは [analyze_selfplay](docs/analyze_selfplay.md) を参照してください。

### 探索の動作確認

```bash
cargo run --release -p tools --bin benchmark -- \
  --internal --usi-option MaterialLevel=1 \
  --limit-type nodes --limit 10000 --tt-mb 16 --eval-hash-mb 16
```

駒得評価で短く探索し、コンソールに NPS、`benchmark_results/` に JSON を保存します。
NNUE を使った性能比較では `--nnue-file` と必要な routing を明示し、
[benchmark](docs/benchmark.md) または [search_only_ab](docs/search_only_ab.md) の条件を揃えて計測します。

## 共通の注意と関連資料

- 各 CLI の全オプションは `cargo run --release -p tools --bin <名前> -- --help` で確認できます。
- 共通の入力ファイル収集は、読めないディレクトリを黙って除外せずエラーを返します。
- 入出力の保護・上書き・再開の契約はツールごとに異なります。各 doc の出力に関する説明を確認してください。
- [prep_hcpe](docs/prep_hcpe.md) はメモリ予算に応じて shuffle 方式を切り替え、開始時に使用量の見積もりを表示します。
- [HCPE の 38 byte 形式と padding](docs/hcpe-format.md)、[LayerStacks routing と進行度係数](docs/nnue-routing.md)。
- CSA client のログは単局集計です。未決の対局は勝敗集計・教師抽出から除外し、SPRT の対象にしません。
  詳細は [解析](docs/analyze_selfplay.md) と [KIF 変換](docs/jsonl_to_kif.md) を参照してください。
- Floodgate の HTTPS 取得は OS の証明書ストアを使います。最小 Linux コンテナには `ca-certificates` を用意してください。
  暗号 provider は `ring` で、ビルドには C コンパイラが必要です（CMake / NASM は不要）。
- [コマンド例集](examples/README.md)、[教師データの追加パイプライン](docs/training_data_pipeline.md)。
- CSA 接続クライアントは別 crate です。[csa_client の手順](../rshogi-csa-client/examples/README.md)を参照してください。
