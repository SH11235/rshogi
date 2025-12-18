# パフォーマンス分析レポート

このドキュメントは、将棋エンジン（Rust実装）のパフォーマンス計測結果と最適化調査の記録です。

## 計測環境

| 項目 | 値 |
|------|-----|
| CPU | AMD Ryzen 9 5950X 16-Core Processor |
| コア数 | 32 |
| OS | Ubuntu (Linux 6.8.0) |
| アーキテクチャ | x86_64 |
| 計測日 | 2025-12-18 |

---

## ホットスポット一覧

### NNUE有効時（本番相当）

計測コマンド: `./scripts/perf_profile_nnue.sh`

| 順位 | 関数 | CPU% | 状態 | 備考 |
|------|------|------|------|------|
| 1 | `MovePicker::next_move` | 6.55% | 調査完了 | [詳細](#movepicker-調査完了) |
| 2 | `refresh_accumulator` | 6.40% | - | NNUE全計算 |
| 3 | `AffineTransform::propagate` | 5.59% | - | NNUE推論（隠れ層） |
| 4 | `attackers_to_occ` | 3.58% | - | 利き計算 |
| - | `partial_insertion_sort` | 2.32% | 調査完了 | MovePicker内部 |
| - | `score_quiets` | 1.07% | 調査完了 | MovePicker内部 |

**注**: kernelオーバーヘッド（`__fsnotify_parent` 4.41%, `dput` 3.46%）はNNUEファイル読み込み時のもので、実際の探索時間には影響しない。

#### NNUE関連の内訳

| 関数 | CPU% | 説明 |
|------|------|------|
| `refresh_accumulator` | 6.40% | Accumulator全計算（差分更新失敗時） |
| `AffineTransform::propagate` | 5.59% | 隠れ層の行列演算 |
| `add_weights` | ~1.0% | 特徴量の重み加算（refresh内） |

### Material評価時（NNUE無効、release build）

計測コマンド: `./scripts/perf_profile.sh`

| 順位 | 関数 | CPU% | 備考 |
|------|------|------|------|
| 1 | `eval_lv7_like` | 24.48% | Material評価のメイン関数 |
| 2 | `direction_of` | 14.57% | 方向計算 |
| 3 | `compute_board_effects` | 9.81% | 盤面効果計算 |
| 4 | `MovePicker::next_move` | 7.96% | 指し手選択 |
| 5 | `search_node` | 4.44% | 探索メインループ |
| 6 | `check_move_mate` | 4.11% | 1手詰め判定 |
| 7 | `do_move` | 3.44% | 指し手実行 |
| 8 | `__memset_avx2` | 3.13% | メモリ初期化 |
| 9 | `attackers_to_occ` | 2.87% | 利き計算 |
| 10 | `build_cont_tables` | 2.25% | Continuation History構築 |

**注**: Material評価は1回の評価計算は軽量だが、評価精度が低いため枝刈りの効率が悪く、NPSはNNUEと同等かそれ以下になることが多い。

---

## 調査完了項目

### MovePicker (調査完了)

**調査日**: 2025-12-18
**結論**: **最適化余地なし** - 現在の実装がYaneuraOu/Stockfishと同等で最適解

#### 背景

perfプロファイルで `MovePicker::next_move` が高いオーバーヘッド（6.63%）を示していたため、最適化の可能性を調査。

#### 内訳

| 関数 | CPU% | 役割 |
|------|------|------|
| `partial_insertion_sort` | 2.32% | 指し手のスコア順ソート |
| `score_quiets` | 1.07% | 静かな手のスコア計算 |
| その他（ステージ遷移等） | 3.16% | オーバーヘッド |

#### 検証した最適化候補

| 候補 | 結果 | 詳細 |
|------|------|------|
| A1. 選択ソート (`pick_best`) | 不採用 | 実装テスト済み: **-24% NPS**の大幅悪化。毎回O(n)走査でO(n²)複雑度 |
| A2. SIMD化 | 対象外 | YaneuraOuでコメントのみ、実装なし |
| A3. limit値調整 | 同一 | 現在の実装（`-3560 * depth`）はYaneuraOuと同一 |
| B1. 遅延評価 | 対象外 | YaneuraOuに実装なし |
| B2. History最適化 | 対象外 | YaneuraOuに実装なし |
| C1. ステージスキップ | 同一 | 現在の実装はYaneuraOuと同一 |

#### YaneuraOuとの比較

YaneuraOuの`movepick.cpp`より:
> 現状、全体時間の6.5〜7.5%程度をこの関数で消費している

**YaneuraOu/Stockfishも同様のオーバーヘッドを認識しているが、解決策を持っていない。**

#### 結論

`partial_insertion_sort`のオーバーヘッド（6-7%）は、指し手順序を適切に保つために必要なコストであり、これ以上の最適化余地はない。

---

### Bitboard256 AVX2 SIMD化 (調査完了)

**調査日**: 2025-12-19
**結論**: **AMD Zen 3環境では効果なし** - フィーチャーフラグで将来の検証用に残す

#### 背景

YaneuraOuではBitboard256（角の利き計算用256bit構造体）にAVX2 SIMD命令を使用している。本エンジンでも同様の最適化を検証。

#### 実装内容

| メソッド | AVX2命令 | 用途 |
|---------|---------|------|
| `BitAnd` | `_mm256_and_si256` | 論理AND |
| `BitOr` | `_mm256_or_si256` | 論理OR |
| `BitXor` | `_mm256_xor_si256` | 論理XOR |
| `new()` | `_mm256_broadcastsi128_si256` | 128bit→256bit複製 |
| `from_bitboards()` | `_mm256_inserti128_si256` | 2つの128bitを結合 |
| `byte_reverse()` | `_mm256_shuffle_epi8` | バイト順反転 |
| `merge()` | `_mm256_extracti128_si256` | 256bit→128bit統合 |

#### ベンチマーク結果

```
計測条件: MaterialLevel=9, Threads=1, movetime=20000ms, target-cpu=native
```

| 構成 | 平均NPS | 変化 |
|-----|--------|-----|
| スカラー版（デフォルト） | 446,587 | ベースライン |
| AVX2版（`--features simd_avx2`） | 442,411 | **-0.9%** |

#### アセンブリ分析

スカラー版とAVX2版で生成されるアセンブリを比較:

- **AVX2版**: `vpand`, `vpor`, `vpxor`, `vinserti128` 等のAVX2命令を使用
- **スカラー版**: `movq` を使用した64bit単位の処理（自動ベクトル化なし）

手動SIMD化は確かに異なるコードを生成しているが、パフォーマンス向上には繋がらなかった。

#### 効果がなかった理由の分析

1. **AMD Zen 3のスカラー性能**: 64bit演算が非常に高速で、AVX2の相対的優位性が小さい
2. **bishop_effectの寄与**: 探索全体に占める`attackers_to_occ`（bishop_effect含む）は3.58%のみ
3. **LLVMの最適化**: スカラーコードでもコンパイラが効率的なコードを生成

#### YaneuraOuとの違い

YaneuraOuでは効果があるとされているが、以下の違いが考えられる:

- **CPU環境**: Intel環境ではAVX2の相対効率が高い可能性
- **コンパイラ**: GCC/MSVCとLLVMで最適化特性が異なる
- **計測条件**: マイクロベンチマーク vs 探索全体のNPS

#### 結論

AMD Zen 3環境では効果なし。ただし、以下の理由でフィーチャーフラグ（`simd_avx2`）として残す:

- Intel環境での将来の検証
- マルチスレッド対応時にメモリ帯域幅がボトルネックになった場合の検証

```bash
# 使用方法
cargo build --release                    # デフォルト: スカラー版
cargo build --release --features simd_avx2  # AVX2版
```

---

## 計測方法

### 前提条件

- Linux環境
- `perf`コマンド（`sudo apt install linux-tools-generic`）
- sudo権限

### スクリプト一覧

| スクリプト | 用途 | 推奨用途 |
|-----------|------|----------|
| `perf_profile_nnue.sh` | NNUE有効時のプロファイリング | **本番相当の計測（推奨）** |
| `perf_profile_debug.sh` | debug buildでシンボル詳細解決 | Material評価時、関数名特定 |
| `perf_profile.sh` | 基本的なホットスポット特定 | 簡易計測 |
| `perf_reuse_search.sh` | SearchWorker再利用効果の測定 | 特定調査用 |

### 使用例

```bash
cd packages/rust-core

# NNUE有効時（推奨）
./scripts/perf_profile_nnue.sh --movetime 5000

# 結果は自動保存
ls perf_results/
# 20251218_121359_nnue_release.txt

# 対話的な詳細分析
sudo perf report -i perf_nnue.data
```

### ベンチマーク（NPS計測）

```bash
cd packages/rust-core
# --nnue-file オプションはperf.confで指定で省略可能
# --nnue-file オプションを指定したときはperf.conf の設定をオーバライド
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
  --internal --threads 1 --limit-type movetime --limit 20000 \
  --nnue-file ./path/to/nn.bin \
  --output-dir ./benchmark_results
```

---

## 変更履歴

| 日付 | 内容 |
|------|------|
| 2025-12-18 | 初回計測実施、ホットスポット一覧を記録 |
| 2025-12-18 | MovePicker最適化調査完了（最適化余地なし） |
| 2025-12-18 | ドキュメント作成 |
| 2025-12-18 | Material評価時の計測をrelease buildに更新、シンボル解決修正 |
| 2025-12-18 | 計測結果を再計測値で更新（NNUE: MovePicker 6.55%, refresh 6.40%, Material: eval_lv7_like 24.48%） |
| 2025-12-19 | Bitboard256 AVX2 SIMD化調査完了（AMD Zen 3環境では効果なし、フィーチャーフラグで残す） |
