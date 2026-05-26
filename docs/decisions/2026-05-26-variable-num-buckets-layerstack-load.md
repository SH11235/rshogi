# ADR: 可変バケット数 LayerStack net の読込・bucket 推論対応 (Issue #727)

- Date: 2026-05-26
- Scope: rshogi-core (`crates/rshogi-core/src/nnue/`)
- Related: tatara ADR `2026-05-23-num-buckets-configurable.md`, rshogi Issue #727, tatara #233 / #236

---

## 1. 背景 (Context)

学習側 (tatara) は LayerStack の bucket 数 N を可変化した
(`crates/nnue-format/src/layerstack_weights.rs`)。これに伴い:

- `.bin` の **`NNUE_VERSION` を `0x7AF32F20` → `0x7AF32F21` に bump**。
- 新 layout では **`arch_str` の直後に `num_buckets: u32`** が挿入される
  (`layerstack_weights.rs:23-24, 80-83`)。
- 旧 `0x7AF32F20` は **`num_buckets` field を持たず、暗黙 9 bucket** として扱う
  legacy compat path。
- progress → bucket の binning は学習側で `floor(sigmoid(sum) × N)` に一般化される。
- bucket 数は `USI option で渡さず、必ず net file から読む`
  (file/option desync による silent な誤評価を防ぐため、tatara ADR §5)。

現状 rshogi engine は LayerStack `.bin` に対して以下を hardcode している:

- `crates/rshogi-core/src/nnue/constants.rs:142` `NUM_LAYER_STACK_BUCKETS = 9`
- `crates/rshogi-core/src/nnue/layer_stacks.rs:177-187`
  `LayerStacks::buckets: [LayerStackBucket; 9]`
- `feature_transformer_layer_stacks.rs:83,87,127-131`
  PSQT `[i32; 9]` + 9-bucket 専用 SIMD `psqt_add_or_sub`
- `accumulator_layer_stacks.rs:30,47,123,138` PSQT acc `[i32; 9]`
- `network.rs:144-152` `PROGRESS_BUCKET_THRESHOLDS: [f32; 7]`
  (固定 8-bucket 用、`ln(k/(8-k))`)
- `network.rs:971-973` `progress_sum_to_bucket()` → 8-bucket 専用
- 受理する version: `NNUE_VERSION` (`0x7AF32F16`, HalfKP) と
  `NNUE_VERSION_HALFKA` (`0x7AF32F20`, HalfKa 系および LayerStack legacy)
  (`network.rs:325`, `network_layer_stacks.rs:174` `version` を読み飛ばすのみ)

このため、新 `.bin` (version bump 後) を読むと:
1. version check で reject される (`network.rs:473-478` の `_ => Err(...)`)、または
2. version pass しても `num_buckets` field が arch_str と FT block の間にあると認識
   されず、FT hash 等を誤読する。

本 ADR では engine 側の対応方針を確定する。

---

## 2. 決定事項 (Decision)

### 2.1 受理する `.bin` version の拡張

`crates/rshogi-core/src/nnue/constants.rs` に新 version 定数を追加する:

```rust
/// LayerStack 可変 bucket 数 layout の version。
/// `arch_str` の直後に `num_buckets: u32` field を持つ self-describing layout。
pub const NNUE_VERSION_LAYERSTACK_NUM_BUCKETS: u32 = 0x7AF32F21;
```

`NNUE_VERSION_HALFKA = 0x7AF32F20` は **legacy LayerStack `.bin`** および現行の
HalfKa 系 `.bin` の両方で引き続き使われるため、値は維持する (tatara 側 ADR §4 の
`LEGACY_NNUE_VERSION_BUCKETS9` と同値)。

`NNUENetwork::read` (`network.rs:324`) の match arm を以下のように拡張する:

```rust
match version {
    NNUE_VERSION | NNUE_VERSION_HALFKA | NNUE_VERSION_LAYERSTACK_NUM_BUCKETS => { ... }
    _ => Err(...),
}
```

`detect_nnue_format` (`network.rs:1086`) の同様の match arm にも追加する。

### 2.2 `num_buckets` の取得経路

`num_buckets` は **`.bin` header の `arch_str` 直後 (`ft_hash` の直前) から読む**。
USI option による上書きは導入しない (tatara ADR §5)。

LayerStack `.bin` の version → bucket 数 の対応:

| version 定数 (engine 側) | 値 | bucket layout | engine が取得する `num_buckets` |
|---|---|---|---|
| `NNUE_VERSION_HALFKA` | `0x7AF32F20` | field 無し、暗黙 9 | 定数 `9` を使う (compat path) |
| `NNUE_VERSION_LAYERSTACK_NUM_BUCKETS` | `0x7AF32F21` | `arch_str` 直後の `u32` | file から読む |

