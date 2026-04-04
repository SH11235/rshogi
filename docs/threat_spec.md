# Threat 仕様固定メモ

bullet-shogi と rshogi で共有する定数・index 構造の確定版。
実装前にこのメモの内容を両リポジトリで完全一致させること。

## ThreatClass 順序

| class_id | name | 含まれる PieceType |
|----------|------|--------------------|
| 0 | Pawn | Pawn |
| 1 | Lance | Lance |
| 2 | Knight | Knight |
| 3 | Silver | Silver |
| 4 | GoldLike | Gold, ProPawn, ProLance, ProKnight, ProSilver |
| 5 | Bishop | Bishop |
| 6 | Rook | Rook |
| 7 | Horse | Horse |
| 8 | Dragon | Dragon |

**除外**: attacker = King, attacked = King

## attacks_per_color

各 class の駒が空盤面上の全 81 マスに置かれた場合の攻撃先マス数の合計。
先手基準で計算（後手は視点変換で対称）。

| class_id | class | attacks_per_color |
|----------|-------|------------------:|
| 0 | Pawn | 72 |
| 1 | Lance | 324 |
| 2 | Knight | 112 |
| 3 | Silver | 328 |
| 4 | GoldLike | 416 |
| 5 | Bishop | 816 |
| 6 | Rook | 1296 |
| 7 | Horse | 1104 |
| 8 | Dragon | 1552 |
| **合計** | | **6020** |

## THREAT_DIMENSIONS

```
2(attacker_side) × 9(attacker_class) × 2(attacked_side) × 9(attacked_class) × attacks_per_color[ac]
= 36 × 6020
= 216,720
```

## index 構造

```
oriented_color = attacker_color ^ perspective   // perspective swap
attack_pattern = oriented_attack_pattern(attacker_class, oriented_color)

threat_index =
    pair_base[attacker_side][attacker_class][attacked_side][attacked_class]
  + from_offset[attack_pattern][from_sq_n]
  + attack_order[attack_pattern][from_sq_n][to_sq_n]
```

`attack_pattern` は方向性駒では色別、非方向性駒では色不問:
- 方向性駒 (Pawn, Lance, Knight, Silver, GoldLike): oriented_color で LUT を切り替え
- 非方向性駒 (Bishop, Rook, Horse, Dragon): oriented_color に関係なく同一 LUT

### 用語

- `attacker_side`: 0 = perspective side (friend), 1 = opposite side (enemy)
  - **注意**: 「現在手番基準の stm/nstm」ではなく、accumulator の perspective から見た friend/enemy。
    差分更新は perspective ごとに行うため、手番反転で side ラベルが全反転しない。
- `attacked_side`: 0 = perspective side (friend), 1 = opposite side (enemy)
- `from_sq_n`: 正規化後の attacker のマス (視点変換 + Half-Mirror)
- `to_sq_n`: 正規化後の attacked のマス (同上)
- `attack_order`: 空盤面上で from_sq_n の駒が攻撃できるマスを、
  **Square の raw 値が小さい順に 0-indexed で番号付け**

### Square raw 値の座標対応

```
raw = file * 9 + rank
file: 0=9筋, 1=8筋, ..., 8=1筋
rank: 0=1段, 1=2段, ..., 8=9段

例:
  raw  0 = 9一 (file=0, rank=0)
  raw 40 = 5五 (file=4, rank=4)
  raw 80 = 1九 (file=8, rank=8)
```

rshogi の `Square::raw()` と bullet-shogi の `ShogiBoard` 座標系で一致すること。

### 正規化（Stockfish FullThreats 準拠）

**HalfKA_hm と同じ perspective 基準**で正規化する。
Stockfish の `FullThreats::make_index` と同じ設計方針を採用。

これにより:
- 学習済みモデルの互換性を他エンジン（YaneuraOu 等）と確保できる
- nnue-pytorch の Threat 実装を参考にできる
- 他のエンジン開発者が既知の Stockfish 設計を前提にできる

```
1. Perspective 基準で正規化:
   sq_n = if perspective == White { sq.inverse() } else { sq }

2. Half-Mirror（perspective の王の筋で判定）:
   hm = is_hm_mirror(king_sq, perspective)
   sq_final = if hm { sq_n.mirror() } else { sq_n }
```

**方向性駒の扱い（Stockfish 準拠: 色別 attack LUT）**:

