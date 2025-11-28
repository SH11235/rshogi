# engine-core アーキテクチャ概要

このドキュメントは `engine-core` crate 内の恒久的な設計情報をまとめたものです。  
実装詳細ではなく、「どのモジュール・型がどんな責務を持つか」を Coding Agent 向けに整理します。

## モジュール構成

- `types`  
  将棋エンジンで使用する基本型（`Color`, `Square`, `Move`, `Value` など）。  
  ここで定義された型を前提に、以降のモジュールが組み立てられます。

- `bitboard`  
  81 マスの盤面を 128bit（`Bitboard`）で表現し、筋・段・升ごとのマスクと各種利きテーブルを提供します。  
  近接駒（歩・桂・銀・金・玉）はテーブル参照、遠方駒（香・角・飛・馬・龍）は `between_bb` / `line_bb` などを用いたスライディング計算で扱います。

- `position`  
  局面 (`Position`) と状態 (`StateInfo`) を管理し、`do_move` / `undo_move` / `do_null_move` によるインクリメンタル更新を行います。  
  盤面配列・Bitboard・手駒・手番・手数・玉位置に加え、Zobrist ハッシュ (`ZOBRIST`) や王手情報・pin 情報・直前の手などを `StateInfo` に保持します。

- `movegen`  
  与えられた局面から pseudo-legal 手と合法手を生成します。  
  `generate_non_evasions` / `generate_evasions` / `generate_all` が王手の有無に応じた手生成を行い、`generate_legal` が `Position::is_legal` でフィルタして完全合法手を `MoveList` に格納します。

- `nnue`（未実装）  
  NNUE 評価関数の読み込み・特徴量計算・差分更新を担当します。  
  SIMD やレイアウトの詳細はこのモジュールに閉じ込め、`Value` を返す API を中心に据えます。

- `tt`（未実装）  
  置換表（Transposition Table）の実装。エントリのレイアウト、世代管理、prefetch など探索の高速化に関わる部分を担います。

- `search`（未実装）  
  Alpha-Beta 検索と各種枝刈り（NMP, LMR, Futility など）を実装します。  
  `position`, `movegen`, `nnue`, `tt` に依存し、`Value` と `Move` を入出力の中心とします。

- `movepick`（未実装）  
  探索効率を上げるための手の順序付け。History 統計や MVV-LVA などをここにまとめます。

- `time`（未実装）  
  持ち時間制御や秒読みなど、探索時間の管理ロジックを担当します。

## 型システムの概要

詳細な定義は `src/types/*.rs` と `src/types/mod.rs` を参照してください。  
ここでは依存関係と主な役割だけを整理します。

```text
Color
  ↓
File, Rank
  ↓
Square
  ↓
PieceType
  ↓
Piece ← Move
  ↓
Hand

Value, Depth, Bound, RepetitionState は独立
```

- `Color`  
  先手・後手を表す列挙型。`index()` により配列インデックスとして利用します。

- `File`, `Rank`  
  盤上の筋・段を表す列挙型。USI 文字との相互変換や、0 ベースのインデックス化を提供します。

- `Square`  
  升目（0〜80）を表すラッパー型。  
  `File`×`Rank` からの生成、USI 文字列との相互変換、左右反転・180 度回転などの座標変換を担います。

- `PieceType`  
  駒種（先後の区別なし）。成り・生駒の対応や、遠方駒判定（香・角・飛・馬・龍）などを提供します。

- `Piece`  
  先後を含む駒。内部では `bit 0-3: PieceType`, `bit 4: Color` というレイアウトで表現します。  
  `Piece::NONE` だけが「駒なし」を表し、それ以外は常に有効な `PieceType`/`Color` を持つ前提です。

- `Move`  
  16bit の指し手表現。`Square` と `PieceType` を組み合わせて、通常手・駒打ち・成りを区別します。  
  歴史統計などに使うための `history_index()` もここで提供します。

- `Hand`  
  手駒枚数を 32bit にパックした型。  
  各駒種ごとの枚数取得・加算・減算・比較（優等局面判定）を行います。

- `Value`  
  評価値（評価関数・探索の入出力）。詰みスコア域（`MATE` 付近）と通常スコア域を明示的に区別します。

- `Depth`  
  探索深さを表す別名型。`MAX_PLY` や静止探索用の深さなどの共通定数を定義します。

- `Bound`  
  置換表に保存する評価値の種類（上界・下界・完全値）を表します。TT 参照時のカットオフ判定に利用します。

- `RepetitionState`  
  千日手や優等/劣等局面の状態を表す列挙型。探索側の終局判定ロジックで利用します。

## 設計の方針

- YaneuraOu の型設計に準拠しつつ、Rust の型システムで安全性と可読性を高める。
- ビットレベルのレイアウト（`Piece`, `Hand`, `Move` など）は、型定義ファイルの先頭コメントに必ず明記する。
- 将来的に NNUE や探索ロジックを差し替えやすいよう、`types` を境界としてモジュール間の責務を分離する。