非 LayerStack (HalfKP / HalfKa 系) は本 ADR の対象外で、`num_buckets` の概念を持たない。

#### tatara `save_quantised` が出力する byte layout (num_buckets-header = `0x7AF32F21`)

tatara `crates/nnue-format/src/layerstack_weights.rs:459-553` の write 順を engine
側 read で対称に追う:

```
[0..4)    NNUE_VERSION                         u32 LE
[4..8)    network_hash                          u32 LE
[8..12)   arch_len                              u32 LE
[12..12+arch_len)  arch_str (UTF-8)
[next 4)  num_buckets                           u32 LE   ← num_buckets-header で追加 / legacy には無い
[next 4)  ft_hash                               u32 LE
...       FT biases (LEB128 magic + size + i16)
...       FT weights (LEB128 magic + size + i16)
...       PSQT block (任意、arch_str に `PSQT={num_buckets},`)
            - bias  i32 LE × num_buckets   (常にゼロ)
            - weights i32 LE × ft_in * num_buckets, layout
              `psqt_w[feat * num_buckets + bucket]` (feature-major)
...       LayerStacks: num_buckets × {
            fc_hash   u32 LE
            L1.bias   i32 LE × LS_L1_OUT
            L1.weight i8  × LS_L1_OUT × pad32(L1)   (raw, padded to 32)
            L2.bias   i32 LE × LS_L2_OUT (= NNUE_PYTORCH_L3 = 32)
            L2.weight i8  × NNUE_PYTORCH_L3 × pad32(LS_L2_IN)
            L3.bias   i32 LE × 1
            L3.weight i8  × 1 × pad32(NNUE_PYTORCH_L3)
          }
EOF
```

engine 側で本順序通り `num_buckets` を `arch_str` と `ft_hash` の間で **1 回だけ
読む**こと、legacy version では本 field を **読まずに `num_buckets = 9` 固定**で
進めることを実装で守る。順序がずれると `ft_hash` で 4 byte ずれた値を取り、
mismatch エラーで気付ける (silent corruption ではないがロード失敗)。

### 2.3 ランタイム可変 N の表現方式

`num_buckets` は file から読まれる **runtime 値**。型レベル const generic 化は
しない (file load 時に N が決まるため、コンパイル時固定では engine が単一 N しか
扱えなくなる)。

具体的な struct 変更:

#### 2.3.1 `LayerStacks` (layer_stacks.rs:177-187)

```rust
pub struct LayerStacks<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> {
    /// 可変長の bucket 列。長さ == num_buckets。
    pub buckets:
        Vec<LayerStackBucket<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>>,
}
```

`Vec` は load 時に 1 回だけ allocate され、評価ホットパスでは **read-only に
indexing するのみ**。再 alloc / push は発生しない (`CLAUDE.md` のヒープ割当禁止
方針はホットパス内のみが対象であり、startup 時の 1 回 alloc は許容範囲)。

`buckets.len()` は engine 内 `num_buckets()` accessor として参照する。

`LayerStacks::read` の signature を変更:

```rust
pub fn read<R: Read>(reader: &mut R, num_buckets: usize) -> io::Result<Self>;
```

呼び出し元 (`network_layer_stacks.rs:284`) は header から読んだ `num_buckets` を渡す。

#### 2.3.2 PSQT 重み・accumulator のレイアウト

PSQT 周りの `[i32; 9]` 固定配列を、**`MAX_LAYER_STACK_BUCKETS = 16` の上限付き
固定配列 + 実行時 N**に置き換える:

```rust
/// LayerStack bucket 数の上限。
/// engine は file の `num_buckets ≤ MAX_LAYER_STACK_BUCKETS` を満たす場合のみ load
/// する。上限超過時は `InvalidData` で reject。
///
/// 現行の tatara ADR §3 は N ≤ 9 を host plumbing 範囲とし、N > 9 は今後の予定。
/// 16 は近未来の sweep (N=8, 12, 16 等) を吸収しつつ、accumulator memory
/// footprint の増分を最小に抑える値として選択。
pub const MAX_LAYER_STACK_BUCKETS: usize = 16;
```

`AccumulatorLayerStacks::psqt_accumulation` (`accumulator_layer_stacks.rs:30`):

```rust
#[cfg(feature = "ls-ext-psqt")]
pub psqt_accumulation: [[i32; MAX_LAYER_STACK_BUCKETS]; 2],
```

メモリ増分は per-accumulator `(MAX - 9) × 2 × 4 bytes = 56 bytes`。
`AccCacheEntry` 等の cache 系も同様に拡張する (現行 ~520 KB → ~580 KB、許容)。

`FeatureTransformerLayerStacks::psqt_biases` (`feature_transformer_layer_stacks.rs:83`):

