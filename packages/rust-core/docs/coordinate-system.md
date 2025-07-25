# 将棋エンジンの座標系ガイド

このドキュメントでは、将棋エンジンで使用される座標系と、よくある混乱点について説明します。

## 座標系の基本

### USI表記
- **筋（file）**: 1-9（右から左）
- **段（rank）**: a-i（上から下）
- 例: `7g7f` = 7筋g段から7筋f段へ

### 内部表現（Square）
```rust
Square::new(file, rank)
// file: 0-8 (0=9筋, 8=1筋)
// rank: 0-8 (0=a段, 8=i段)
```

**重要**: 内部表現は0ベースです！

## 座標変換表

| USI表記 | file（筋） | rank（段） | Square::new() |
|---------|-----------|-----------|---------------|
| 1a      | 1         | a         | (0, 0)        |
| 9i      | 9         | i         | (8, 8)        |
| 7g      | 7         | g         | (6, 6)        |
| 7f      | 7         | f         | (6, 5)        |
| 3c      | 3         | c         | (2, 2)        |
| 3d      | 3         | d         | (2, 3)        |

## 座標系変換関数

### 基本的な変換関数

#### Square ↔ USI表記
```rust
// Square → USI文字列
let sq = Square::new(6, 6);  // 7g
println!("{}", sq);           // "7g" (Display trait実装)
let usi_str = sq.to_string(); // "7g"

// USI文字列 → Square
use engine_core::usi::parse_usi_square;
let sq = parse_usi_square("7g").unwrap();  // Square::new(6, 6)
```

#### Move ↔ USI表記
```rust
// USI文字列 → Move
use engine_core::usi::parse_usi_move;
let mv = parse_usi_move("7g7f").unwrap();
let mv = parse_usi_move("B*5e").unwrap();  // 駒打ち
let mv = parse_usi_move("8c8b+").unwrap(); // 成り

// Move → USI文字列
use engine_core::usi::move_to_usi;
let usi_str = move_to_usi(&mv);  // "7g7f"
```

#### Position ↔ SFEN
```rust
// SFEN → Position
use engine_core::usi::parse_sfen;
let pos = parse_sfen("startpos").unwrap();
let pos = parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();

// Position → SFEN
use engine_core::usi::position_to_sfen;
let sfen = position_to_sfen(&pos);
```

### Squareの内部メソッド
```rust
let sq = Square::new(6, 6);  // 7g

// 基本プロパティ
sq.file()  // 6 (0-8)
sq.rank()  // 6 (0-8)
sq.index() // 60 (0-80の一次元インデックス)

// 操作
sq.flip()  // 先後反転した座標を返す
```

### 変換時の注意点

1. **エラーハンドリング**
   ```rust
   // parse関数は Result を返す
   match parse_usi_square("7g") {
       Ok(sq) => { /* 正常処理 */ },
       Err(e) => { /* エラー処理 */ }
   }
   ```

2. **筋段の範囲チェック**
   - file: 0-8（9筋分）
   - rank: 0-8（9段分）
   - 範囲外の値は parse エラーになる

3. **Display実装の活用**
   ```rust
   // Squareは自動的にUSI形式で表示される
   let sq = Square::new(4, 4);  // 内部file=4は5筋
   println!("Square: {}", sq);  // "Square: 5e"
   ```

## 初期局面での重要な位置

### 先手（Black）の駒
- 王将: 5i (4, 8)
- 飛車: 2h (7, 7) ← 内部file=7が2筋
- 角行: 8h (1, 7) ← 内部file=1が8筋
- 歩兵: 1g-9g → 内部では(8,6)-(0,6)

### 後手（White）の駒
- 王将: 5a (4, 0)
- 飛車: 8b (1, 1) ← 内部file=1が8筋
- 角行: 2b (7, 1) ← 内部file=7が2筋
- 歩兵: 1c-9c → 内部では(8,2)-(0,2)

## よくある混乱点

