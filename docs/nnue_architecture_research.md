# NNUE アーキテクチャ研究計画

## 概要

現行の LayerStack (HalfKA_hm, without PSQT) をベースに、
PSQT ショートカットと Threat 特徴量の追加を検証する。
学習は `../bullet-shogi` で実施。

## 用語

| 用語 | 意味 |
|------|------|
| **HalfKA_hm** | 「玉位置 × 各駒位置」の入力特徴量。rshogi の現行方式 |
| **LayerStack** | bucket 分割（progress8kpabs）付きのネットワーク構造。1536x16x32 |
| **PSQT** | Piece-Square Table。Feature Transformer から L1 を通さず直接最終評価に加算するショートカット |
| **Threat** | 駒の利き関係（どの駒がどの駒を攻撃しているか）の特徴量 |

## 現行アーキテクチャ

```
LayerStack (SFNNwoP1536_V2, without PSQT):

Input: HalfKA_hm 特徴量
  ↓
Feature Transformer (差分更新): → accumulator [i16; 1536×2]
  ↓ SqrClippedReLU + ClippedReLU → [u8; 1536]
L1: AffineTransform(1536 → 16)
  ↓ split [15, 1] → SqrClippedReLU(15) + ClippedReLU(15) → [u8; 30]
L2: AffineTransform(30 → 32)
  ↓ ClippedReLU → [u8; 32]
Output: AffineTransform(32 → 1) + skip(l1_out[15])
  → i32 → 最終評価値

bucket 選択: progress8kpabs（進行度ベース、9 バケット）
```

## 実験マトリクス

### 一覧

| # | PSQT | Threat | 概要 | 優先度 | 状態 |
|---|:----:|:------:|------|:------:|------|
| 0（現行） | なし | なし | LayerStack 1536x16x32 | — | ベースライン |
| **1** | **あり** | なし | LayerStack + PSQT ショートカット | **高** | 未着手 |
| **2a** | なし | **あり（盤上のみ）** | LayerStack + Threat（チェス準拠） | 中 | 未着手 |
| **2b** | なし | **あり（持ち駒考慮）** | LayerStack + Threat（将棋拡張） | 中 | 未着手 |
| 3 | あり | あり | LayerStack + PSQT + Threat（best of 1+2） | 低 | 1,2 の結果次第 |
| **4** | — | — | **Reckless 方式: PST+Threat 分離アキュムレータ** | 低 | 1-3 完了後 |

**実験 4** は Reckless 型の「PST ベクトル accumulator + Threat 別 accumulator → 要素和 → Hadamard 積」
という別アーキテクチャの検証。実験 1-3（Stockfish 型 shortcut PSQT）の結果を踏まえて実施する。
PSQT の入れ方比較ではなく、**別 NNUE 設計比較**として扱う。

### 実験 1: PSQT ショートカット追加

**概要**: Feature Transformer の出力に PSQT チャネルを追加し、L1 を通さず最終評価値に直接加算。

```
変更後:

Feature Transformer → accumulator [i16; 1536×2]
                    → psqt [i32; bucket_count]  ← 追加
  ↓
L1(1536→16) → L2(16→32) → Output(32→1) + psqt[bucket]
```

**メリット**:
- 実装コスト小（Feature Transformer の出力チャネル追加 + スカラー加算）
- 推論コストほぼゼロ（差分更新に含まれる）
- Stockfish では標準的な改善として確立済み

**変更箇所**:
- bullet-shogi: Feature Transformer 定義に PSQT 出力を追加、損失関数で PSQT 項を反映
- rshogi: PSQT 値の読み込み + evaluate() で加算

**学習**: 既存データで再学習。比較的短期間で完了見込み。

### 実験 2a: Threat（盤上のみ、チェス準拠）

**概要**: Stockfish FullThreats 仕様に基づく Threat 特徴量を将棋に適用。盤上の駒の利き関係のみ。
Rust 実装技法は Reckless を参考にする（ただし特徴量定義は Stockfish に合わせる）。

**入力特徴量（追加分）**:
- 各マスに対する 8 方向レイ上の最近接スライダー駒
- 各マスに対する近接攻撃駒（桂馬、歩等）
- 持ち駒は**考慮しない**

**メリット**: Stockfish で実績のある設計を将棋に適用可能
**デメリット**: 将棋の重要な情報（持ち駒の打ち込み脅威）が欠落
**注意**: 将棋の駒種・マス数がチェスと異なるため、特徴量次元と除外ルールは将棋向けに再設計が必要

### 実験 2b: Threat（持ち駒考慮、将棋拡張）

**概要**: 2a に加え、持ち駒の打ち込み脅威を特徴量に含める。

**追加する特徴量の候補**:

1. **打ち込み利きマスク**: 手番側が持っている各駒種について、合法な打ち込み先のビットマスク
   - 例: 「先手が角を持っている → 盤上の空きマスで角を打てる場所」
   - 次元: `持ち駒種(7) × 2(手番)` のバイナリ特徴

2. **打ち込み脅威**: 持ち駒を打ったときに相手駒に利く関係
   - 例: 「銀を 4 四に打つと相手の飛車に利く」
   - これは「仮想的に打った場合の Threat」で計算コストが大きい

3. ~~**持ち駒存在フラグ**~~: **不要** — HalfKA_hm が手駒を active feature として既に列挙している
   （`rshogi: half_ka_hm.rs:53`, `bullet-shogi: shogi_halfka.rs:173`）

**推奨**: まず 1（打ち込み利きマスク）のみで試行。2 は計算コストが高すぎる。
3 は HalfKA_hm に含まれるため追加不要。2b で検討すべきは「持っているか」ではなく
「打ち込みで新たに生まれる脅威」。

**メリット**: 将棋固有の重要情報を直接入力できる
**デメリット**: チェスの実装から大きく逸脱、独自設計が必要

### 実験 3: PSQT + Threat

実験 1 と 2（best of 2a/2b）の組み合わせ。両方が個別に有効な場合のみ実施。

## アーキテクチャ結合度

PSQT と Threat はどちらも **Feature Transformer 層の変更** であり、L1 以降の構造には影響しない。
ただし既存モデルの重みは使えず、各実験ごとに bullet-shogi での再学習が必須。

Threat は **別アキュムレータ（FT 出力と同次元）** を持ち、
FT 出力に**要素和**で加算してから活性化関数に渡す（Stockfish / Reckless 共通の方式）。
L1 入力次元は変更しない。特徴量定義は Stockfish 仕様に合わせる（Reckless とは次元・除外ルールが異なる）。
（参照: `Stockfish nnue_feature_transformer.h:344`, `Reckless forward/scalar.rs:activate_ft()`）