```rust
#[cfg(feature = "ls-ext-psqt")]
pub(crate) psqt_biases: [i32; MAX_LAYER_STACK_BUCKETS],
```

ただし **有効範囲は `[..num_buckets]` のみ**、`[num_buckets..]` は load 時にゼロで
残す (PSQT add/sub 時に 0 加算/減算で副作用なし)。

`psqt_weights` (`feature_transformer_layer_stacks.rs:87`) は `AlignedBox<i32>` で
すでに動的サイズなので、長さを `FT::DIMENSIONS × num_buckets` に変えるだけ。
レイアウトは `psqt_weights[feature_idx * num_buckets + bucket]` を維持
(tatara `layerstack_weights.rs:354` `(num_buckets, l2_out, l2_in)` row-major と整合)。

`FeatureTransformerLayerStacks` に `num_buckets: usize` field を追加して、
`add_psqt_weights` / `sub_psqt_weights` (`feature_transformer_layer_stacks.rs:649,666`)
で `offset = index * self.num_buckets` と参照する。

#### 2.3.3 SIMD `psqt_add_or_sub` の N 化

現行 `psqt_add_or_sub<ADD>` (`feature_transformer_layer_stacks.rs:127-131`) は
`const { assert!(NUM_LAYER_STACK_BUCKETS == 9, ...) }` で 9-bucket 専用に SIMD lane
mask / scalar tail を hardcode している。

これを **runtime `n` 引数を取る関数**に置き換える:

```rust
#[inline(always)]
fn psqt_add_or_sub<const ADD: bool>(
    psqt_acc: &mut [i32; MAX_LAYER_STACK_BUCKETS],
    weights: *const i32,
    n: usize,  // 有効 bucket 数 (= self.num_buckets)
) {
    debug_assert!(n <= MAX_LAYER_STACK_BUCKETS);
    // n 個の i32 を psqt_acc[..n] と weights[..n] で add/sub する。
    // ...
}
```

実装方針:

- **scalar loop** を default 実装とし、`n` ループで `wrapping_add` / `wrapping_sub`
  を行う。`MAX = 16` で n ≤ 16 → ループ 16 回未満。compiler が auto-vectorize する
  可能性が高い。
- 既存の手書き SIMD path (AVX-512 / AVX2 / SSE2 / NEON) は **削除**して scalar に
  統一する。理由:
  - N が runtime 値になり、9-lane 固定 mask / tail 8 番目 scalar の構造が壊れる。
  - n が runtime のため lane mask を runtime 構築するコストが scalar をある程度
    打ち消す可能性がある。
  - PSQT add/sub の hot 度合いは FT の i16 加算と比べて小さく
    (`FT::DIMENSIONS × NUM_BUCKETS << FT::DIMENSIONS × L1`)、early optimization に
    値しない (`CLAUDE.md`「早すぎる最適化は禁止」)。
  - 実測で必要と判明したら別 ADR / Issue で再導入を検討する。

#### 2.3.4 `[i32; NUM_LAYER_STACK_BUCKETS]` を引数で取る関数の更新

`accumulator_layer_stacks.rs:286,288,298,299,1065` 等で
`&mut [i32; NUM_LAYER_STACK_BUCKETS]` を取っている API を、
`&mut [i32; MAX_LAYER_STACK_BUCKETS]` + `num_buckets: usize` の組に変更する。

呼び出し側で同一の `num_buckets` を渡し、loop 範囲を `..num_buckets` に統一する。

### 2.4 progress → bucket binning の N 化

`crates/rshogi-core/src/nnue/network.rs:144-152` の固定 `[f32; 7]` 閾値テーブルを、
**runtime N で動的構築**する形に変更する。

#### 2.4.1 binning 式と tatara との等価性

tatara 側 (`crates/shogi-features/src/progress_kpabs.rs:170`) は:

```rust
let p = sigmoid(sum);
let raw = (p * num_buckets as f32).floor() as i32;
let bucket = raw.clamp(0, num_buckets as i32 - 1) as u8;
```

engine 側はホットパスで `exp()` / `sigmoid()` を避けるため、閾値を pre-compute して
`partition_point` で求める:

```rust
/// progress sum (= ∑weights[k(p)][bp] over piece list) を [0, N) の bucket index
/// に写像する。式は `floor(sigmoid(sum) × N).clamp(0, N-1)` だが、exp() を避け、
/// `sum >= ln(k / (N-k))` (k=1..N-1) の partition_point で計算する。
///
/// 学習側 (tatara) の `bucket()` 一般化と完全に一致させる。
pub fn progress_sum_to_bucket(sum: f32, num_buckets: usize) -> usize {
    debug_assert!(num_buckets >= 1 && num_buckets <= MAX_LAYER_STACK_BUCKETS);
    let thresholds = layer_stack_progress_thresholds(num_buckets);
    thresholds.partition_point(|&t| sum >= t)
}
```

