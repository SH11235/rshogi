# rshogi

Rust で書かれた将棋エンジンです。

[![Crates.io](https://img.shields.io/crates/v/rshogi-core.svg)](https://crates.io/crates/rshogi-core)
[![Documentation](https://docs.rs/rshogi-core/badge.svg)](https://docs.rs/rshogi-core)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPL--3.0--or--later-blue.svg)](LICENSE)

## 特徴

### 評価関数のモデルを 1 つのバイナリで読み分ける

- HalfKP・HalfKA・HalfKA_hm の NNUE と、LayerStacks (局面の進行度などで出力層の重みを切り替える構造) の
  両方に対応しています。PSQT、Threat、EffectBucket の拡張も読めます。
- 既定の `edition-universal` はモデルのヘッダーから構成と寸法を読み取るので、形の違うモデルを同じバイナリで
  読み込んで比べられます。大会や SPRT のように同じモデルを長く使うときは、`cargo xtask build --edition <preset>`
  でモデルの構成に固定したバイナリを作り、`engines/` に構成名を付けて並べて管理します。
- 対応範囲は [サポートされている NNUE アーキテクチャ](docs/nnue-supported-architectures.md) にまとめています。

### 詰将棋のルールで正解手順を求める詰み探索

- `go mate` に df-pn で応答し、置換表を共有した複数スレッドで解けます (`MateThreads`)。
- `MateShortest=Tsume` で、無駄合いを数えず、同じ手数なら駒余りを避ける詰将棋のルールの最短手順を返します。
  攻め方の玉が無い局面も解けます。詳細は [rshogi-usi の README](crates/rshogi-usi/README.md#詰み探索-go-mate)。

### 大きい定跡をファイルから直接引く

- YaneuraOu 形式の定跡 (`.db`) とバイナリ定跡 (`.ybb`) を読みます。`BookOnTheFly=true` なら丸読みせず、
  probe のたびにファイルを二分探索して引きます。
- 番号付きの優先定跡と、評価値の差・depth の下限の先後別の指定に対応しています。
  詳細は [rshogi-book の README](crates/rshogi-book/README.md)。

### ブラウザでも並列探索する

- `wasm32-unknown-unknown` と `wasm32-wasip1` 向けに build でき、`wasm-threads` を付けると Web Worker で
  並列に探索します。[Ramu Shogi](https://ramu-shogi.sh11235.com/) はこの構成でブラウザ上で探索しています。
- `AnalysisSession` で、止めた解析を完了した反復から再開できます ([局面解析セッション](docs/analysis-session.md))。

### 対局の場と開発用のツールを同じリポジトリに置く

- CSA プロトコルの対局サーバー (TCP 版と Cloudflare Workers 版) と、USI エンジンを floodgate などの
  CSA サーバーにつなぐ `csa_client` があります ([CSA サーバー](docs/csa-server/README.md) /
  [CSA クライアント](docs/csa-client.md))。
- 開発用のツールとして、エンジン同士の対局と SPRT (`tournament`)、探索パラメータと NNUE の重みの SPSA (`spsa`)、
  教師データの生成と付け直し (`gensfen`・`rescore_psv`)、定跡の作成と点検 (`book_mine`) があります。
  教師の付け直しには、NNUE のほか外部の USI エンジンや dlshogi 系の ONNX モデルを使えます。
  一覧は [tools の README](crates/tools/README.md)。
- NNUE の学習には学習器 [tatara](https://github.com/SH11235/tatara) を使っています。tatara が書き出す
  推論用の形式のモデルを `EvalFile` で読み込めます。

### オプショナルな独自ルール

- **パス権** - 1 手パスできるルール。ハンデ戦や遊び要素として使えます
  (探索でパス権を評価するには `search-pass-rules` feature を付けて build します。既定は無効)。

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

## インストール

`rshogi-core` をライブラリとして使う場合は `Cargo.toml` に追加します。

```toml
[dependencies]
rshogi-core = "0.9"
```

## ビルド

`.cargo/config.toml` で `target-cpu=native` を採用しているため、`cargo build` だけで
build マシンの SIMD (AVX2 等) は自動で有効になる。

```bash
# 開発用 build (全 NNUE arch を runtime dispatch)
cargo build --release

# テスト実行
cargo test
```

複数 NNUE architecture binary を `engines/` 配下で並行管理したい場合 (selfplay / SPRT /
tournament 等) は `cargo xtask` 経由で preset edition ごとに別 binary を build できる:

```bash
# 利用可能な preset edition を列挙
cargo xtask list-editions

# 特定 preset を build (engines/rshogi-usi-<edition> に配置 + .meta.toml 記録)
cargo xtask build --edition layerstacks-halfka_hm_merged-1536x16x32-psqt

# 複数 preset を一気に build
cargo xtask build --edition X,Y
cargo xtask build --all-presets

# Edition 軸と直交する opt-in feature を追加 (engines/rshogi-usi-<edition>+mimalloc に配置)
cargo xtask build --edition X --features mimalloc

# engines/ 配下の binary 一覧 (preset / commit / age / status を表示)
cargo xtask list-binaries
```

詳細は [`docs/build.md`](docs/build.md) を参照。
保守者向けの crates.io publish・engine tag・GitHub Release の手順は [RELEASING.md](RELEASING.md) を参照。

## このエンジンを使用したアプリ

- [Ramu Shogi](https://ramu-shogi.sh11235.com/) - Web 将棋アプリ

## 参考・影響

本プロジェクトは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) およびチェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。

## ライセンス

GPL-3.0-or-later License - 詳細は [LICENSE](LICENSE) を参照してください。
