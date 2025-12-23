# パフォーマンス分析レポート

このドキュメントは、将棋エンジン（Rust実装）のパフォーマンス計測結果と最適化調査の記録です。

## 計測環境

| 項目 | 値 |
|------|-----|
| CPU | AMD Ryzen 9 5950X 16-Core Processor |
| コア数 | 32 |
| OS | Ubuntu (Linux 6.8.0) |
| アーキテクチャ | x86_64 |
| 計測日 | 2025-12-23 (更新) |

---

## NPS計測結果

### 最新ベンチマーク（主開発環境: AMD Ryzen 9 5950X）

計測条件: `--threads 1 --tt-mb 1024 --limit-type movetime --limit 20000`

#### NNUE評価時

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 16 | 1,056,007 | 2e2d |
| 2 | 中盤（詰将棋風） | 13 | 480,790 | 8d8f |
| 3 | 終盤（王手飛車） | 14 | 611,674 | 5d6c+ |
| 4 | 終盤（詰み筋） | 15 | 513,612 | G*2h |
| **平均** | - | - | **665,521** | - |

#### Material評価時（NNUE無効、MaterialLevel=9）

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 17 | 529,551 | 5i6h |
| 2 | 中盤（詰将棋風） | 18 | 423,551 | 8d7d |
| 3 | 終盤（王手飛車） | 18 | 447,891 | N*4d |
| 4 | 終盤（詰み筋） | 17 | 416,696 | G*1c |
| **平均** | - | - | **454,422** | - |

### VNNI効果測定（別端末: Intel Cascade Lake-X）

| 構成 | NNUE平均NPS | 変化 |
|------|----------:|-----:|
| AVX2（VNNI無効） | - | ベースライン |
| AVX512-VNNI | - | **+13%** |

※ VNNI対応CPUでのみ効果あり（Intel Ice Lake以降、AMD Zen 4以降）

### YaneuraOu比較（主開発環境）

#### 通常ビルド（開発時）

| エンジン | NNUE NPS | Material NPS | 備考 |
|---------|--------:|-------------:|------|
| 本エンジン | 665,521 | 454,422 | `cargo build --release` |
| YaneuraOu | 1,118,219 | 1,545,172 | 参考値 |
| **対YaneuraOu比** | **59%** | **29%** | - |

#### PGOビルド（本番用）

| エンジン | NNUE NPS | 対YO比 | 備考 |
|---------|--------:|-------:|------|
| 本エンジン（PGO前） | 681,366 | 61% | ベースライン |
| **本エンジン（PGO後）** | **723,855** | **65%** | **+6.2%向上** |
| YaneuraOu | 1,118,219 | 100% | 参考値 |

※ PGOビルド: `./scripts/build_pgo.sh`

### PGO (Profile-Guided Optimization) 効果

#### NNUE評価時（本番相当）

計測条件: `./target/release/benchmark --nnue-file ...` (3回実行の平均)

| 状態 | Run 1 | Run 2 | Run 3 | 平均NPS |
|------|------:|------:|------:|--------:|
| PGO前 | - | - | - | 681,366 |
| **PGO後** | 722,809 | 725,249 | 723,507 | **723,855** |

| 指標 | 値 |
|------|-----|
| **NPS向上率** | **+6.2%** |
| 絶対値向上 | +42,489 NPS |

#### Material評価時

計測条件: `./target/release/benchmark` (3回実行の平均)

| 状態 | Run 1 | Run 2 | Run 3 | 平均NPS |
|------|------:|------:|------:|--------:|
| PGO前 | 435,567 | 434,590 | 435,712 | 435,290 |
| **PGO後** | 494,473 | 500,417 | 498,039 | **497,643** |

| 指標 | 値 |
|------|-----|
| **NPS向上率** | **+14.3%** |
| 絶対値向上 | +62,353 NPS |

#### PGOの最適化内容

- 分岐予測最適化（頻繁に取られる分岐を優先配置）
- コードレイアウト最適化（ホットパスを連続メモリに配置）
- インライン判断の改善（実行頻度に基づく）

**注**: NNUE評価はMaterial評価より計算負荷が高いため、PGO効果が相対的に小さくなる（+6.2% vs +14.3%）

### LTO・PGO組み合わせ効果（NNUE、参考値）

| 構成 | 平均NPS | 対ベースライン | 備考 |
|------|--------:|---------------:|------|
| Thin LTO（ベースライン） | 681,366 | - | `lto = "thin"` |
| Full LTO | 692,132 | +1.6% | `lto = "fat"` |
| Thin LTO + PGO | 723,855 | +6.2% | - |
| **Full LTO + PGO** | **728,017** | **+6.8%** | **本番用（推奨）** |