等価性: thresholds は `[t_1, t_2, ..., t_{N-1}]` with `t_k = ln(k / (N-k))`
(`sigmoid(t_k) = k/N`)。`sum ∈ [t_k, t_{k+1})` ⇔ `sigmoid(sum) ∈ [k/N, (k+1)/N)` ⇔
`floor(p × N) = k`。partition_point は「satisfying threshold の個数」を返すため
`sum ∈ [t_k, t_{k+1})` のとき `k+1`... ではなく、`sum ≥ t_1, ..., t_k` の k 個。
`sum ∈ [t_k, t_{k+1})` で `sum < t_{k+1}` (含む `k=0` で `sum < t_1`) のため
ちょうど k 個 → bucket = k。tatara の `floor(p × N)` と一致。

sum = +∞ (p = 1) のとき partition_point は `N-1`、tatara は
`clamp(N, 0, N-1) = N-1`。一致。

tie-break: tatara は `floor` で下方向、engine は `sum ≥ t_k` で `t_k` ちょうどは
含む。`sigmoid(t_k) = k/N` ⇔ `sum = t_k` のとき、tatara `floor(k/N × N) = k`、
engine partition_point は k → 一致。

#### 2.4.2 閾値テーブルのキャッシュ

N ごとの `Vec<f32>` を `OnceLock<[OnceLock<Box<[f32]>>; MAX+1]>` または
`'static` 配列に lazy 構築し、`progress_sum_to_bucket` の各呼び出しでは
slice 参照のみ取る。`ln()` の評価は **N ごとに最大 N-1 回 / 1 度きり**。

#### 2.4.3 `num_buckets` を global state ではなく net instance に持つ

`get_layer_stack_progress_kpabs_weights()` (`network.rs:208`) の global pointer
pattern と統一性を持たせる選択肢もあるが、Codex レビューで指摘された通り **複数
net load / thread safety / load 順序** の事故面が増える。

本 ADR では **`num_buckets` を net instance のフィールドとして保持**する方針を採る:

```rust
pub struct NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT, FT> {
    pub feature_transformer: FeatureTransformerLayerStacks<L1, FT>,  // num_buckets を含む
    pub layer_stacks: LayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    pub fv_scale: i32,
    pub num_buckets: usize,
    _ft: PhantomData<FT>,
}
```

評価パスでは:

```rust
pub fn evaluate(&self, pos: &Position, acc: &AccumulatorLayerStacks<L1>) -> Value {
    let bucket_index = compute_layer_stacks_bucket_index(
        pos, pos.side_to_move(), self.num_buckets,
    );
    self.evaluate_with_bucket(pos, acc, bucket_index)
}
```

`compute_layer_stacks_bucket_index` (`network_layer_stacks.rs:67`) は
`num_buckets` を第 3 引数で受け取り、`compute_layer_stack_progress8kpabs_bucket_index`
に伝播する:

```rust
fn compute_layer_stacks_bucket_index(
    pos: &Position, side_to_move: Color, num_buckets: usize,
) -> usize { ... }

pub fn compute_layer_stack_progress8kpabs_bucket_index(
    pos: &Position, side_to_move: Color, weights: &[f32], num_buckets: usize,
) -> usize {
    let sum = compute_progress8kpabs_sum(pos, weights);
    progress_sum_to_bucket(sum, num_buckets)
}
```

差分更新 path (`update_and_evaluate_layer_stacks`, `network.rs:1239` 周辺) も同様に
net instance の `num_buckets` を渡す。

**CACHED_PROGRESS_BUCKET** (`network.rs:159-161`) は thread-local の bucket index
キャッシュで、`Cell<Option<usize>>` 型。差分更新側で sum を計算した時点で
`progress_sum_to_bucket(sum, num_buckets)` を呼び、確定した bucket index のみを
キャッシュに置く (生 sum はキャッシュしない)。読み出し側はそのまま index を
消費する。

不変条件 (本 ADR で明示する単一 net 前提):

1. engine の USI loop は **同時に複数の LayerStack net をロード状態に持たない**。
   `EvalFile` を変更する場合は前 net を完全に drop してから新 net を load する
   (現状の `NNUENetwork` enum は単一 instance を保持する設計と一致)。
2. キャッシュへの **生成 / 消費** は同一 thread 内・同一探索イテレーション内で
   pair になる (`update_and_evaluate_layer_stacks` の構造から保証される)。
3. 上記 1 から、キャッシュに格納される bucket index は **常に現在 active な net
   の `num_buckets` で binning された値**になる。キャッシュ entry 自体に
   `num_buckets` を持つ必要はない。

