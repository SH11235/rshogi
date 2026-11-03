# rshogi

Rust で書かれた将棋エンジンと、その開発に使うツール群です。

[![Crates.io](https://img.shields.io/crates/v/rshogi-core.svg)](https://crates.io/crates/rshogi-core)
[![Documentation](https://docs.rs/rshogi-core/badge.svg)](https://docs.rs/rshogi-core)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPL--3.0--or--later-blue.svg)](LICENSE)

## どこから読むか

| やりたいこと | 読む場所 |
|---|---|
| 将棋 GUI でエンジンを使って対局・検討する | [USI エンジンを使う](#usi-エンジンを使う) |
| 教師データの生成・加工、対局による棋力の比較、SPSA、定跡の整備をする | [ツールを使う](#ツールを使う) |
| エンジンを floodgate などの CSA サーバーで対局させる | [CSA クライアントを使う](#csa-クライアントを使う) |
| Rust のアプリや Web アプリにエンジンを組み込む | [ライブラリとして使う](#ライブラリとして使う) |

## 特徴

### 教師データ・棋力評価・SPSA のツール

- 教師データの生成 (`gensfen`) と、評価値の付け直し (`rescore_psv`) ができます。付け直しには NNUE のほか、
  外部の USI エンジンや dlshogi 系の ONNX モデルを使えます。重複の除去やシャッフルなどの加工のツールもあり、
  億単位の局面を扱う前提で作っています。
- エンジン同士の対局と SPRT (逐次検定で強さの差を判定する方法) による棋力の比較 (`tournament`)、
  探索パラメータと NNUE の重みの SPSA (対局の結果からパラメータを調整する方法) (`spsa`)、
  定跡の作成と点検 (`book_mine`) ができます。
- NNUE の学習には別のリポジトリの学習器 [tatara](https://github.com/SH11235/tatara) を使います。
  tatara で学習して書き出したモデルを `EvalFile` に指定して使えます。

### 評価関数のモデルを 1 つのバイナリで読み分ける

- 入力の特徴量が HalfKP・HalfKA・HalfKA_hm の NNUE に対応しています。出力層を局面の進行度などで切り替える
  LayerStacks の構造と、PSQT・Threat・EffectBucket の拡張も読めます。
- 既定の `edition-universal` はモデルのヘッダーから構成と寸法を読み取るので、構成や寸法の違うモデルを
  同じバイナリで読み込んで比べられます。大会や SPRT のように同じモデルを長く使うときは、
  モデルの構成に固定したバイナリを作れます ([edition ごとの build](#edition-ごとの-build))。
- 対応範囲は [サポートされている NNUE アーキテクチャ](docs/nnue-supported-architectures.md) にまとめています。

### やねうら王 (YaneuraOu) 形式の定跡に対応

- やねうら王形式の定跡ファイル (`.db`) とバイナリ定跡 (`.ybb`) をそのまま使えます。
- `BookOnTheFly=true` にすると定跡をメモリに読み込まず、局面ごとにファイルから引きます。
  メモリに収まらない大きさの定跡も使えます。テキストの `.db` は局面 (SFEN) の文字列順に並べておく必要があります。
- 定跡ファイルを重ねて使えます。手を直したい局面だけを番号付きの小さいファイルに書いておくと、
  元の大きい定跡より先にそちらを引きます。
- 定跡の手を使う条件 (最善の手からの評価値の差、読みの深さの下限) を、先手番と後手番で別に設定できます。
  詳細は [rshogi-book の README](crates/rshogi-book/README.md) を参照してください。

### 詰将棋エンジン

`go mate` で局面に詰みがあるかを df-pn (詰み専用の探索) で調べ、詰みがあれば手順を返します。
目的に合わせて USI オプションで進め方を選びます。

| やりたいこと | 設定 |
|---|---|
| 詰むかどうかを速く知る (手順は最短とは限らない) | 既定 (`MateShortest=Off`) |
| 詰めろの判定のように、詰まない局面を多く調べる | `MateMode=Speed` |
| 詰将棋の正解手順を、最短であることを確かめて求める (無駄合いを数えず、同じ手数なら駒余りを避ける) | `MateShortest=Tsume` |
| 詰将棋の正解手順を速く推定する (最短であることは確かめない) | `MateShortest=Estimate` |
| 合駒もすべて応手として数えた最短手順を求める | `MateShortest=All` |

`MateMode` (探索の進め方) と `MateShortest` (返す手順の求め方) は組み合わせて指定できます。
`MateMode` の既定の `Power` は長手数や難しい詰みを解く力を優先します。最短手順を求める設定は、
詰むかどうかだけを調べるより時間がかかります。
設定の詳細は [rshogi-usi の README](crates/rshogi-usi/README.md#詰み探索-go-mate) を参照してください。

### Web アプリへの組み込み

- エンジンのコアを `rshogi-core` として crates.io で公開しています。WebAssembly 向けにも build でき、
  `wasm-threads` で並列探索に対応します。[ラム将棋](https://ramu-shogi.sh11235.com/) はこの構成で
  ブラウザ上で探索しています。
- 止めた解析を、完了した反復から再開できる API (`AnalysisSession`) があります。

### CSA の対局サーバーとクライアント

- USI エンジンを floodgate などの CSA サーバーにつなぐ `csa_client` で、GUI を介さずに連続対局できます。
- CSA プロトコルの対局サーバーを Cloudflare Workers 上で公開しています (`wss://rshogi-csa-server.sh11235.com`)。
  同じルーム ID を指定して対局したり、ロビーで相手を待ったりできます。自分のエンジンを別々のマシンから
  対局させる検証にも、ほかの人のエンジンとの対局にも使えます。
- 同じ対局サーバーを手元の TCP サーバーとしても動かせます ([CSA サーバー](docs/csa-server/README.md))。

### 独自ルール (オプション)

- **パス権** - 1 手パスできるルール。ハンデ戦や遊び要素として使えます
  (探索でパス権を評価するには `search-pass-rules` feature を付けて build します。既定は無効)。

## ビルドと使い方

配布用のバイナリは用意していないので、Rust の環境で build します。必要な環境は
[ビルドガイド](docs/build.md#前提環境) を参照してください。

この workspace には USI エンジンのほか、70 本を超えるバイナリを持つ tools や CSA サーバー・クライアントが入っています。
`-p` を付けない `cargo build --release` はこれらの crate を全部 build するので、時間とディスクを多く使います。
使うものだけを `-p` と `--bin` で選んで build してください。Windows では実行ファイルに `.exe` が付きます。

`.cargo/config.toml` で `target-cpu=native` を指定しているので、build したマシンの SIMD (AVX2 等) が有効になります。
そのため、できたバイナリは命令セットの異なる別の CPU では動かないことがあります。別のマシンで使う場合は、
そのマシンで build してください。

### USI エンジンを使う

```bash
# 対局用 (LTO を最大にした production profile)。target/production/rshogi-usi ができる
cargo build --profile production -p rshogi-usi

# 動作確認用 (build が速い)。target/release/rshogi-usi ができる
cargo build --release -p rshogi-usi
```

できた `rshogi-usi` を、将棋所や ShogiGUI などの USI 対応の GUI にエンジンとして登録します。
評価関数のファイルは同梱していないので、NNUE のモデルを用意して `EvalFile` に指定してください
(指定しなければエンジンの作業ディレクトリの `eval/nn.bin` を読みます)。どちらの build も既定の
`edition-universal` で、対応する NNUE を実行時に読み分けます。LayerStacks のモデルには振り分け方式などの
設定も要ります ([サポートされている NNUE アーキテクチャ](docs/nnue-supported-architectures.md))。
オプションの一覧は [rshogi-usi の README](crates/rshogi-usi/README.md#usi-options) にあります。

### ツールを使う

教師データの生成・加工、対局による棋力の比較、SPSA、定跡の整備などのツールは `tools` クレートにあります。
使い方は [tools の README](crates/tools/README.md) と [目的別の全ツール一覧](crates/tools/docs/tools-reference.md) にあります。

```bash
# 使うツールだけを --bin で選ぶ。target/release/rescore_psv ができる
cargo build --release -p tools --bin rescore_psv
```

`--bin` を付けない `cargo build --release -p tools` は tools のバイナリを全部 build します。

### CSA クライアントを使う

```bash
# target/release/csa_client ができる
cargo build --release -p rshogi-csa-client
```

floodgate への接続と、公開している対局サーバーへの参加 (`--target production` に時計の設定名 `--game-name` と、
ルーム ID またはロビーを指定します) の手順は [CSA クライアント](docs/csa-client.md#公開インスタンスに参加する) にあります。

### ライブラリとして使う

`rshogi-core` を `Cargo.toml` に追加します。

```toml
[dependencies]
rshogi-core = "0.9"
```

最小の探索例と NNUE の読み込みは [rshogi-core の README](crates/rshogi-core/README.md)、API は
[docs.rs](https://docs.rs/rshogi-core) にあります。WebAssembly 向けの build は
[ビルドガイド](docs/build.md#xtask-の-webassembly-target)、止めた解析の再開は
[局面解析セッション](docs/analysis-session.md) を参照してください。

### workspace 全体 (開発用)

```bash
# 全 crate の build (USI エンジンは対応する NNUE を実行時に読み分ける)
cargo build --release

# テスト実行
cargo test
```

### edition ごとの build

NNUE の構成ごとのバイナリを `engines/` に並べて管理したい場合 (selfplay / SPRT / tournament など) は、
`cargo xtask` で preset edition ごとに別のバイナリを build します。

```bash
# 利用可能な preset edition を列挙
cargo xtask list-editions

# 特定 preset を build (engines/rshogi-usi-<edition> に配置 + .meta.toml 記録)
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-psqt

# 複数 preset を一気に build
cargo xtask build --edition X,Y
cargo xtask build --all-presets

# edition とは別に追加の feature を指定する (engines/rshogi-usi-<edition>+mimalloc に配置)
cargo xtask build --edition X --features mimalloc

# engines/ 配下の binary 一覧 (preset / commit / age / status を表示)
cargo xtask list-binaries
```

詳細は [`docs/build.md`](docs/build.md) を参照してください。
保守者向けの crates.io publish・engine tag・GitHub Release の手順は [RELEASING.md](RELEASING.md) にあります。

## クレート構成

```
crates/
├── rshogi-core/                # エンジンのコアライブラリ (crates.io で公開)
├── rshogi-usi/                 # USI エンジンの実行バイナリ
├── rshogi-book/                # 定跡ファイルの読み込みと probe
├── rshogi-csa/                 # CSA 形式の棋譜の解析と USI への変換
├── rshogi-csa-client/          # USI エンジンを CSA サーバーへつなぐクライアント
├── rshogi-csa-server/          # CSA 対局サーバーの対局処理 (通信方式に依存しない部分)
├── rshogi-csa-server-tcp/      # CSA サーバーの TCP 版
├── rshogi-csa-server-workers/  # CSA サーバーの Cloudflare Workers 版
├── tools/                      # 対局評価・教師データ・定跡・計測のツール群
└── xtask/                      # edition ごとの build と engines/ への配置
```

## このエンジンを使用したアプリ

- [ラム将棋](https://ramu-shogi.sh11235.com/) - Web の将棋アプリです。手元の NNUE 評価関数を読み込んで、
  対局や局面の検討に使えます。ほかに、CSA サーバーの対局の観戦や棋譜の閲覧などの機能があります。

## 参考・影響

本プロジェクトは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。

## ライセンス

GPL-3.0-or-later License - 詳細は [LICENSE](LICENSE) を参照してください。
