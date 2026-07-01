# kifu_player — PSV / tournament JSONL 棋譜プレイヤー TUI

PSV (PackedSfenValue) ファイルと `tournament` の出力 JSONL を、同じ TUI で
1手ずつ再生・閲覧するツール。`tournament` の out-dir は数千局規模になりうるため、
横断した対局リストをインクリメンタルにフィルタして特定の対局へすぐ辿り着けるように
してある。

読み取り専用のビューアであり、PSV のシャッフル・分割・統合や JSONL の編集は行わない。

## ビルド

`ratatui`/`crossterm` への依存を持つため、default feature には含まれていない
（学習パイプライン専用の checkout・ビルドに対話的 TUI 依存を持ち込まないため）。

```bash
cargo build -p tools --release --features kifu-player --bin kifu_player
```

## 使い方

### PSV を開く

```bash
cargo run -p tools --release --features kifu-player --bin kifu_player -- \
  --psv runs/gensfen/20260701_120000/gensfen.psv
```

`--psv` には**連続した自己対局ストリーム**（`gensfen` の直接出力等）を渡す。
`shuffle_psv`/`merge_psv` 等でレコード順序がシャッフルされたプールを渡すと、
対局境界検出（`game_ply` のリセット検出）が機能せず、ほぼ全レコードが
1レコードだけの対局として誤検出される。索引構築後に平均対局長が極端に短い
場合は起動時に警告が出る。

### tournament の out-dir を開く

```bash
cargo run -p tools --release --features kifu-player --bin kifu_player -- \
  --tournament-dir runs/selfplay/20260701_120000-v1-vs-v2
```

out-dir 配下の `*-vs-*.jsonl`（`tournament` が出力するペアファイル）を横断して
1つの対局リストにまとめる。`control_history.jsonl` 等、対局データを含まない
付随ファイルは自動的に除外される。

## 画面構成

- 左: 対局一覧（`/` で検索・フィルタ。対局ラベル・`black_win`/`white_win`/`draw`/
  `error`・`game_id`・`pair_index`/`pair_slot`/`startpos_idx` で絞り込める）
- 中央: 盤面（先手の駒は黄、後手の駒は シアン で色分け）
- 右: 指し手一覧（棋譜風ラベル。現在の手をハイライト）
- 評価値グラフ: 先手から見た評価値（**プラス=先手優勢、マイナス=後手優勢**に
  固定した POV）を、着手側の色（先手=黄、後手=シアン）で線分を塗り分けて表示する。
  tournament 対局で先後に別エンジンが付いている場合、評価値はエンジンごとに
  異なるスケールでありうる点に注意（グラフのギザギザを単純な形勢の振れと
  読みすぎない）。PSV はレコードに残っている `game_ply` をそのまま X 軸に使うため、
  `skip_initial_ply`/`skip_in_check` による欠番がある場合はそのまま見える。
  評価値が無い手（JSONL でエンジンが返さなかった場合）は打点しない。
- 下部: 現在手の注釈（深さ・ノード数・NPS・経過時間・エンジン名等、ある分だけ表示）

## キーバインド

| キー | 動作 |
|------|------|
| `h` / `←` | 1手戻す |
| `l` / `→` | 1手進める |
| `j` / `↓` | 次の対局（フィルタ後のリスト内） |
| `k` / `↑` | 前の対局 |
| `/` | 検索・フィルタ入力開始（`Enter`/`Esc` で終了、`Esc` はフィルタもクリア） |
| `q` / `Esc` | 終了 |

## スコープ外

- 実行中の tournament の live-tail（オフライン解析専用）
- セッションをまたいだ「最後に見ていた対局」の記憶
- PSV の pack（書き込み）、KIF/CSA へのエクスポート（`jsonl_to_kif` 等の既存ツールで代替）