将来 multi-net hot-swap や per-thread 異 net を導入する場合は、本不変条件の
再評価が必要。その時点で `Cell<Option<(usize, u32)>>` のように
`(bucket, num_buckets_epoch)` ペアを格納する拡張で対応する (本 ADR の範囲外)。

global state は `LAYER_STACK_PROGRESS_KP_ABS_PTR` (progress weights、`network.rs:175`)
だけに留め、`num_buckets` の global state は **新設しない**。

### 2.5 命名と文字列の整理

ADR では `progress8kpabs` の "8" は historical な命名として **保持**する (tatara ADR
`Consequences` §6: rshogi 側に残る literal 8/9 の識別子は既存 net 互換のため保持)。

ただし comment / doc では「N=8 専用」のような誤解を招く記述は修正し、「KP-absolute
特徴量を使う progress 計算」と「N bucket への binning」を **分離**して説明する:

- `compute_progress8kpabs_sum`: KP-abs 特徴量で sum を計算 (N に依存しない)
- `progress_sum_to_bucket(sum, n)`: sum を `[0, n)` の bucket へ binning

### 2.6 受理ポリシー: `num_buckets ≤ MAX_LAYER_STACK_BUCKETS`

`MAX_LAYER_STACK_BUCKETS = 16` を超える `num_buckets` を含む `.bin` は
`InvalidData` で reject する:

```
NumBuckets=20 exceeds engine's MAX_LAYER_STACK_BUCKETS=16.
This engine build does not support such large bucket counts; rebuild rshogi-core
with a larger MAX_LAYER_STACK_BUCKETS (see ADR 2026-05-26).
```

将来 N > 16 が必要になったら `MAX_LAYER_STACK_BUCKETS` を上げる。

`num_buckets == 0` も reject (`InvalidData`)。

### 2.7 architecture detection への影響

`crates/rshogi-core/src/nnue/spec.rs` の `detect_architecture_from_size` は
**LayerStack 以外**の formula で動作する (`network.rs:411,367` の通り、LayerStack は
file_size 検出パスを通らない)。よって本変更は `spec.rs` に影響しない。

`parse_arch_dimensions` (`spec.rs`) は arch_str から `L1 / L2 / L3` を抜き出すが、
**`num_buckets` は arch_str に埋め込まずに typed header field のみから取得**する
(tatara ADR §4 と一致、header の自己記述性を担保)。よって `parse_arch_dimensions`
の signature 変更は不要。

### 2.7.1 Threat / HandCount との interaction (本 ADR では out of scope)

現状の engine は `arch_str` に `Threat=` / `ThreatProfile=` が含まれる場合に
LayerStack 読込中に追加 block を読む (`network_layer_stacks.rs:239-274`)。
tatara `save_quantised` (`crates/nnue-format/src/layerstack_weights.rs:430-553`) は
**Threat / HandCount を未実装**で、PSQT block の直後に layerstacks を書く構造。

本 ADR の変更は **PSQT block の長さに `num_buckets` を掛ける**点が主な影響:

- legacy: PSQT bias `[i32; 9]` + weights `[i32; ft_in × 9]`
- num_buckets-header: PSQT bias `[i32; N]` + weights `[i32; ft_in × N]`

Threat / HandCount block 自体は `num_buckets` に依存しない (per-FT feature) ため、
本 ADR で構造変更は不要。Threat と num_buckets-header の組み合わせは tatara 側
がまだ書き出さないため、engine 側でも load 時に「両者同時指定の `.bin`」は
未テスト扱いとする (将来 tatara が対応した時に別 PR で検証する)。

### 2.7.2 9-固定の参照箇所一覧 (実装で全て一般化する)

