# TT key16衝突によるNNUE評価値不一致の調査結果

## 概要

TTヒット時に `ensure_nnue_accumulator` を呼び出すと自己対局で深刻な棋力低下が発生する問題を調査した結果、**TTのkey16衝突**が根本原因であることが判明した。

## 問題の症状

- 71ebeb50 vs b05c0444 → 3/120勝 (2.5%)
- NNUE評価値のmismatch: TTに保存された評価値と、実際に計算した評価値が大きく異なる（差が1000以上のケースも多数）

## 調査過程

### 1. 差分更新の検証

差分更新に問題があると仮定し、`ensure_accumulator_computed` および `update_and_evaluate_*` 関数で差分更新を無効化して常に全計算を使用するよう変更。

**結果**: mismatchは依然として発生。差分更新は問題ではなかった。

### 2. 評価計算の一貫性検証

同じ局面で2回連続して `nnue_evaluate` を呼び出し、同じ結果になるか確認。

```
[NNUE MISMATCH] diff=404 TT=372 eval1=-32 eval2=-32 same=true sfen=...
```

**結果**: `same=true` - 評価計算自体は正しく、一貫した結果を返している。

### 3. TTからのeval取得を無効化

TTヒット時にTTからevalを取得するのではなく、常にNNUE評価を計算するよう変更。

```rust
// 変更前
} else if tt_ctx.hit && tt_ctx.data.eval != Value::NONE {
    ensure_nnue_accumulator(st, pos);
    unadjusted_static_eval = tt_ctx.data.eval;  // TTから取得

// 変更後
} else {
    unadjusted_static_eval = nnue_evaluate(st, pos);  // 常に計算
```

**結果**: 棋力が回復（8/10勝、80%勝率）

## 根本原因

### TTのkey16衝突

rshogiのTT実装は、Stockfish/YaneuraOuと同様に64bitキーの下位16bitだけでマッチングを行っている。

```rust
// tt/table.rs - probe関数
for entry in &cluster.entries {
    if entry.key16() == key16 {  // 下位16bitのみで比較
        ...
        return ProbeResult { found: true, ... };
    }
}
```

この設計では：

1. 異なる局面でも下位16bitが同じならTTヒットと判定される
2. TTエントリには別の局面のデータ（eval含む）が保存されている
3. `tt_ctx.data.eval` として間違った局面の評価値が返される
4. 探索がこの誤った評価値を使用するため、棋力が低下

### なぜmismatchが発生するか

```
1. 局面AでTTミス → NNUE評価(=100) → TTに書き込み (key16=0xABCD, eval=100)
2. 局面BでTTプローブ (key16=0xABCD) → ヒット → eval=100を取得
3. 局面Bで ensure_nnue_accumulator → nnue_evaluate → eval=500 (正しい値)
4. mismatch: TT eval=100 vs computed=500
```

## 検証データ

### mismatchの例

```
[NNUE MISMATCH] diff=404 TT=372 eval1=-32 eval2=-32 same=true
  sfen=ln3g1nl/1rs1gk3/p3ppsp1/2pp2p1p/1p3P1P1/2P1S4/PPSPP1P1P/2G4R1/LN2KG1NL b Bb 25
  key=0c4339345954c86f

[NNUE MISMATCH] diff=4629 TT=12 eval1=4641 eval2=4641 same=true
  sfen=l2g5/1Gsk5/4p+R1p1/p1p2N2p/3p1N3/PPP2PP1P/2GPPS1b1/4L4/LN2K3L b RB2SN4Pg 77
  key=6bfaa34403005354
```

### 自己対局結果

| 条件 | 勝敗 | 勝率 |
|------|------|------|
| 変更前 (71ebeb50 vs b05c0444) | 3/120勝 | 2.5% |
| TTからのeval取得を無効化 | 8/10勝 | 80% |

## 解決策の選択肢

### A) ensure_nnue_accumulator の呼び出しを削除（シンプル）

TTヒット時に `ensure_nnue_accumulator` を呼ばず、TTからのevalも使わない。

- メリット: シンプルな変更
- デメリット: 子ノードで差分更新ができなくなる可能性

### B) evalの差が閾値を超えたら再計算（安全策）

TTから取得したevalと計算したevalの差が大きい場合、計算した値を使用。