### 1. 先手後手の判別
初期局面（startpos）では：
- `side_to_move = Black`（先手番）
- 先手の歩は**g段**にある（7g, 8g など）
- 後手の歩は**c段**にある（3c, 4c など）

### 2. 手番による合法手の違い
```rust
// 初期局面での合法手の例
// 先手番: 歩を前進させる手（段が減る）
"7g7f"  // 正しい（先手の歩）
"3c3d"  // エラー！（後手の歩を先手番で動かせない）

// 7c7dを指した後（後手番）
"8c8d"  // 正しい（後手の歩）
"5i6h"  // 正しい（後手の王）
```

### 3. Move型とfrom()メソッド
```rust
// Move::from() は Option<Square> を返す
// ドロップ（持ち駒を打つ）の場合は None
let mv = parse_usi_move("7g7f").unwrap();
match mv.from() {
    Some(from_sq) => { /* 通常の移動 */ },
    None => { /* 駒を打つ手 */ }
}
```

## デバッグのヒント

### 手の合法性を確認
```rust
// 任意の局面で合法手を列挙
let mut move_gen = MoveGen::new();
let mut legal_moves = MoveList::new();
move_gen.generate_all(&position, &mut legal_moves);

// USI形式で表示
for i in 0..legal_moves.len() {
    println!("{}: {}", i, move_to_usi(&legal_moves[i]));
}
```

### USI文字列から手を生成（推奨）
```rust
// 手動でSquareを指定するのは避ける
// NG: Move::normal(Square::new(6, 6), Square::new(6, 5), false)

// OK: USIパーサーを使用
let mv = parse_usi_move("7g7f").unwrap();
```

### 合法手との照合
parse_usi_moveで生成した手は、フラグ（成り/不成など）が不完全な場合があります。
完全な手を得るには、合法手リストと照合してください：

```rust
fn parse_and_validate_move(position: &Position, usi_move: &str) -> Result<Move> {
    let mut move_gen = MoveGen::new();
    let mut legal_moves = MoveList::new();
    move_gen.generate_all(position, &mut legal_moves);
    
    // USI文字列で比較
    for i in 0..legal_moves.len() {
        if move_to_usi(&legal_moves[i]) == usi_move {
            return Ok(legal_moves[i]);
        }
    }
    
    Err(anyhow!("Move {} is not legal", usi_move))
}
```

## まとめ

1. **座標は0ベース**: rankは0-8、fileは0-8（ただし左右反転）
2. **fileの左右反転に注意**: 内部file 0 = 9筋、内部file 8 = 1筋
3. **先手後手を確認**: side_to_moveで現在の手番を確認
4. **USI文字列を使う**: 手動でSquareを組み立てるより安全
5. **合法手と照合**: 完全な手の情報を得るため

このガイドラインに従えば、座標系の混乱を避けることができます。

## 開発者向けガイド

### なぜ左右反転しているのか

本実装では、内部のfile座標がUSI表記と左右反転しています。これは歴史的経緯によるもので、現在の実装全体がこの前提で動作しています。

### Square::new()を避けるべき理由

1. **左右の混乱**: USIの"7g"をSquare::new(6, 6)と書いてしまうと"3g"になる
2. **バグの温床**: 手動で座標を指定すると、座標系の理解不足からバグが生まれやすい
3. **コードレビューの難しさ**: 数値だけでは意図が伝わりにくい

### 推奨されるAPI使用パターン

```rust
// ◎ 良い例：USI文字列から変換
let sq = parse_usi_square("7g").unwrap();
let mv = parse_usi_move("7g7f").unwrap();

// × 悪い例：手動で座標を指定
let sq = Square::new(6, 6);  // 意図は7gだが実際は3g
let mv = Move::normal(Square::new(6, 6), Square::new(6, 5), false);
```

### 既存コードを修正する際のチェックリスト

1. Square::new()を使っている箇所を探す
2. USI文字列からの変換に置き換える
3. テストを実行して動作確認
4. コメントで意図を明記する