| ファイル | 行 | 9-固定の参照 |
|---|---|---|
| `constants.rs` | 142 | `pub const NUM_LAYER_STACK_BUCKETS: usize = 9;` |
| `layer_stacks.rs` | 19, 186 | `use ... NUM_LAYER_STACK_BUCKETS;` / `[LayerStackBucket; NUM_LAYER_STACK_BUCKETS]` |
| `layer_stacks.rs` | 227-230, 242 | `debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS)` |
| `feature_transformer_layer_stacks.rs` | 83, 87, 104-107 | PSQT bias `[i32; 9]`, weights layout, doc-comment |
| `feature_transformer_layer_stacks.rs` | 127-131 | `psqt_add_or_sub` の `const { assert!(NUM == 9, ...) }` |
| `feature_transformer_layer_stacks.rs` | 134-263 | 9-bucket 専用 SIMD path (AVX512 mask `0x01FF`、AVX2/SSE2/NEON の `4+4+1` 構造) |
| `feature_transformer_layer_stacks.rs` | 282-283, 330, 367, 404 | PSQT bias/weights の new 初期化 |
| `feature_transformer_layer_stacks.rs` | 433-444, 461-463, 466, 635, 649-670, 1065 | PSQT read / add / sub 関連 |
| `feature_transformer_layer_stacks.rs` | 1852, 2127-2163 | test 用 PSQT 配列 |
| `accumulator_layer_stacks.rs` | 10, 30, 47 | `psqt_accumulation: [[i32; 9]; 2]` |
| `accumulator_layer_stacks.rs` | 123, 138 | `AccCacheEntry::psqt_accumulation: [i32; 9]` |
| `accumulator_layer_stacks.rs` | 264-1124 | API signature の `[i32; 9]` 引数 |
| `network.rs` | 144-152 | `PROGRESS_BUCKET_THRESHOLDS: [f32; 7]` (8-bucket 固定) |
| `network.rs` | 832-844, 910-925 | `compute_layer_stack_progress8kpabs_bucket_index`, `update_progress8kpabs_sum_diff` の bucket 計算 |
| `network.rs` | 969-973 | `progress_sum_to_bucket(sum) -> usize` (引数 N 無し) |
| `network.rs` | 1197-1275 | `update_and_evaluate_layer_stacks` の bucket 計算 |
| `network.rs` | 2115-2147 | bucket index range / sigmoid 一致 test |
| `bin/dump_psqt_stats.rs` | 23-118 | `NUM_LAYER_STACK_BUCKETS` 参照 (debug tool) |

本 ADR の実装 PR は **上記すべてを単一の change set で N 化**する。
中途半端な部分置換 (一部 `[i32; 9]`、一部 `[i32; MAX]`) は混乱の元なので避ける。

### 2.7.3 `LayerStacks::buckets` の Vec 不変条件

`Vec<LayerStackBucket>` は load 時に 1 回だけ `Vec::with_capacity(num_buckets)` +
`push` で構築され、以後 **read-only**:

- `clear` / `push` / `pop` / `resize` / `truncate` を **呼ばない**。
- ホットパス (`evaluate_raw`) では `self.buckets[bucket_index]` または
  `self.buckets.get_unchecked(bucket_index)` で indexing のみ。
- bucket_index は `progress_sum_to_bucket` で `[0, num_buckets)` に閉じている
  ため、`get_unchecked` は安全 (`SAFETY` コメントに明記)。

heap allocation は load 時の 1 回のみで、`CLAUDE.md` 「ホットパスでヒープ割当
禁止」と矛盾しない。

### 2.8 USI 連携

USI option での bucket 数指定は **追加しない** (tatara ADR §5)。ただし `info string`
で **load した `num_buckets` を観測可能**にする (debug / SPRT で混同を防ぐため):

```
info string NNUE LayerStack num_buckets=9 (legacy v0x7AF32F20)
info string NNUE LayerStack num_buckets=12 (v0x7AF32F21)
```

出力位置は `crates/rshogi-usi/src/main.rs` の NNUE load 完了ログに追加する。

---

## 3. 代替案と棄却理由 (Alternatives)

### 3.1 const generic `N` で型レベル可変化

各 N を別の monomorphization にする案。

- **棄却**: file load 時に N が決まるため、engine は単一 N しか扱えなくなる
  (build を別 N で複数本作る必要がある)。Issue #727 の目的「N を振って学習・棋力
  比較」と相性が悪い (engine 側も毎回 build し直す)。
- USI 起動時に N を選ぶ logic を入れれば一応動くが、cfg-gated explosion と
  cargo build matrix の複雑さが大きい。

### 3.2 全 PSQT 配列を `Vec<i32>` に変更 (MAX 上限撤廃)

`psqt_accumulation: Vec<[i32; 2]>` 等で完全可変化する案。

- **棄却**: `AccumulatorLayerStacks` は search stack の各 ply に常駐する hot data。
  `Vec` 化すると (a) `Vec<i32>` 間接参照が cacheline 跨ぎを発生させ、(b) clone /
  copy のコストが上がる。fixed `[i32; MAX=16]` は per-accumulator +56 bytes で
  ホットパスのレイアウト変更を最小化できる。
- 学習側で実際に試す N の幅 (5〜12 程度を想定) には 16 が十分に余裕がある。

### 3.3 PSQT SIMD path を維持して N 別に分岐

`match num_buckets { 9 => avx2_n9_path(), 12 => avx2_n12_path(), ... }` で各 N 別の
SIMD 専用 path を保持する案。

- **棄却**: 維持コストが N の数だけ増える。N が runtime のため `const` mask が
  使えず、runtime mask 構築のコストで scalar との差が縮む。