| 変更箇所 | PSQT | Threat |
|----------|:----:|:------:|
| Feature Transformer 入力次元 | 変更なし | 変更なし |
| Feature Transformer 出力 | **PSQT チャネル追加** | 変更なし |
| Threat アキュムレータ | 不要 | **新規追加**（FT 出力と同次元 = 1536） |
| FT 出力への結合 | なし | **要素和**（活性化関数の前に加算） |
| L1 入力次元 | 変更なし | 変更なし |
| L2/Output | 変更なし | 変更なし |
| bucket 機構 | 変更なし | 変更なし |
| quantised.bin フォーマット | 拡張 | 拡張 |
| 既存モデルの再利用 | **不可** | **不可** |

## 実施順序

```
Phase 1: PSQT（実験 1）
├── bullet-shogi: PSQT 出力の追加
├── 学習: 既存データで再学習
├── rshogi: PSQT 読み込み + evaluate 加算
└── 評価: 1000 局 nodes=300K

Phase 2: 結果判断
├── PSQT 有効 → Phase 3 のベースに PSQT を含める
└── PSQT 無効 → without PSQT のまま進行

Phase 3: Threat（実験 2a → 2b）
├── 2a: 盤上のみ（チェス準拠）で実装・学習・評価
├── 2a 有効 → 2b（持ち駒考慮）も試行
└── 2a 無効 → 2b のみ試行（持ち駒が将棋で効く可能性）

Phase 4: 統合（実験 3）
└── Phase 1-3 の best 構成を組み合わせ
```

## 工数見積もり

| Phase | 作業 | bullet-shogi | rshogi | 学習時間 |
|-------|------|:----------:|:------:|:-------:|
| 1 PSQT | アーキテクチャ変更 + 推論 | 小 | 小 | 数日 |
| 3 Threat 2a | 盤上 Threat 特徴量 + SIMD 差分更新 | 中 | 中 | 1-2 週間 |
| 3 Threat 2b | 持ち駒拡張 | 中 | 中（2a の追加） | 数日（2a のモデルから継続学習可能） |
| 4 統合 | 組み合わせ | 小 | 小 | 数日 |

**注**: Threat は別アキュムレータ（FT 出力と同次元）を FT 出力に要素和する方式のため、
L1 層の変更は不要。rshogi 側の主な作業は Threat 特徴量抽出と差分更新の実装。

## 参照先の使い分け

PSQT と Threat で参照すべき実装が異なる。混同すると設計が崩れるため明確に分離する。

| 項目 | 仕様源 | Rust 実装参考 | 理由 |
|------|--------|---------------|------|
| **PSQT** | **Stockfish / nnue-pytorch のみ** | — (Reckless 不可) | Reckless の PstAccumulator は i16 ベクトル FT 入力であり、Stockfish 型の per-bucket i32 scalar shortcut とは別物 |
| **Threat 仕様** (特徴量次元, indexing, 除外ルール) | **Stockfish FullThreats** | — | Reckless は次元 (66,864) と除外ルールが Stockfish (60,144) と異なる |
| **Threat 実装技法** (Rust 構造化, 差分更新, SIMD) | — | **Reckless** | Rust + AVX2 の実装パターンとして有用 |

### Reckless PstAccumulator ≠ Stockfish PSQT

**この2つは名前が似ているが全く異なるもの**:

- **Stockfish PSQT**: FT から別に取り出した per-bucket の **i32 scalar** を最終段へ shortcut 加算
  （`nnue_feature_transformer.h:248`, `network.cpp:184`）
- **Reckless PstAccumulator**: 主 FT 入力を作る **i16 ベクトル accumulator** で、最終段 shortcut ではない
  （`psq.rs:27`, `nnue.rs:221`）

本設計の PSQT は Stockfish 型を採用する。Reckless の PST は別アーキテクチャへの組み替えになるため
Phase 1 の実験には不適。Reckless 方式は実験 4 として 1-3 完了後に別系統で検証する。

**実験 4 に進む判断基準**:
- Stockfish 型 PSQT shortcut の効果が限定的だった場合
- piece/threat 分離アキュムレータ自体に可能性があると考える根拠が出た場合
- 「PSQT の入れ方比較」ではなく「別 NNUE 設計比較」として結果を扱えること

### Stockfish と Reckless の Threat 実装差異

高レベルの流れ（別アキュムレータ → FT 活性化前に要素和）は共通だが、
特徴量定義は一致しない:

| | Stockfish | Reckless |
|---|---|---|
| Threat 次元 | **60,144** (`full_threats.h:45`) | **66,864** (`nnue.rs:281`) |
| King 行の除外 | **全除外** (`full_threats.h:60`) | 除外なし (`threat_index.rs:35`) |
| 差分: 66,864 − 60,144 = 6,720 | — | King 関連ペアを含む分 |

将棋版の Threat 設計では Stockfish の除外ルールを基準にしつつ、
将棋固有の駒種（成駒、香車等）に合わせて調整する。

## Threat 参照コード・資料

### Reckless（Rust 実装技法の参考）

参考にしてよい部分:
- Rust での accumulator 構造化、差分更新の持ち方
- AVX2 mailbox ベースの SIMD 化
- `threats.rs` / `vectorized/avx2.rs` のコードパターン

参考にしてはいけない部分:
- 特徴量の次元・indexing（Stockfish と異なる）
- 重複除去ルール（King 除外の有無が違う）

**ファイル**:
- `/mnt/nvme1/development/Reckless/src/nnue/accumulator/threats.rs`
- `/mnt/nvme1/development/Reckless/src/nnue/accumulator/threats/vectorized/avx2.rs`
- `/mnt/nvme1/development/Reckless/src/board.rs:627-636` — mailbox SIMD 一括ロード

### Stockfish（仕様の基準）