Pawn, Lance, Knight, Silver, GoldLike は攻撃方向が駒の色に依存する。
perspective 基準の正規化では、後手の駒はマスが inverse されるが攻撃方向は
先手基準にならない。

Stockfish はこの問題を **色別の attack LUT** で解決している:
- `attack_order[W_PAWN][from][to]` と `attack_order[B_PAWN][from][to]` を別に持つ
- perspective で orient した後のマスに対して、orient 後の駒色に対応する LUT を引く

将棋では方向性駒が 5 種（Pawn, Lance, Knight, Silver, GoldLike）あるため:
- 非方向性駒: Bishop, Rook, Horse, Dragon — 色不問で同一 LUT (4 エントリ)
- 方向性駒: Pawn, Lance, Knight, Silver, GoldLike — 色別 LUT (5 × 2 = 10 エントリ)
- 合計: 14 attack pattern エントリ（LUT サイズ増加は数 KiB で無視できる）

**THREAT_DIMENSIONS は変わらない**（index 空間は同じ、LUT の引き方だけが変わる）。

**重要**:
- from_sq と to_sq の両方に同じ perspective 変換を適用する（相対位置が保存される）
- 駒色は perspective で swap する: `oriented_color = attacker_color ^ perspective`
- orient 後のマスと oriented_color を使って色別 attack LUT を引く
- HalfKA_hm と Threat で正規化基準が統一される

### pair_base テーブル

展開順序: `attacker_side → attacker_class → attacked_side → attacked_class`

各 pair の要素数 = `attacks_per_color[attacker_class]`

flat index: `as * 162 + ac * 18 + ds * 9 + dc`
(162 = 9 * 18, 18 = 2 * 9)

pair_base[i] = 前の pair までの累積和。

### from_offset テーブル

`from_offset[attacker_class][sq]` = sq=0 から sq-1 までの各マスの攻撃数の累積和。

空盤面上で、先手基準の攻撃利きを数える。
方向は PieceType ごとの standard effect で、盤端で制限される。

### attack_order テーブル

`attack_order[attacker_class][from_sq][to_sq]` = 
空盤面上で from_sq の attacker_class 駒が to_sq を攻撃するときの、
その pair 内でのローカル index。

**順序**: from_sq の駒が攻撃できる全マスを Square raw 値の昇順で列挙し、
0-indexed で番号付け。to_sq がその列挙の何番目かが attack_order の値。

例: Rook at sq=40 (5五) は空盤面上で上下左右に 16 マスを攻撃する。
これを Square raw 値の昇順でソートした順に 0, 1, 2, ... と番号付ける。

**スライダー駒（Lance, Bishop, Rook, Horse, Dragon）の注意**:
`from_offset` と `attack_order` の **index 生成は空盤面**上の利きを使う。
実盤面の occupied は無視。これにより静的テーブルとして事前計算可能。

**ただし、実際に active にする threat pair の列挙は実盤面の occupied を使う。**
空盤面利きは index テーブルの付番方式を決めるためだけに使い、
ある局面で実際に発生している threat（実盤面上で attacker が attacked を攻撃している）
を列挙する際には `attackers_to_occ(sq, occupied)` を使用する。

## アーキテクチャ文字列

```
Threat=216720
```

## ファイルフォーマットのブロック順

```
FT biases (i16, LEB128)
FT weights (i16, LEB128)
[PSQT biases + weights (i32, raw)]    ← PSQT ありの場合のみ
Threat weights (i8, raw)               ← Threat ありの場合のみ
LayerStack per-bucket data
```

Threat weights レイアウト: `i8[THREAT_DIMENSIONS × NNUE_PYTORCH_L1]`
(feature-major, 各特徴の 1536 i8 重みが連続)

## refresh 条件

```
needs_threat_refresh(perspective) =
    is_hm_mirror(prev_king_sq[perspective], perspective)
 != is_hm_mirror(curr_king_sq[perspective], perspective)
```

玉が HM mirror 境界（5筋側 ↔ 6-9筋側）を跨いだときのみ full refresh。
同じ側の中で動く限り差分更新可能。

## 初期容量

| 定数 | 初期値 | 根拠 |
|------|-------:|------|
| MAX_ACTIVE_THREAT_FEATURES | 320 | 盤上 40 駒 × 最大 8 occupied-target の安全側上限 |
| MAX_CHANGED_THREAT_FEATURES | 192 | changed square 周辺の再列挙 + 開き利き分 |

debug build で実測し、十分小さいと確認できてから定数を詰める。