- 実測で SIMD が必要と判明したら別 ADR / Issue で「N≤16 を 16-lane AVX-512 mask
  で 1 命令」など方針を立てた上で再実装する。
- 本 ADR スコープの「N の可変化」と「PSQT SIMD の N 化」は分離する。

### 3.4 旧 legacy version `0x7AF32F20` を強制 reject

新 version 専用に統一する案。

- **棄却**: 既存の `v87-400-layerstack.bin` 等の配布 net (legacy 0x7AF32F20) が
  load 不能になり、ロールアウト時に SPRT が止まる。tatara ADR §4 の「両 version
  混在しても silent corruption は起きない」前提と、engine 側で legacy compat path
  を持つことが要件 (Issue 本文)。

---

## 4. 影響範囲 (Affected files / Consequences)

### 主要変更ファイル

| ファイル | 変更内容 |
|---|---|
| `crates/rshogi-core/src/nnue/constants.rs` | `NNUE_VERSION_LAYERSTACK_NUM_BUCKETS`, `MAX_LAYER_STACK_BUCKETS`, `DEFAULT_NUM_BUCKETS` を追加。`NUM_LAYER_STACK_BUCKETS` は `DEFAULT_NUM_BUCKETS` に rename (§4.1 補足) |
| `crates/rshogi-core/src/nnue/network.rs` | version match の拡張。`progress_sum_to_bucket(sum, n)` に signature 変更。`PROGRESS_BUCKET_THRESHOLDS` を N ごとの runtime 計算 (OnceLock キャッシュ) に置換。LayerStack load path で `num_buckets` を読む |
| `crates/rshogi-core/src/nnue/network_layer_stacks.rs` | `read_with_options` で legacy / num_buckets-header を判別し `num_buckets` を取得、`LayerStacks::read(reader, num_buckets)` に伝播 |
| `crates/rshogi-core/src/nnue/layer_stacks.rs` | `LayerStacks::buckets: Vec<...>`, `read(reader, num_buckets)`, `evaluate_raw` の `debug_assert!` を `< self.buckets.len()` に変更 |
| `crates/rshogi-core/src/nnue/feature_transformer_layer_stacks.rs` | `psqt_biases: [i32; MAX]`, `psqt_weights` 長さ = `DIM × num_buckets`, `num_buckets` field 追加、`psqt_add_or_sub` を runtime n に対応、add/sub の offset 計算を `num_buckets` 駆動に |
| `crates/rshogi-core/src/nnue/accumulator_layer_stacks.rs` | `psqt_accumulation: [[i32; MAX]; 2]`, `AccCacheEntry::psqt_accumulation: [i32; MAX]`, API signature を `[i32; MAX]` に揃える、loop 範囲を `..num_buckets` に統一 |
| `crates/rshogi-core/src/bin/dump_psqt_stats.rs` | `NUM_LAYER_STACK_BUCKETS` 参照を runtime `num_buckets` に変更 |
| `crates/rshogi-usi/src/main.rs` | NNUE load 後に `info string NNUE LayerStack num_buckets=...` を出力 |

### 4.1 補足: `NUM_LAYER_STACK_BUCKETS` の扱い

`pub const NUM_LAYER_STACK_BUCKETS: usize = 9` は legacy 互換のために
`pub const DEFAULT_NUM_BUCKETS: usize = 9` と意味的に同じ。混乱を避けるため
`DEFAULT_NUM_BUCKETS` に **rename** し、既存の `NUM_LAYER_STACK_BUCKETS` は
`pub use ... as NUM_LAYER_STACK_BUCKETS` 等の re-export はせず削除する。

ただし外部 binary (例: `dump_psqt_stats`) や test で参照している箇所が多いため、
1 PR でまとめて変更する。

### テスト

- **sigmoid 等価性**: `progress_sum_to_bucket(sum, n)` の出力が
  `floor(sigmoid(sum) * n).clamp(0, n-1)` と全領域で一致することを `n ∈ {1, 2, 3,
  4, 5, 8, 9, 12, 16}` × `sum ∈ {-10, -5, t_k-ε, t_k, t_k+ε, 0, 5, 10}` で検証。
- **境界 / clamp**:
  - `n = 1`: 常に bucket 0。
  - `sum = +∞`: bucket = n-1 (clamp working)。
  - `sum = -∞`: bucket = 0。
  - `sum = t_k` (閾値ちょうど): partition_point の `>=` 比較で **k** を返すこと。
- **legacy compat**: version `0x7AF32F20` (legacy) の `.bin` を load → engine 内部
  の `num_buckets` が **9** になることを in-memory fixture で確認。
- **num_buckets-header path**: version `0x7AF32F21` + `num_buckets = 9` を含む
  in-memory `.bin` を構築 (header + 9 個の空 LayerStackBucket) し、load 完了する
  ことを確認。