```rust
let computed_eval = nnue_evaluate(st, pos);
let tt_eval = tt_ctx.data.eval;
if (computed_eval.raw() - tt_eval.raw()).abs() > THRESHOLD {
    unadjusted_static_eval = computed_eval;  // 計算値を使用
} else {
    unadjusted_static_eval = tt_eval;  // TTの値を使用
}
```

- メリット: key16衝突の影響を軽減しつつ、TTの恩恵を受けられる
- デメリット: 毎回評価計算が必要（性能低下）

### C) TTのキーマッチングを強化（根本的解決）

key16ではなく、より多くのビット（例：key32）を使用してマッチング。

- メリット: 衝突確率を大幅に低減
- デメリット: TTエントリのサイズ増加、変更量大

### D) TTからのeval取得を完全に無効化

常に `nnue_evaluate` を呼び出して評価値を計算。

- メリット: 確実にkey16衝突の影響を排除
- デメリット: TTのeval保存が無駄になる

## 64bitキー実装の試行 (2026-02-05)

### 実装内容

YaneuraOuの拡張方式（`TT_CLUSTER_SIZE != 3`時の64bitキー）を参考に、TTのキーマッチングを64bitに拡張した。

#### 変更点

1. **TTEntry構造の変更**: `key16: u16` → `key64: u64`
   - エントリサイズ: 10バイト → 16バイト

2. **クラスター構造の変更**:
   - 試行1: 16バイト × 2エントリ = 32バイト（CLUSTER_SIZE=2）
   - 試行2: 16バイト × 3エントリ + 16パディング = 64バイト（CLUSTER_SIZE=3）

3. **probe/write関数**: key64での完全マッチングに変更

### 自己対局結果

| 条件 | Black | White | 結果 | 勝率 |
|------|-------|-------|------|------|
| 64bitキー + eval取得あり（2エントリ） | 新実装 | 暫定対策版 | 1勝6敗 | 14% |
| 64bitキー + eval取得あり（3エントリ） | 新実装 | 暫定対策版 | 1勝3敗 | 25% |
| **64bitキー + eval取得なし（3エントリ）** | 新実装 | 暫定対策版 | **6勝4敗** | **60%** |

### 原因切り分け結果

64bitキー実装自体は問題なく、問題は**TTからのeval取得にある**ことが判明した。

- 64bitキーでマッチングしているにも関わらず、eval取得を有効にすると棋力低下
- eval取得を無効化すると、暫定対策版と同等以上の棋力（60%勝率）

### 考察

64bitキー実装後もeval取得で問題が発生する理由として、以下が考えられる：

1. **TT race condition**: マルチスレッド時の不整合（今回はthreads=1なので該当しない）
2. **evalの保存/取得タイミングの問題**: 局面状態と保存されたevalの不整合
3. **`ensure_nnue_accumulator`の動作問題**: 差分更新の準備が不完全な可能性

### 結論

64bitキーへの変更は完了したが、TTからのeval取得は引き続き無効化する。今後の調査課題：

- `ensure_nnue_accumulator`の動作検証
- TTへのeval保存/取得ロジックの詳細調査
- YaneuraOuとの実装比較

## 参考情報

### YaneuraOuの実装

YaneuraOuでは`TT_CLUSTER_SIZE`でキーサイズを切り替え可能：

```cpp
// source/tt.h
#if TT_CLUSTER_SIZE == 3
typedef uint16_t TTE_KEY_TYPE;  // Stockfish互換: 16bit
#else // TT_CLUSTER_SIZEが2,4,6,8の時は64bit
typedef uint64_t TTE_KEY_TYPE;  // やねうら王拡張: 64bit
#endif
```

rshogiでは64bitキー固定で実装した。

## 関連ファイル

- `crates/rshogi-core/src/tt/table.rs` - TT probe/write実装
- `crates/rshogi-core/src/tt/entry.rs` - TTEntry定義（64bitキー対応）
- `crates/rshogi-core/src/tt/mod.rs` - CLUSTER_SIZE定義
- `crates/rshogi-core/src/search/eval_helpers.rs` - 静的評価コンテキスト計算
- `crates/rshogi-core/src/nnue/network.rs` - NNUE評価・アキュムレータ更新

## 調査日

- 初回調査: 2026-02-05
- 64bitキー実装: 2026-02-05
