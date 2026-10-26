# rshogi-core

Rust 製の将棋エンジンライブラリです。局面表現、ビットボードによる合法手生成、
NNUE 評価、Null Move・Futility・LMR などの枝刈りを備えた Alpha-Beta 探索、
ロックフリーの置換表、時間管理、Lazy SMP による並列探索を提供します。
NNUE は HalfKP / HalfKA 系と LayerStacks に対応し、既定の `edition-universal` は
モデルに応じて構成を選びます。

## 依存関係の追加

```toml
[dependencies]
rshogi-core = "0.8"
```

## 最小の探索例

探索前に NNUE を読み込むか、Material 評価を明示的に有効にしてください。
次の例は追加のモデルファイルを使わず、Material 評価で平手初期局面を深さ4まで探索します。
両方が未設定のまま評価を行うと panic します。

```rust
use rshogi_core::eval::material::{self, MaterialLevel};
use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::search::{LimitsType, Search, SearchInfo};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    material::set_material_level(MaterialLevel::Lv2);
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE)?;

    let mut search = Search::new_with_eval_hash(16, 1);
    let mut limits = LimitsType::new();
    limits.depth = 4;
    let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
    println!("bestmove {}", result.best_move.to_usi());
    Ok(())
}
```

`Search::new_with_eval_hash` の引数は置換表と評価キャッシュのサイズ（MB）です。
`limits.nodes` では全探索スレッドの合計ノード数、`limits.movetime` では固定の思考時間
（ミリ秒）を指定できます。`search.set_num_threads(4)` で探索スレッド数を変更できます。

## NNUE の利用

モデルはリポジトリ外の共有データ領域 `$SHOGI_DATA/nnue/` に用意します。
NNUE を使うときは、上の例の Material 設定に代えて、探索開始前に次の初期化を行います。

```no_run
use rshogi_core::{eval::material, nnue};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = std::env::var_os("SHOGI_DATA").ok_or("SHOGI_DATA が未設定です")?;
    nnue::init_nnue(PathBuf::from(data).join("nnue/model.nnue"))?;
    material::disable_material();
    Ok(())
}
```

`init_nnue` はプロセス内で共有するモデルを設定します。Material 評価を有効にしていた場合は
`disable_material()` で無効にします。モデルと評価設定は探索開始前に設定してください。
LayerStacks では、モデルに合うバケット振り分け方式と、必要に応じて進行度係数も設定します。
対応形式・寸法・routing は [NNUE 対応表](https://github.com/SH11235/rshogi/blob/main/docs/nnue-supported-architectures.md)、
固定 edition の選び方は [ビルドガイド](https://github.com/SH11235/rshogi/blob/main/docs/build.md) を参照してください。

### 読み込みエラー

`NNUENetwork::read` は `Read + Seek`、LayerStacks FT の `read_leb128` は `Read` のみで利用できます。
LEB128 ブロックは実際に読めたバイト数に応じてバッファを確保し、重みの確保前に要素数を検証します。
既定の universal edition では、非圧縮の動的な重み配列も必要バイト数と入力の残長を確保前に照合します。
これらの入力が途中で切れていたり要素数が合わなかったりする場合は `io::Error` を返します。
これらの重みと圧縮バッファの確保失敗は、取得可能な場合 `ErrorKind::OutOfMemory` で返します。
圧縮ブロックの上限は 2 GiB です。

## 読み取り専用の局面

`position::StaticPosition` は SFEN または盤面・手駒・手番から、駒在庫を検査した読み取り専用の局面を作ります。
key、正規の SFEN、盤上の駒、手駒、手番、玉の位置、王手の有無を参照できます。
利きの全升集計、NNUE の駒表、駒割を初期化しないため、大量の定跡の索引や表記の検査に使えます。

```rust
use rshogi_core::position::{StaticPosition, SFEN_HIRATE};

fn main() -> Result<(), rshogi_core::position::SfenError> {
    let diagram = StaticPosition::from_sfen(SFEN_HIRATE)?;
    let key = diagram.key();
    let position = diagram.into_position()?;
    assert_eq!(position.key(), key);
    Ok(())
}
```

`StaticPosition` は指し手の実行・評価の API や `Position` への参照を公開しません。
`into_position()` は利き・NNUE の駒表・駒割などを完全に初期化してから通常の `Position` を返します。
`Position::set_sfen` / `set_from_parts` も完全な局面を構築します。

## LayerStacks の progress 係数

`nnue::get_layer_stack_progress_kpabs_weights()` は所有権付きの `ProgressKpAbsWeights` を返します。
`&weights` を `&[f32]` として渡せます。係数を再設定・reset しても取得済みのスナップショットは有効で、
最後の所有者が破棄されるまで保持されます。

探索スタックへの反映は生成・reset 時に行い、探索中はスタックが保持する係数を参照します。
固定 edition と実行時寸法版の `NNUEEvaluator` も同じ f32 係数を reset まで保持します。
`refresh()` はアキュムレータの再計算のみで、係数のスナップショットは更新しません。

スタックを受け取らない低レベル API `NetworkLayerStacks::evaluate()` と `evaluate_with_diagnostics()` は、
評価ごとにグローバル設定のスナップショットを取得します。係数の寿命を評価器に固定する場合は
`NNUEEvaluator` を使ってください。

## ライセンス・参考

GPL-3.0-or-later License。
本クレートは将棋エンジン [YaneuraOu](https://github.com/yaneurao/YaneuraOu) および
チェスエンジン [Stockfish](https://github.com/official-stockfish/Stockfish) を参考にしています。