- **同一 net の legacy/num_buckets-header 一致**: 同じ FT / PSQT / LayerStacks
  weight を持つ legacy・num_buckets-header fixture を作り、同じ局面で
  **`evaluate()` が bit-identical** になることを確認 (`num_buckets = 9` ならば
  binning 式自体は両 path 同じ式になる)。
- **N ≠ 9 path**: in-memory で N=5 / N=12 の最小 fixture を作り、`buckets.len() == N`、
  `psqt_weights.len() == DIM × N` を確認。
- **reject**: `num_buckets ∈ {0, 17, 100}` で `InvalidData` reject される test。
- **Threat token 混在の reject**: num_buckets-header layout だが arch_str に
  `Threat=` を含む fixture を作成し、現状 tatara が未実装である旨を documented
  behavior として test に書く (load 自体は engine 既存パスで読めるが、本 ADR の
  動作確認範囲外)。
- **PSQT off path**: num_buckets-header + `arch_str` に `PSQT=` 無し で load し、
  PSQT 領域を読まずに layerstacks に到達することを確認。
- **PSQT layout byte-level 等価性**: tatara write の `psqt_w[feat * N + bucket]`
  と engine の `psqt_weights[feature_idx * num_buckets + bucket]` が一致することを
  byte-level fixture で検証する。具体的には:
  - in-memory `.bin` を「PSQT bias = `[0_i32; N]`、PSQT weights = `(0..ft_in*N as i32)`
    を順番に書き出した buffer」で生成 (feat-major、各 feat 内 bucket 連番)。
  - engine で load した後、`feature_transformer.psqt_weights()` の slice が
    `0, 1, 2, ..., ft_in*N - 1` の連続値であることを `assert_eq!` する。
  - 同 fixture について `add_psqt_weights(&mut acc, feat_idx)` を呼び、
    `acc[bucket]` が `feat_idx * N + bucket` (= file 上の i32 値そのもの) に
    なることを `bucket ∈ 0..N` で確認する。
  - 上記を `N ∈ {1, 5, 9, 16}` で実行し、layout の N 一般化に regression が無い
    ことを保証する。

外部 net `v87-400-layerstack.bin` (legacy `0x7AF32F20`、N=9 配布 net) を **手元の
load smoke test** として CI スキップ可な `#[ignore]` test に追加する (file path は
`crates/rshogi-core/tests/fixtures/` に配置済みであれば; 無ければ skip)。

### 性能

- 評価ホットパスへの追加コスト:
  - `progress_sum_to_bucket` で `OnceLock<&[f32]>` から閾値 slice を取る分の数 ns。
  - PSQT add/sub の手書き SIMD path 削除 → scalar に統一。`MAX = 16` の loop は
    compiler が auto-vectorize する可能性大。実測で回帰したら別 ADR で再 SIMD 化。
  - LayerStack eval の `buckets[i].propagate(...)` は indexing が `Vec` 経由になる
    が、外側 dispatch は 1 回のため影響微小。
- Accumulator memory: 1 accumulator あたり PSQT 部分が `[i32; 9]` (36B) → `[i32; 16]`
  (64B) の +28B 拡張 × 2 perspective = +56B/accumulator。
  `AccCacheEntry` も同様 (162 entries で +9 KB)。

### ロールアウト

1. 本 ADR を merge し、num_buckets 対応 engine を release する。本 engine は
   **legacy `.bin` も num_buckets-header `.bin` も両方 load できる**。
2. tatara 側の version bump (tatara #236) が landing 後、新 `.bin` を本 engine で
   load して SPRT を実行する。
3. 既存配布 net (legacy `0x7AF32F20`、`v87-400-layerstack.bin` 等) は引き続き
   load 可 (compat path で N=9 として読む)。

旧 engine (本 ADR landing 前) に num_buckets-header `.bin` を渡すと version mismatch で reject される
(silent corruption は起きない、tatara ADR §4 の前提通り)。

---

## 5. 未決事項 (Open questions)

- `MAX_LAYER_STACK_BUCKETS = 16` の妥当性。tatara 側で実際に試す N の上限が明確に
  なれば変更する。実装時は **N ≤ 9 を最初に動かして、上限拡張は後続 PR** とする
  ことも可能。本 ADR では「engine が認識する file 上の N」と「engine 内部の
  MAX 配列長」を分離して書いている。
- `NUM_LAYER_STACK_BUCKETS` const の即時 rename vs alias 経由の段階的 rename。
  外部 binary や test に多数参照があるため、**1 PR でまとめて rename** する方針。
- PSQT SIMD path 削除に伴う性能回帰の有無。実測は実装 PR の中で
  `cargo bench` (該当があれば) または engine NPS で確認する。