**ファイル**:
- [SFNNv10 Full Threat Inputs コミット](https://github.com/official-stockfish/Stockfish/commit/8e5392d79a36aba5b997cf6fb590937e3e624e80)
- [nnue-pytorch Full Threats ドキュメント](https://github.com/official-stockfish/nnue-pytorch/blob/master/docs/nnue.md#full_threats-feature-set)
- [Stockfish half_ka_v2_hm.h](https://github.com/official-stockfish/Stockfish/blob/master/src/nnue/features/half_ka_v2_hm.h)
- `/mnt/nvme1/development/Stockfish/src/nnue/features/full_threats.h` — 次元定義・除外ルール
- `/mnt/nvme1/development/Stockfish/src/nnue/nnue_feature_transformer.h:344` — 要素和結合

### チェス版 Full Threats の概要

- 「駒 A が駒 B を攻撃している」というペア関係を特徴量化
- 視点ごとに水平ミラーリング
- 冗長な組み合わせを削除:
  - 飛車が女王を攻撃 → 女王も飛車を攻撃（双方向で冗長）
  - 同種駒同士のペアは片方のみ
  - **King 行は全除外**（Stockfish 固有、Reckless は含む）
- i8 量子化で帯域幅削減

## 将棋固有の考慮事項

### 持ち駒

盤上にないため通常の Threat に含まれないが、将棋では最も重要な戦力。
実験 2b で「打ち込み利きマスク」として特徴量化を検討。

### 成駒

成りで利きパターンが大きく変化（例: 桂→成桂で金と同じ利き）。
Threat の差分更新で成り前後の利き変化を正しく反映する必要がある。
成小駒（成歩・成香・成桂・成銀）は金と同じ利きなので統合可能。

### 香車

前方のみの片方向レイ。チェスのルーク/ビショップとは異なるパターン。
8 方向レイのうち 1 方向のみに香車のスライダー効果。

### マス数・駒種

81 マス × 14 駒種で特徴量次元がチェス（64 × 6）より大きい。
成駒を統合する等の圧縮を検討。

### 冗長な特徴量の削除（将棋版）

チェス版の冗長削除ルールを将棋に適用する場合:
- 飛車が角を攻撃 ↔ 角が飛車を攻撃: **双方向で異なる意味** を持つ（チェスの Q-R とは異なり、将棋では取った駒を持ち駒にできるため攻撃方向が重要）
- 同種駒ペア: 将棋では歩が最大 9 枚あり、歩同士の対面は頻出
- 王への利き: 理論上は「王手の脅威」として有用な可能性があるが、**v1 では Stockfish に合わせて除外**する。必要なら 2a 完了後の variant として再検討する

---

## Phase 1: PSQT 詳細設計

### 概要

Feature Transformer の出力に PSQT チャネル（バケット数分のスカラー値）を追加し、
L1〜Output の密結合層を迂回して最終評価値に直接加算するショートカットパスを実装する。

```
Feature Transformer
  ├── main accumulator [i16; 1536] × 2視点  （既存）
  └── psqt accumulator [i32; 9] × 2視点     （新規）

          main path                     psqt path
            │                              │
   SqrClippedReLU [u8; 1536]         select bucket
            │                              │
   L1→L2→Output [i32]                psqt_value [i32]
            │                              │
            └──────── + ───────────────────┘
                      │
               raw_score [i32]
                      │
               / fv_scale
                      │
               最終評価値
```

### 設計原理

**Stockfish SFNNv10 準拠**: PSQT を FT の追加出力チャネルとして実装するアプローチ。
差分更新に含まれるため推論コストはほぼゼロ。

**スケール整合**: PSQT の整数スケールを密結合ネットワークの出力スケール
（QA × QB = 127 × 64 = 8128）に合わせることで、単純な加算で結合可能。

**視点差分**: `psqt_value = (stm_psqt_acc[bucket] - nstm_psqt_acc[bucket]) / 2`
- Stockfish 準拠の `/2` 正規化を適用（nnue-pytorch docs, `nnue_feature_transformer.h:248`）
- バイアスは差分で相殺されるが、フォーマットの統一性のため保持

**`/2` の根拠（実装時のコードコメント用）**:
PSQT アキュムレータは全駒の寄与を視点ごとに累積する。各駒は両視点に
逆符号で寄与するため、`stm - nstm` は正味の配置価値を約2倍にカウントする。

```
例: STM の銀が盤上にいる場合
  STM 視点: 「味方の銀が X」 → +v
  NSTM 視点: 「敵の銀が Y」 → −v (概ね)
  stm - nstm への寄与: (+v) − (−v) = 2v
```

`/2` はこの二重カウントを補正する正規化。将棋でも HalfKA_hm の構造
（各駒が両視点の特徴量を生成、手駒も同様）はチェスと同一のため正しい。
省略しても学習が吸収するが、リファレンス実装とのスケール一致のため含める。

---

### 量子化パラメータ

| パラメータ | 値 | 備考 |
|------------|-----|------|
| PSQT バケット数 | 9 | LayerStack と同一 |
| PSQT 重み型 | i32 | i16 だとオーバーフローリスクあり |
| PSQT 重みスケール | 8128 (= QA × QB) | 密結合出力と同一スケール |
| PSQT バイアス型 | i32 | |
| PSQT バイアススケール | 8128 (= QA × QB) | |
| PSQT accumulator 型 | i32 | ~40 特徴 × i32 重み → 安全にi32に収まる |

**スケール一致の根拠**:
```
密結合ネットワーク raw_score ≈ 8128 × float_output
PSQT int_value = (8128 × float_psqt_stm - 8128 × float_psqt_nstm) / 2
              = 8128 × float_psqt_diff / 2
→ PSQT 重みスケール 8128 × /2 で、結果は密結合出力と同等のオーダー
→ score = (raw_score + psqt_value) / fv_scale で統一的に処理
```

---

### bullet-shogi 側の変更

#### 対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `examples/shogi_layerstack.rs` | PSQT 層追加、forward pass 変更、export 変更 |

#### 1. ネットワーク定義の変更

```rust
// 既存
let l0 = builder.new_affine("l0", input_size, ft_out);
// ...

// 追加: PSQT 層（FT と同じ入力、出力 = バケット数）
// 学習初期に「PSQTなし」と等価にするため、weights/bias を両方 Zeroed で開始する。
let psqt_layer = Affine {
    weights: builder.new_weights(
        "psqtw",
        Shape::new(NUM_BUCKETS, input_size),
        InitSettings::Zeroed,
    ),
    bias: builder.new_weights(
        "psqtb",
        Shape::new(NUM_BUCKETS, 1),
        InitSettings::Zeroed,
    ),
}; // 73305 → 9
```

**命名規則**: `new_affine("psqt", ...)` を使った場合でも acyclib は
`psqtw` / `psqtb` という ID を生成する (`builder.rs:111-114`)。
ただし今回は zero-init が必要なため、`Affine` を手動構築して ID を明示する。

#### 2. Forward pass の変更

```rust
// 既存の密結合パス（変更なし）
let stm = l0.forward(stm_inputs).crelu().pairwise_mul() * (127.0 / 128.0);
let ntm = l0.forward(ntm_inputs).crelu().pairwise_mul() * (127.0 / 128.0);
let combined = stm.concat(ntm);

let l1_out_t = l1.forward(combined).select(output_buckets) + l1f.forward(combined);
let l1_main = l1_out_t.slice_rows(0, l1_effective_c);
let l1_skip = l1_out_t.slice_rows(l1_effective_c, l1_out_c);

let l1_sqr = l1_main.abs_pow(2.0) * (127.0 / 128.0);
let l2_input_tensor = l1_sqr.concat(l1_main).crelu();
let l2_out_t = l2.forward(l2_input_tensor).select(output_buckets).crelu();
let l3_out = l3.forward(l2_out_t).select(output_buckets);
let net_output = l3_out + l1_skip;

// 追加: PSQT ショートカット (Stockfish 準拠: (stm - nstm) / 2)
let stm_psqt = psqt_layer.forward(stm_inputs);         // [9]
let ntm_psqt = psqt_layer.forward(ntm_inputs) * (-1.0); // [9] 符号反転
let psqt_diff = (stm_psqt + ntm_psqt).select(output_buckets) * 0.5; // scalar, /2

// 最終出力 = 密結合出力 + PSQT
net_output + psqt_diff
```

**注意**:
- bullet の `GraphBuilderNode` は `Mul<f32>` を実装しており、任意の `f32` スカラー倍をサポートする
  （`crates/acyclib/src/graph/builder/node.rs:91-120`）。
- `* (-1.0)` と `* 0.5` は既存の WRM loss 実装でも使われており、未検証事項ではない
  （`examples/shogi_layerstack.rs:1399-1402`）。
- 減算は `* (-1.0)` + `+` で代替できる。

#### 3. 量子化 export の変更

PSQT 重みを FT 重みと LayerStack 重みの間に挿入:

```rust
// FT weights/biases (LEB128 compressed) — 既存
ft_biases_leb128,
ft_weights_leb128,
// PSQT weights/biases (raw i32) — 新規
psqt_data,
// LayerStack per-bucket data — 既存
layerstack_data,
```

PSQT export の実装:

```rust
let psqt_data = SavedFormat::empty()
    .transform(move |graph, _| {
        let psqt_w = graph.get("psqtw");  // [NUM_BUCKETS, input_size] column-major
        let psqt_b = graph.get("psqtb");  // [NUM_BUCKETS]

        let scale = (QA as i32 * QB as i32) as f64; // 8128.0
        let mut bytes: Vec<u8> = Vec::new();

        // Biases: i32[9]
        for bucket in 0..NUM_BUCKETS {
            let val = (scale * psqt_b.values[bucket] as f64).round() as i32;
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        // Weights: i32[73305][9] (feature-major)
        for feat in 0..input_size {
            for bucket in 0..NUM_BUCKETS {
                // column-major → feat * rows + bucket
                let w = psqt_w.values[feat * NUM_BUCKETS + bucket];
                let val = (scale * w as f64).round() as i32;
                bytes.extend_from_slice(&val.to_le_bytes());
            }
        }

        bytes.iter().map(|&b| (b as i8) as f32).collect()
    })
    .quantise::<i8>(1);
```

#### 4. アーキテクチャ文字列の変更

```rust
// 現行:
// "Features=HalfKA_hm(Friend)[73305->1536x2],Network=..."
// 変更後:
// "Features=HalfKA_hm(Friend)[73305->1536x2],PSQT=9,Network=..."
```

rshogi 側で `"PSQT="` の存在を検出し、PSQT 読み込みの有無を判定する。

---

### rshogi 側の変更

#### 対象ファイル

| ファイル | 変更内容 |
|----------|----------|
| `accumulator_layer_stacks.rs` | PSQT accumulator フィールド追加 |
| `feature_transformer_layer_stacks.rs` | PSQT 重み/バイアス読み込み、差分更新に PSQT 追加 |
| `network_layer_stacks.rs` | evaluate() で PSQT 加算、アーキテクチャ文字列解析 |
| `constants.rs` | PSQT 関連定数（必要に応じて） |

#### 1. AccumulatorLayerStacks の拡張

```rust
// accumulator_layer_stacks.rs
pub struct AccumulatorLayerStacks {
    /// 各視点の累積値 [perspective][dimension]
    pub accumulation: [[i16; NNUE_PYTORCH_L1]; 2],          // 既存

    /// PSQT アキュムレータ [perspective][bucket]
    pub psqt_accumulation: [[i32; NUM_LAYER_STACK_BUCKETS]; 2], // 新規: [2][9]

    pub computed_accumulation: bool,
    pub computed_score: bool,
}
```

**メモリ影響**: +72 bytes/accumulator（[i32; 9] × 2 = 72B）。
主アキュムレータ（6144B）に比べ約1.2%の増加。AccumulatorStack（MAX_PLY分）でも
72 × 256 = 18KB 追加のみ。

**Stack への影響**:
`StackEntryLayerStacks` は `AccumulatorLayerStacks` を内包しているため、
ply ごとの `psqt_accumulation` 保持はこのフィールド追加だけで達成される。
別の `psqt_stack` は不要。
ただし `push()` / `get_prev_and_current_accumulators()` / `source_acc.clone()` など、
アキュムレータ全体をコピーする経路は enlarged accumulator 前提でレビューする。

#### 2. FeatureTransformerLayerStacks の拡張

```rust
// feature_transformer_layer_stacks.rs
pub struct FeatureTransformerLayerStacks {
    pub biases: Aligned<[i16; NNUE_PYTORCH_L1]>,     // 既存
    pub weights: AlignedBox<i16>,                      // 既存

    /// PSQT バイアス [NUM_LAYER_STACK_BUCKETS]
    pub psqt_biases: [i32; NUM_LAYER_STACK_BUCKETS],   // 新規

    /// PSQT 重み [HALFKA_HM_DIMENSIONS × NUM_LAYER_STACK_BUCKETS]
    /// レイアウト: psqt_weights[feature_idx * 9 + bucket]
    pub psqt_weights: AlignedBox<i32>,                 // 新規

    /// PSQT が有効か（アーキテクチャ文字列で判定）
    pub has_psqt: bool,                                // 新規
}
```

**メモリ影響**: PSQT 重み = 73,305 × 9 × 4 = 2,638,980 bytes ≈ 2.5 MB。
主 FT 重み（73,305 × 1536 × 2 ≈ 225 MB）に比べ約1.1%の増加。

#### 3. ファイル読み込みの変更

```rust
// network_layer_stacks.rs の read() メソッド

// アーキテクチャ文字列を解析
let has_psqt = arch_str.contains("PSQT=");

// Feature Transformer 読み込み（既存）
let feature_transformer = FeatureTransformerLayerStacks::read_leb128(reader)?;

// PSQT 読み込み（新規、has_psqt の場合のみ）
let (psqt_biases, psqt_weights) = if has_psqt {
    read_psqt(reader)?  // i32 raw bytes
} else {
    default_psqt()      // ゼロ初期化
};

// LayerStacks 読み込み（既存）
let layer_stacks = LayerStacks::read(reader)?;
```

PSQT 読み込み関数:

```rust
fn read_psqt<R: Read>(reader: &mut R) -> io::Result<([i32; 9], AlignedBox<i32>)> {
    let mut biases = [0i32; NUM_LAYER_STACK_BUCKETS];
    let mut buf4 = [0u8; 4];

    // Biases: i32[9]
    for bias in biases.iter_mut() {
        reader.read_exact(&mut buf4)?;
        *bias = i32::from_le_bytes(buf4);
    }

    // Weights: i32[73305 × 9]
    let weight_count = HALFKA_HM_DIMENSIONS * NUM_LAYER_STACK_BUCKETS;
    let mut weights = AlignedBox::new_zeroed(weight_count);
    for w in weights.iter_mut() {
        reader.read_exact(&mut buf4)?;
        *w = i32::from_le_bytes(buf4);
    }

    Ok((biases, weights))
}
```

#### 4. 差分更新の変更

**refresh_accumulator (フル再計算)**:

```rust
// 既存の主アキュムレータ計算
acc.accumulation[perspective] = self.biases;
for &feature_idx in active_indices {
    self.add_weights(&mut acc.accumulation[perspective], feature_idx);
}

// PSQT アキュムレータ（新規）
if self.has_psqt {
    acc.psqt_accumulation[perspective] = self.psqt_biases;
    for &feature_idx in active_indices {
        let offset = feature_idx * NUM_LAYER_STACK_BUCKETS;
        for bucket in 0..NUM_LAYER_STACK_BUCKETS {
            acc.psqt_accumulation[perspective][bucket] +=
                self.psqt_weights[offset + bucket];
        }
    }
}
```

**update_accumulator (差分更新)**:

```rust
// 既存の主アキュムレータ差分
acc.accumulation[perspective] = prev_acc.accumulation[perspective];
for &removed in removed_indices {
    self.sub_weights(&mut acc.accumulation[perspective], removed);
}
for &added in added_indices {
    self.add_weights(&mut acc.accumulation[perspective], added);
}

// PSQT 差分（新規）
if self.has_psqt {
    acc.psqt_accumulation[perspective] = prev_acc.psqt_accumulation[perspective];
    for &removed in removed_indices {
        let offset = removed * NUM_LAYER_STACK_BUCKETS;
        for bucket in 0..NUM_LAYER_STACK_BUCKETS {
            acc.psqt_accumulation[perspective][bucket] -=
                self.psqt_weights[offset + bucket];
        }
    }
    for &added in added_indices {
        let offset = added * NUM_LAYER_STACK_BUCKETS;
        for bucket in 0..NUM_LAYER_STACK_BUCKETS {
            acc.psqt_accumulation[perspective][bucket] +=
                self.psqt_weights[offset + bucket];
        }
    }
}
```

**性能**: 9 バケット × 変更特徴数（通常 1〜4）= 最大 36 回の i32 加減算。
主アキュムレータの 1536 × (1〜4) 回の i16 加減算に比べ無視できるコスト。

#### 5. forward_update_incremental の変更

`forward_update_incremental()` は複数手分の差分を祖先ノードからまとめて適用するパス。
`update_accumulator()` と同様に PSQT も更新する必要がある。

```rust
pub fn forward_update_incremental(
    &self,
    pos: &Position,
    stack: &mut AccumulatorStackLayerStacks,
    source_idx: usize,
) -> bool {
    let Some(path) = stack.collect_path(source_idx) else {
        return false;
    };

    // source_acc には main accumulation と psqt_accumulation の両方が入る。
    let source_acc = stack.entry_at(source_idx).accumulator.clone();
    stack.current_mut().accumulator = source_acc;

    for entry_idx in path.iter() {
        let dirty_piece = stack.entry_at(entry_idx).dirty_piece;

        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let king_sq = pos.king_square(perspective);
            let mut removed = IndexList::new();
            let mut added = IndexList::new();
            append_changed_indices(
                &dirty_piece,
                perspective,
                king_sq,
                &mut removed,
                &mut added,
            );

            let current = &mut stack.current_mut().accumulator;

            // main accumulation
            let fast_applied = self.try_apply_dirty_piece_fast(
                current.get_mut(p),
                &dirty_piece,
                perspective,
                king_sq,
            );
            if !fast_applied {
                for index in removed.iter() {
                    self.sub_weights(current.get_mut(p), index);
                }
                for index in added.iter() {
                    self.add_weights(current.get_mut(p), index);
                }
            }

            // PSQT accumulation
            // try_apply_dirty_piece_fast は main path 専用なので、
            // PSQT は removed/added を必ず明示的に適用する。
            if self.has_psqt {
                for index in removed.iter() {
                    let offset = index * NUM_LAYER_STACK_BUCKETS;
                    for bucket in 0..NUM_LAYER_STACK_BUCKETS {
                        current.psqt_accumulation[p][bucket] -= self.psqt_weights[offset + bucket];
                    }
                }
                for index in added.iter() {
                    let offset = index * NUM_LAYER_STACK_BUCKETS;
                    for bucket in 0..NUM_LAYER_STACK_BUCKETS {
                        current.psqt_accumulation[p][bucket] += self.psqt_weights[offset + bucket];
                    }
                }
            }
        }
    }

    stack.current_mut().accumulator.computed_accumulation = true;
    stack.current_mut().accumulator.computed_score = false;
    true
}
```

#### 6. evaluate() の変更

```rust
// network_layer_stacks.rs
pub fn evaluate_with_bucket(
    &self,
    pos: &Position,
    acc: &AccumulatorLayerStacks,
    bucket_index: usize,
) -> Value {
    let side_to_move = pos.side_to_move();

    // SqrClippedReLU 変換 + 密結合ネットワーク（既存）
    let (us_acc, them_acc) = ...;
    let mut transformed = ...;
    sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
    let raw_score = self.layer_stacks.evaluate_raw(bucket_index, &transformed.0);

    // PSQT ショートカット（新規、Stockfish 準拠: (stm - nstm) / 2）
    let psqt_value = if self.feature_transformer.has_psqt {
        let stm = side_to_move as usize;
        let nstm = (!side_to_move) as usize;
        (acc.psqt_accumulation[stm][bucket_index]
            - acc.psqt_accumulation[nstm][bucket_index]) / 2
    } else {
        0
    };

    let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
    Value::new((raw_score + psqt_value) / fv_scale)
}
```

#### 7. Finny Tables キャッシュの変更

**実装判断: PSQT は Finny Tables に含めない。**

PSQT の再計算コストは 40特徴 × 9バケット = 360 回の i32 加算で、
主アキュムレータ（1536 × i16 SIMD）と比較して無視できる。
キャッシュ復元パス（`refresh_perspective_with_cache`）で PSQT はフル再計算する。
`AccCacheEntry` の肥大化を避け、キャッシュ API のシンプルさを維持する。

---

### ファイルフォーマット

#### 変更後のレイアウト

```
[Header]
  version: u32 (0x7AF32F20 — LayerStack 用, shogi_layerstack.rs:993)
  network_hash: u32 (変更: PSQT を含むハッシュ)
  desc_len: u32
  description: "Features=HalfKA_hm(Friend)[73305->1536x2],PSQT=9,Network=..."

[FT layer hash: u32]
[FT biases:  LEB128 compressed i16[1536]]
[FT weights: LEB128 compressed i16[73305 × 1536]]

[PSQT biases:  raw LE i32[9]]                          ← 新規 (36 bytes)
[PSQT weights: raw LE i32[73305 × 9]]                  ← 新規 (~2.5 MB)
  レイアウト: weights[feature_index * 9 + bucket]

[Per bucket × 9]:                                       ← 既存（変更なし）
  [fc_hash: u32]                                          (各バケット先頭にハッシュ)
  [L1 biases:   i32[16]]
  [L1 weights:  i8[16 × pad32(1536)]]
  [L2 biases:   i32[32]]
  [L2 weights:  i8[32 × pad32(30)]]
  [Output bias: i32[1]]
  [Output weights: i8[pad32(32)]]
```

**注**: `0x7AF32F16` は HalfKP/非 LayerStack 用。LayerStack は `0x7AF32F20`
（`rshogi constants.rs:60`, `bullet-shogi shogi_layerstack.rs:993`）。
ヘッダの `network_hash` は既存フォーマットに含まれており、独立した `[Network hash]`
ブロックは存在しない。各バケット先頭の `fc_hash` がバケット単位のハッシュ。

#### ハッシュ計算の扱い

現行 `bullet-shogi` の LayerStack writer は以下の式でヘッダハッシュを作っている
（`examples/shogi_layerstack.rs:963-965`）:

```rust
let fc_hash = compute_layerstack_fc_hash(ft_out, l2_in, l2_out);
let ft_hash = FEATURE_HASH_HM_V2 ^ ((ft_out * 2) as u32);
let network_hash = fc_hash ^ ft_hash;
```

PSQT 追加時の注意点:
- `fc_hash` は FC 段の shape に依存するため、PSQT shortcut 追加だけなら通常は変わらない
- `ft_hash` も現行式のままだと PSQT の有無を表現しない
- ただし **現状の rshogi LayerStack reader は header `network_hash` も per-bucket `fc_hash` も検証していない**
  ため、これは互換性上の blocker ではない

実装方針:
- **Phase 1 実装の unblock 観点では、arch string の `PSQT=9` と PSQT ブロック追加だけで十分**
- 将来ハッシュ検証を入れるなら、その時点で `PSQT_HASH_SALT` などの固定値を導入し、
  writer / reader / ドキュメントで一括管理する

**後方互換性**: アーキテクチャ文字列に `"PSQT="` が含まれない場合、
PSQT ブロックを読み飛ばし既存動作と完全互換。

**ファイルサイズ増加**: +2.5 MB（PSQT 重みは未圧縮。FT 本体と比較して微小）

---

### 学習手順

1. **bullet-shogi に PSQT 層を追加** (上記の変更)
2. **既存データで from-scratch 学習** (PSQT 付きモデルは既存モデルから継続学習不可)
   - 学習設定は現行と同一（SCReLU/pairwise, AdamW, 同一 lr schedule）
   - PSQT 層は `weights` / `bias` とも `InitSettings::Zeroed` で初期化
     （`new_affine()` ではなく `Affine { new_weights(..., Zeroed), ... }` を使う）
3. **quantised.bin を export** (PSQT 付きフォーマット)
4. **rshogi で読み込み・評価テスト**
   - Golden Forward テスト: **quantised.bin を読んだ整数 forward** を参照値として一致確認
   - bullet / Python の float forward は bit-exact 参照ではなく、値のドリフト切り分け用
   - 初期局面の PSQT 値が妥当か確認

### 評価方法

- 1000 局 nodes=300K の自己対局で PSQT あり vs なしを比較
- 同一学習データ・同一 epoch 数で公平比較

### Golden Forward テスト手順

bit-exact 比較は **float forward ではなく quantised.bin ベースの整数経路** を使う。
実装前に参照値生成方法を固定しておく。

1. `bullet-shogi/examples/shogi_layerstack_eval.rs` を PSQT 対応し、
   `quantised.bin` から以下を読めるようにする
   - `psqt_biases`
   - `psqt_weights`
   - 必要なら sample check（`psqtw` / `psqtb` vs `weights.bin`）
2. 同ツールで固定 `packed_sfen` 1件に対し、少なくとも以下を出力する
   - bucket index
   - `stm_psqt_acc[bucket]`
   - `nstm_psqt_acc[bucket]`
   - `psqt_value = (stm - nstm) / 2`
   - `raw_score`（LayerStack 本体）
   - `raw_score + psqt_value`
3. `rshogi` 側は `diagnostics` を使って同じ局面・同じ `quantised.bin` で以下を出力する
   - bucket index
   - `psqt_accumulation[stm][bucket]`
   - `psqt_accumulation[nstm][bucket]`
   - `psqt_value`
   - `raw_score`
   - 最終 score
4. 比較は次の順に行う
   - export bytes の sample check
   - `psqt_value` の一致
   - `raw_score` の一致
   - `raw_score + psqt_value` の一致
   - 最終評価値の一致

**重要**:
- bullet / Python の float forward は量子化誤差を含むため、bit-exact 参照には使わない
- float dump は「どの段でズレ始めたか」の切り分け用としてのみ使う

---

### 実装チェックリスト

#### bullet-shogi

- [ ] `shogi_layerstack.rs`: `psqt` 層の追加（weights/bias とも Zeroed）
- [ ] `shogi_layerstack.rs`: forward pass に PSQT ショートカット追加
- [ ] `shogi_layerstack.rs`: export に PSQT biases/weights 追加
- [ ] `shogi_layerstack.rs`: アーキテクチャ文字列に `PSQT=9` 追加
- [ ] network hash の扱いを決定（現状 reader 未検証のため非 blocker）
- [ ] 学習テスト: loss が正常に下がることを確認

#### rshogi

- [ ] `constants.rs`: PSQT 関連定数（必要に応じて）
- [ ] `accumulator_layer_stacks.rs`: `psqt_accumulation` フィールド追加
- [ ] `feature_transformer_layer_stacks.rs`: PSQT 重み/バイアスフィールド追加
- [ ] `feature_transformer_layer_stacks.rs`: PSQT ファイル読み込み
- [ ] `feature_transformer_layer_stacks.rs`: refresh_accumulator で PSQT 計算
- [ ] `feature_transformer_layer_stacks.rs`: update_accumulator で PSQT 差分更新
- [ ] `feature_transformer_layer_stacks.rs`: forward_update_incremental で PSQT 差分更新
- [ ] `accumulator_layer_stacks.rs`: StackEntry / Stack copy path が enlarged accumulator を正しく扱うことを確認
- [ ] `network_layer_stacks.rs`: アーキテクチャ文字列から PSQT 検出
- [ ] `network_layer_stacks.rs`: evaluate() で PSQT 加算
- [ ] ~~`accumulator_layer_stacks.rs`: AccCacheEntry に PSQT 追加~~ → 不採用（フル再計算で十分）
- [ ] `network_layer_stacks.rs`: diagnostics で positional と psqt を分離出力
- [ ] PSQT なしモデルの後方互換テスト
- [ ] PSQT ありモデルの Golden Forward テスト

---

### Stockfish / Reckless との設計比較

| 項目 | rshogi PSQT (本設計) | Stockfish PSQT | Reckless PST |
|------|---------------------|----------------|--------------|
| 役割 | 最終段 shortcut | 最終段 shortcut | **FT 入力の i16 ベクトル** |
| 出力型 | per-bucket i32 scalar | per-bucket i32 scalar | i16[768] ベクトル |
| 最終評価への結合 | `(stm-nstm)/2` 加算 | `(stm-nstm)/2` 加算 | Threat と要素和 → Hadamard 積 |
| 独立性 | 密結合と完全独立 | 密結合と完全独立 | Threat と密結合 |

**Reckless の PST は Stockfish 型 PSQT とは別物**（詳細は「参照先の使い分け」節参照）。
本設計は Stockfish SFNNv10 に準拠する。

---

## Phase 3: Threat 2a に進む前の補足仕様

### 先に固定すべきこと

Threat 2a は大枠の方向性だけでは実装に着手しにくい。特に次の 3 点は先に固定する必要がある。

1. **駒種の圧縮単位**
   - `rshogi` の `DirtyPiece` / `ExtBonaPiece` / `PieceList` は、既に
     `Gold + {ProPawn, ProLance, ProKnight, ProSilver}` を同一 BonaPiece として扱う
     （`PIECE_BASE` が Gold と成小駒を同じ base に割り当てている）。
   - したがって Threat でもこの粒度を超えて Gold と成小駒を区別すると、
     既存の `DirtyPiece` だけでは差分更新に必要な旧/新状態を復元できない。

2. **bullet-shogi の入力 API 制約**
   - `ValueTrainerBuilder::build()` が学習グラフに供給する sparse input は
     `stm` / `ntm` の 1 組のみ。
   - Threat を別の sparse input node として増やすには dataloader / builder 側の拡張が必要。
   - v1 は dataloader を拡張せず、**HalfKA_hm と Threat を 1 つの連結 sparse input として扱う**。

3. **`board_effect` 依存禁止**
   - `layerstack-only` ビルドでは `Position::should_update_board_effects()` が `false` になり、
     `board_effects` は差分更新されない。
   - Threat 差分は `board_effect()` / `board_effects()` 前提で設計せず、
     `piece_on()` / `occupied()` / `attackers_to_occ()` / 各種 effect table から再構成する。

### Threat 2a v1 の固定方針

**結論**: Threat 2a の v1 は、**王を除いた 9 クラスの盤上駒 family** を使う。
Stockfish の冗長削除ルールを最初から全面移植するのではなく、
**まずは「王行・王列を除外するだけ」の単純版**で実装し、サイズや NPS に問題が出たら
測定結果を見て pruning を追加する。

#### Threat class（v1）

| Threat class | 含まれる `PieceType` |
|-------------|----------------------|
| Pawn | `Pawn` |
| Lance | `Lance` |
| Knight | `Knight` |
| Silver | `Silver` |
| GoldLike | `Gold`, `ProPawn`, `ProLance`, `ProKnight`, `ProSilver` |
| Bishop | `Bishop` |
| Rook | `Rook` |
| Horse | `Horse` |
| Dragon | `Dragon` |

**除外**:
- attacker = `King` は含めない
- attacked = `King` も含めない
- 2a では手駒・打ち込み脅威は含めない（2b で別扱い）

#### この粒度を採用する理由

- `DirtyPiece` から復元できる情報粒度と一致する
- `bona_piece.rs` / `PIECE_BASE` の既存分類と整合する
- Gold と成小駒を Threat 側だけで分けるために `DirtyPiece` や `PieceList` を広げる必要がない
- YAGNI に沿う。まずは既存の差分更新経路を壊さない最小追加で進められる

### Threat 次元とサイズ感

盤幾何から空盤面上の attack count を数えると、1 色あたりの cumulative attack offset は次になる。

| class | cumulative attacks / color |
|------|----------------------------:|
| Pawn | 72 |
| Lance | 324 |
| Knight | 112 |
| Silver | 328 |
| GoldLike | 416 |
| Bishop | 816 |
| Rook | 1296 |
| Horse | 1104 |
| Dragon | 1552 |
| **合計** | **6020** |

v1 は attacker color 2 通り、attacked side 2 通り、target class 9 通りなので:

```text
THREAT2A_DIMENSIONS = 2 * 6020 * (2 * 9) = 216,720
```

サイズ感の比較:

| 案 | 次元 | 量子化後 i8 重みサイズ | 学習時 fp32 重みサイズ | 判定 |
|----|-----:|-----------------------:|-----------------------:|------|
| 14駒種そのまま（King 除外） | 399,568 | 約 585 MiB | 約 2.29 GiB | v1 では重すぎる |
| **9 family（v1）** | **216,720** | **約 318 MiB** | **約 1.24 GiB** | **採用** |
| 9 family + pruning | 約 198,000 | 約 290 MiB | 約 1.13 GiB | 後で測定してから |

**注**:
- 上の「9 family + pruning」は、Stockfish 風の subset-rule や same-type dedupe を足した場合の
  目安であり、v1 の必須要件ではない。
- Threat は i8 量子化するので、推論ファイルサイズは fp32 学習サイズより大幅に小さい。

### 正規化と index 方針

Threat 2a の index は **King bucket 45 個** には依存させない。依存するのは
**視点変換 + Half-Mirror の有無だけ**とする。

#### 正規化

```text
1. 視点正規化:
   sq_p = if perspective == Black { sq } else { sq.inverse() }

2. Half-Mirror:
   hm = is_hm_mirror(king_sq, perspective)
   sq_n = if hm { sq_p.mirror() } else { sq_p }

3. side 正規化:
   side_n = 0 (stm) / 1 (nstm)
```

#### index 構造

```text
index =
    pair_base[attacker_side][attacker_class][attacked_side][attacked_class]
  + from_offset[attacker_class][from_sq_n]
  + attack_order[attacker_class][from_sq_n][to_sq_n]
```

v1 では **same-type dedupe も subset-rule も入れない**。
理由は次の通り:

- まずは `append_active_indices()` / `append_changed_indices()` の正しさを優先したい
- 冗長削除はサイズ最適化であり、測定前に複雑化させるべきではない
- 将棋ではチェスより攻防・持ち駒変換の意味が異なるため、Stockfish の pruning をそのまま入れると
  仕様検証に時間がかかる

### refresh 条件

Threat 2a は king square そのものではなく `hm_mirror` のみを使うため、
full refresh が必要なのは **この視点の王が mirror 境界を跨いだときだけ**。

```text
needs_refresh(perspective) =
    is_hm_mirror(prev_king_sq[perspective], perspective)
 != is_hm_mirror(curr_king_sq[perspective], perspective)
```

つまり:
- 王が同じ Half-Mirror 側の中で動く: 差分更新可能
- 5筋側 ↔ 6-9筋側を跨ぐ: full refresh

Stockfish の「d-e file を跨いだときだけ refresh」と同じ考え方を、将棋の 9x9 / 5筋 mirror に
置き換えたもの。

### 差分更新の基本方針

Stockfish のように `DirtyThreats` を `Position::do_move()` から配線する方法もあるが、
v1 ではそこまで広げない。既存の `DirtyPiece` と current `Position` から、
NNUE 側で局所的に before/after を再構成する。

#### v1 で追加する helper

- `decode_board_ext_bonapiece(...) -> Option<(Color, ThreatClass, Square)>`
  - `ExtBonaPiece::ZERO` と手駒は `None`
  - Gold / 成小駒は `GoldLike` に丸める
- `piece_on_before_after(square, dirty_piece, pos)` 相当の helper
  - current `Position` を after とみなし、`DirtyPiece.changed_piece` から before/after の差を復元

#### changed threat 生成アルゴリズム

1. `DirtyPiece` から **盤上の changed square** を集める
   - old が盤上なら old square
   - new が盤上なら new square
   - 手駒 only の変化は 2a では無視
2. `before_occ` / `after_occ` を current `Position` + `DirtyPiece` から局所再構成する
3. 各 changed square `sq` について、次を source 候補に追加する
   - `sq` 上の駒（before / after）
   - `attackers_to_occ(sq, before_occ)`
   - `attackers_to_occ(sq, after_occ)`
4. source 候補それぞれについて
   - before の occupied-target 一覧
   - after の occupied-target 一覧
   を列挙し、set difference で `removed` / `added` を得る
5. 得られた threat index を `threat_accumulation` に add/sub する

この方法なら、**移動した駒の直接利き**だけでなく、**開き利きで変化した遠方駒の threat** も拾える。

#### v1 で defer するもの

- `Position::do_move()` からの `DirtyThreatList` 配線
- AVX2/AVX512 用の fused threat delta 生成
- `try_apply_dirty_piece_fast` に相当する threat 専用 fast path

### 初期容量の目安

Threat 2a は v1 では安全側に多めに取る。

| 定数 | 初期値 | 根拠 |
|------|-------:|------|
| `MAX_ACTIVE_THREAT_FEATURES` | 320 | 盤上 40 駒 × 1 駒あたり最大 8 occupied-target の安全側上限 |
| `MAX_CHANGED_THREAT_FEATURES` | 192 | changed square 周辺 source の再列挙 + 開き利き分を含めた余裕込み |

**必須**:
- debug build で active / changed の実測最大値をカウントする
- 実測が十分小さいと確認できてから定数を詰める

### bullet-shogi で補足すべき仕様

#### 学習用 input は「連結 sparse input」にする

`ValueTrainerBuilder` は sparse input を 1 組しか供給できないため、
Threat 2a は新しい `SparseInputType` を作って **HalfKA_hm + Threat の連結入力**にする。

```text
input_size = HALFKA_HM_DIMENSIONS + THREAT2A_DIMENSIONS
max_active = 40 + MAX_ACTIVE_THREAT_FEATURES
```

推奨ファイル:
- `bullet-shogi/crates/bullet_lib/src/game/inputs/shogi_halfka_hm_threat2a.rs`
- `bullet-shogi/crates/bullet_lib/src/game/inputs.rs` に re-export を追加

#### 学習グラフ

学習グラフでは 1 本の `l0` を使ってよい。線形性により、
推論時の「piece accumulator + threat accumulator」を
学習時の「連結 sparse input への 1 本の affine」と等価に扱える。

```text
l0w = [piece_rows ; threat_rows]
l0b = shared bias
```

量子化 export では `l0w` を行方向に分割して:
- piece rows → 既存 FT weights（i16）
- threat rows → Threat weights（i8）

として保存する。

#### 追加で固定する項目

- アーキテクチャ文字列に `Threat2A=216720` を入れる
- `l0.init_with_effective_input_size(...)` は Threat active 数を実測して再設定する
  - 既存の `32` をそのまま使う前提にはしない

### 将来の AB テストは別アーキテクチャとして扱う

`attacked = King` を入れるかどうかは、推論時の挙動だけでなく

- Threat 次元
- index table
- active / changed feature 数
- 学習入力
- export/import フォーマット
- arch 文字列

を一式変える。したがって、将来 AB テストする場合も **runtime feature flag ではなく別アーキテクチャ variant**
として扱う。

推奨:

- 現行 v1: `Threat2A`
- 比較 variant: `Threat2A_KingTarget`

内部実装は helper や差分更新骨格を共有してよいが、
**学習済みモデル・量子化ファイル・arch 文字列・実験名** は分離する。

### rshogi で補足すべき仕様

#### accumulator / weight 型

- `threat_accumulation: [[i16; NNUE_PYTORCH_L1]; 2]`
- `threat_weights: AlignedBox<i8>` または等価の 64-byte aligned 領域
- main accumulator と別保持し、活性化直前で要素和する

#### 読み込み・フォーマット

Threat 2a ありモデルの暫定フォーマット順:

```text
FT biases (i16, LEB128)
FT weights (i16, LEB128)
Threat weights (i8, raw or block-compressed)
LayerStack weights (既存)
```

`NetworkLayerStacks::read()` はアーキテクチャ文字列の `Threat2A=` を見て
Threat ブロックの有無を判定する。

#### キャッシュ

Finny Tables でも threat を別持ちする。

- `AccCacheEntry` に `threat_accumulation` を追加
- `refresh_or_cache()` で main accumulator と threat accumulator を両方コピー / diff 適用する

### テストと検証

Threat 2a に進む前に、少なくとも次をドキュメント化しておく。

1. **feature enumeration テスト**
   - 固定 SFEN で active threat index 一覧が stable であること
2. **refresh vs update 一致テスト**
   - same position を full refresh した結果と差分更新結果が一致すること
3. **cache path 一致テスト**
   - Finny 経由 refresh と通常 refresh が一致すること
4. **forward_update_incremental 一致テスト**
   - 祖先アキュムレータからの多段差分適用結果が直接 refresh と一致すること
5. **Golden Forward**
   - `quantised.bin` を参照値にして、piece path / threat path / 合算後 transformed / raw_score を照合すること
6. **容量ログ**
   - 学習データ or 実戦局面で `max_active_threats` / `max_changed_threats` の実測を採ること

### 2b（持ち駒考慮）はまだ block

2a がこの仕様で実装可能になっても、2b は別。
未確定なのは次:

- 「打てるか」のマスクを Threat に混ぜるか、別チャネルにするか
- 合法打ち判定（歩二歩・打ち歩詰め）を Threat 特徴量でどこまで扱うか
- hand-only の `DirtyPiece` 変化を Threat へどう反映するか

したがって、**まずは 2a を実装・学習・評価してから 2b を設計する**。

---

## ステータス

- 2026-04-03: 研究計画作成、Phase 1 PSQT 詳細設計完了
- 2026-04-03: Threat 2a の前提仕様（family 粒度、差分更新方針、bullet/rshogi 制約）を追記
- 現在のステータス: **Phase 1 PSQT は実装待ち、Phase 3 Threat 2a は v1 仕様が固まった**
- 次のアクション: Threat 2a の feature index helper と bullet-shogi の連結 input prototype を実装
