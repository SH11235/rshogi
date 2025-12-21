# パフォーマンス分析レポート

このドキュメントは、将棋エンジン（Rust実装）のパフォーマンス計測結果と最適化調査の記録です。

## 計測環境

| 項目 | 値 |
|------|-----|
| CPU | AMD Ryzen 9 5950X 16-Core Processor |
| コア数 | 32 |
| OS | Ubuntu (Linux 6.8.0) |
| アーキテクチャ | x86_64 |
| 計測日 | 2025-12-21 |

---

## ホットスポット一覧

### NNUE有効時（本番相当）

計測コマンド: `./scripts/perf_profile_nnue.sh`

| 順位 | 関数 | CPU% | 状態 | 備考 |
|------|------|------|------|------|
| 1 | `MovePicker::next_move` | 9.07% | 調査完了 | [詳細](#movepicker-調査完了) |
| 2 | `Network::evaluate` | 5.93% | - | NNUE推論（隠れ層演算含む） |
| 3 | `attackers_to_occ` | 3.10% | - | 利き計算 |
| 4 | `__memset_avx2` | 2.82% | - | メモリ初期化 |
| 5 | `search_node` | 2.50% | - | 探索メインループ |
| 6 | `refresh_accumulator` | 2.38% | - | NNUE全計算 |
| - | `partial_insertion_sort` | 5.07% | 調査完了 | MovePicker内部 |

**注**: kernelオーバーヘッド（`__fsnotify_parent` 4.40%, `dput` 3.46%）はNNUEファイル読み込み時のもので、実際の探索時間には影響しない。

#### NNUE関連の内訳

| 関数 | CPU% | 説明 |
|------|------|------|
| `Network::evaluate` | 5.93% | NNUE推論（隠れ層演算含む） |
| `refresh_accumulator` | 2.38% | Accumulator全計算（差分更新失敗時） |
| `check_move_mate` | 1.93% | 1手詰め判定 |
| `append_active_indices` | 1.35% | 特徴量インデックス取得 |
| `update_accumulator` | 1.33% | Accumulator差分更新 |

### Material評価時（NNUE無効、release build）

計測コマンド: `./scripts/perf_profile.sh`

| 順位 | 関数 | CPU% | 備考 |
|------|------|------|------|
| 1 | `eval_lv7_like` | 25.51% | Material評価のメイン関数 |
| 2 | `direction_of` | 15.85% | 方向計算 |
| 3 | `compute_board_effects` | 9.51% | 盤面効果計算 |
| 4 | `MovePicker::next_move` | 7.57% | 指し手選択 |
| 5 | `search_node` | 4.67% | 探索メインループ |
| 6 | `check_move_mate` | 4.53% | 1手詰め判定 |
| 7 | `__memset_avx2` | 3.27% | メモリ初期化 |
| 8 | `do_move` | 2.76% | 指し手実行 |
| 9 | `build_cont_tables` | 2.15% | Continuation History構築 |
| 10 | `attackers_to_occ` | 2.07% | 利き計算 |

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

#### フィーチャーフラグについて

**デフォルトでは `simd_avx2` は無効**です。有効にするには明示的に指定が必要です。

```bash
# デフォルト: スカラー版（simd_avx2 無効）
cargo build --release

# AVX2版を有効化
cargo build --release --features simd_avx2

# ベンチマーク実行時
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release \
  --features simd_avx2 -- --internal --threads 1 ...
```

#### 並列探索実装時の検証方法

マルチスレッド環境ではメモリ帯域幅がボトルネックになる可能性があり、SIMD版の効果が出る可能性がある。以下の手順で検証を推奨:

**1. スレッド数を変えた比較**

```bash
# スカラー版とAVX2版を各スレッド数で比較
for threads in 1 2 4 8 16; do
  echo "=== Threads: $threads (scalar) ==="
  RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
    --internal --threads $threads --limit-type movetime --limit 20000

  echo "=== Threads: $threads (AVX2) ==="
  RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release \
    --features simd_avx2 -- \
    --internal --threads $threads --limit-type movetime --limit 20000
done
```

**2. 検証ポイント**

| 項目 | 確認内容 |
|------|---------|
| NPS | スレッド数増加時にAVX2版の相対効率が向上するか |
| bestmove | 同一入力で同一結果が得られるか（探索の非決定性による差異は許容） |
| メモリ帯域 | `perf stat -e cache-misses` でキャッシュミス率を確認 |

**3. perfプロファイル（マルチスレッド）**

```bash
# マルチスレッドでのホットスポット確認
./scripts/perf_profile_nnue.sh --threads 4 --movetime 10000

# キャッシュミス統計
sudo perf stat -e cache-references,cache-misses,L1-dcache-load-misses \
  ./target/release/engine-usi <<< "usi
setoption name Threads value 8
go movetime 10000
quit"
```

**4. 期待される結果**

- スレッド数が少ない場合: スカラー版とAVX2版でほぼ同等
- スレッド数が多い場合: メモリ帯域幅がボトルネックになればAVX2版が有利になる可能性

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
| 2025-12-19 | simd_avx2フィーチャーフラグの説明と並列探索時の検証方法を追加 |
| 2025-12-20 | 計測結果更新（NNUE: MovePicker 8.11%, Network::evaluate 5.86%, refresh 5.70%、Material: eval_lv7_like 25.84%, direction_of 16.12%） |
| 2025-12-21 | 計測結果更新（NNUE: MovePicker 8.83%, AffineTransform 5.93%, refresh 2.27%、Material: eval_lv7_like 26.38%, direction_of 15.88%）。refresh_accumulatorが5.70%→2.27%に大幅改善（AccumulatorとFeatureTransformerへのAlignedBox導入によるメモリアラインメント最適化の効果） |
| 2025-12-21 | 計測結果更新（NNUE: MovePicker 9.07%, Network::evaluate 5.93%, refresh 2.38%、Material: eval_lv7_like 25.51%, direction_of 15.85%）。フラットレポート（nnue_flat.txt）を使用した正確な自己時間計測に基づく更新 |