- Full LTO単体: +1.6%
- PGO単体: +6.2%
- Full LTO + PGO: +6.8%（PGOに対して+0.6%の追加効果）

**結論**: 本番リリースでは最大性能を優先し、**Full LTO + PGO**（`--profile production`）を使用。`build_pgo.sh` はこの構成でビルドする

---

## ホットスポット一覧

### NNUE有効時（本番相当）

計測コマンド: `./scripts/perf_profile_nnue.sh`

| 順位 | 関数 | CPU% | 状態 | 備考 |
|------|------|------|------|------|
| 1 | `MovePicker::next_move` | 11.10% | 調査完了 | [詳細](#movepicker-調査完了) |
| 2 | `network::evaluate` | 3.74% | - | NNUE推論メイン |
| 3 | `attackers_to_occ` | 3.05% | - | 利き計算 |
| 4 | `search_node` | 2.85% | - | 探索メインループ |
| 5 | `refresh_accumulator` | 2.41% | - | NNUE全計算 |
| 6 | `check_move_mate` | 2.03% | - | 1手詰め判定 |
| 7 | `__memset_avx2` | 1.91% | - | メモリ初期化 |
| - | `partial_insertion_sort` | - | 調査完了 | MovePicker内部 |

**注**: kernelオーバーヘッド（`__fsnotify_parent` 4.34%, `dput` 3.62%）はNNUEファイル読み込み時のもので、実際の探索時間には影響しない。

#### NNUE関連の内訳

| 関数 | CPU% | 説明 |
|------|------|------|
| `network::evaluate` | 3.74% | NNUE推論メイン |
| `refresh_accumulator` | 2.41% | Accumulator全計算（差分更新失敗時） |
| `check_move_mate` | 2.03% | 1手詰め判定 |
| `do_move` | 1.72% | 指し手実行 |
| `update_accumulator` | 1.41% | Accumulator差分更新 |
| `append_active_indices` | 1.19% | 特徴量インデックス取得 |

### Material評価時（NNUE無効、release build）

計測コマンド: `./scripts/perf_profile.sh`

| 順位 | 関数 | CPU% | 備考 |
|------|------|------|------|
| 1 | `eval_lv7_like` | 27.39% | Material評価のメイン関数 |
| 2 | `direction_of` | 15.59% | 方向計算 |
| 3 | `compute_board_effects` | 9.26% | 盤面効果計算 |
| 4 | `MovePicker::next_move` | 7.78% | 指し手選択 |
| 5 | `check_move_mate` | 4.75% | 1手詰め判定 |
| 6 | `search_node` | 4.62% | 探索メインループ |
| 7 | `do_move` | 2.91% | 指し手実行 |
| 8 | `__memmove_avx` | 1.88% | メモリコピー |
| 9 | `see_ge` | 1.58% | SEE計算 |
| 10 | `attackers_to_occ` | 1.55% | 利き計算 |

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
| **`build_pgo.sh`** | **PGO最適化ビルド** | **本番デプロイ用（+14% NPS）** |

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

### 計測時のビルドプロファイル

- **差分追跡の基準**: `--release` を使用（本ドキュメントのNPS/perfはここを基準に記録）
- **最高最適化の計測**: `build_pgo.sh`（`--profile production` 相当 / Full LTO + PGO）

### PGOビルド（本番デプロイ用）

```bash
cd packages/rust-core

# PGOビルド実行（約3分）- Full LTO + PGOで最大性能
./scripts/build_pgo.sh

# 効果確認付き
./scripts/build_pgo.sh --verify

# プロファイルデータ削除
./scripts/build_pgo.sh --clean
```

PGOビルドの処理フロー:
1. プロファイル収集用ビルド (`--profile production -C profile-generate`)
2. ベンチマーク実行でプロファイル収集
3. `llvm-profdata merge` でマージ
4. PGO適用ビルド (`--profile production -C profile-use`)

**出力先**: `./target/production/` ディレクトリ

**注意**: 開発中の反復作業には通常ビルドを推奨（高速なイテレーション）。PGOビルドはリリース前の最終計測・本番デプロイ時に使用。

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
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 8.76%, AffineTransform 4.68%, refresh 2.44%、Material: eval_lv7_like 26.25%, direction_of 16.16%）。NNUE関連の内訳をフラットレポートに基づき修正（AffineTransform::propagateが主要な処理として明確化）。**改善点**: `__memset_avx2`が2.82%→1.91%に約32%減少（MoveBuffer関連のmemset削減最適化の効果） |
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 8.86%, AffineTransform 4.74%, refresh 2.40%、Material: eval_lv7_like 26.22%, direction_of 16.85%）。Material評価時の順位変動: `__memmove_avx`が9位に上昇、`attackers_to_occ`が10位に |
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 9.52%, network::evaluate 3.74%, refresh 2.49%、Material: eval_lv7_like 25.95%, direction_of 16.25%）。**改善点**: AffineTransformのループ逆転最適化により `network::evaluate` が4.74%→3.74%に約21%減少（外側ループを入力チャンク、内側を出力に変更し、入力ブロードキャストと重みアクセスの連続性を改善）。NNUE推論高速化の結果、`MovePicker` が8.86%→9.52%、`check_move_mate` が2.17%でホットスポット6位に浮上するなど、相対比率が変動 |
| 2025-12-22 | **NPS計測結果セクション追加**。NNUE/Material両方の局面別NPS、YaneuraOu比較表を追加。**VNNI dpbusd命令対応**: AVX512-VNNI対応CPUでNNUE積和演算を1命令化（`_mm256_dpbusd_epi32`）。別端末（Intel Cascade Lake-X）での計測で**+13% NPS向上**を確認 |
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 9.36%, network::evaluate 3.73%, refresh 2.45%、Material: eval_lv7_like 25.78%, direction_of 16.39%）。NPS: NNUE平均 681,366（+1.5%）、Material平均 435,547。YaneuraOu比が60%→61%に微増 |
| 2025-12-22 | **PGO (Profile-Guided Optimization) 導入**: `scripts/build_pgo.sh`追加。NNUE NPS **+6.2%向上**（681,366→723,855）、Material NPS **+14.3%向上**（435,290→497,643）。YaneuraOu比がNNUE 61%→65%に改善。PGO効果の詳細計測結果を追加 |
| 2025-12-22 | **本番ビルドプロファイル追加**: `[profile.production]`をCargo.tomlに追加。Full LTO、codegen-units=1、overflow-checks無効化。WASMビルドで-4.2%サイズ削減（865KB→829KB）。CIデプロイがproductionプロファイルを使用するよう更新 |
| 2025-12-22 | **LTO・PGO組み合わせ効果計測**: Full LTO単体+1.6%、Thin LTO+PGO +6.2%、Full LTO+PGO +6.8%。PGO効果が大きく、Full LTOの追加効果は限定的（+0.6%）。通常はThin LTO+PGOを推奨 |
| 2025-12-22 | **build_pgo.sh を Full LTO + PGO に変更**: 本番リリースでは最大性能を優先し、`--profile production`（Full LTO）を使用するよう変更。出力先は `./target/production/` |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 9.05%, network::evaluate 3.98%, refresh 2.59%、Material: eval_lv7_like 26.34%, direction_of 16.11%）。NPS: NNUE平均 682,777、Material平均 449,439（+3.2%向上）。Material評価時の順位変動: `do_move`が7位に上昇（3.17%）、`attackers_to_occ`が9位、`__memmove_avx`が10位に。**perfスクリプト修正**: `--call-graph dwarf`を`--call-graph fp`に変更（大規模ネスト配列のDWARF解析によるハング回避） |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 10.46%, network::evaluate 3.75%, refresh 2.37%、Material: eval_lv7_like 26.07%, direction_of 17.23%）。NPS: NNUE平均 668,968、Material平均 451,135。計測誤差の範囲内で大きな変動なし。Material評価時の`direction_of`が16.11%→17.23%に微増 |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 11.10%, network::evaluate 3.74%, attackers_to_occ 3.05%、Material: eval_lv7_like 27.39%, direction_of 15.59%）。NPS: NNUE平均 665,521、Material平均 454,422。計測誤差の範囲内。Material評価時の順位変動: `direction_of`が17.23%→15.59%に減少、`see_ge`が9位に新登場（1.58%） |
| 2025-12-23 | **コード品質改善**: `cont_history_ptr()`と`set_cont_history_for_move()`に`debug_assert!`境界チェック追加、`NonNull<PieceToHistory>`のSAFETYドキュメント追加、`unsafe impl Send`のSAFETYコメント詳細化、MovePicker内のContinuationHistoryインデックス4スキップの理由コメント追加、Sentinel初期化テスト追加 |
