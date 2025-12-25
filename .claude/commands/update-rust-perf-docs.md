# Rust エンジン パフォーマンスドキュメント更新

`packages/rust-core/docs/performance/README.md` を最新の計測結果で更新します。

## 前提条件

以下のスクリプトを実行済みであること:

```bash
cd packages/rust-core
./scripts/perf_all.sh                # 基本計測（perfプロファイリング + benchmark + 並列効率）
./scripts/perf_all.sh --perf-stat    # perf stat計測も含む場合
./scripts/perf_all.sh --threads 1,2,4,8  # スレッド数を指定する場合
```

このスクリプトは内部でsudoを使用するため、ユーザーが実行する必要があります。

**注意**:
- `--perf-stat` オプションを指定した場合のみ perf stat（dTLB-load-misses, cache-misses, branch-misses）が計測されます
- デフォルトでは1スレッドと8スレッドのベンチマークが実行され、並列効率が計算されます

## 手順

1. **計測結果の確認**
   - `packages/rust-core/perf_results/` ディレクトリの最新ファイルを確認
   - `packages/rust-core/benchmark_results/` ディレクトリの最新ファイルを確認

2. **結果の読み取り**

   **perfレポート（推奨: フラットレポートを優先）**
   - `nnue_flat.txt` - NNUE有効時のフラットレポート（--no-children、自己時間のみ）**← 最も正確**
   - `nnue_callers.txt` - NNUE有効時のコールグラフ（-g caller、呼び出し元情報付き）
   - `*_nnue_release.txt` - NNUE有効時の詳細レポート（コールツリー付き）
   - `*_release.txt` - Material評価時のレポート

   **並列探索サマリ（重要）**
   - `*_parallel_summary.txt` - 並列探索効率の計測結果
   - Material評価とNNUE評価それぞれの1T/8T NPS、スケール倍率、効率（%）を記録
   - 例: `8    4,665,227    7.65x    95.7%` （8スレッドで4.6M NPS、7.65倍スケール、95.7%効率）

   **ベンチマーク結果（NPS計測）**
   - NNUE有効時: `nnue_enabled: true` のJSONファイル
   - Material評価時: `nnue_enabled: false` のJSONファイル
   - 各局面のNPS、depth、bestmoveを記録
   - 平均NPSを計算（4局面の単純平均）

   **perf stat 結果（`--perf-stat` 指定時のみ）**
   - `*_perfstat_nnue.txt` - NNUE有効時の perf stat
   - `*_perfstat_material.txt` - Material評価時の perf stat
   - 計測項目: dTLB-load-misses, cache-misses, branch-misses

   **フラットレポート vs 詳細レポートの違い**
   - `nnue_flat.txt`: 各関数の自己時間（self time）のみ。ホットスポット一覧の更新に最適
   - `*_nnue_release.txt`: コールツリー付き。関数の内訳分析に有用

3. **前回計測との比較分析**
   - `packages/rust-core/docs/performance/README.md` の現在の値と新しい計測値を比較
   - 変更履歴の直近エントリから前回の値を確認
   - 以下の観点で分析:
     - **改善した項目**: CPU%が減少した関数（最適化の効果）
     - **目立つようになった項目**: CPU%が増加した関数（他の処理が高速化した結果、相対比率が上昇）
     - **順位変動**: ホットスポットの順位が入れ替わった場合
   - 改善があった場合は、その原因（直近のコミットやブランチ名から推測）も記載

4. **ドキュメント更新**
   - `packages/rust-core/docs/performance/README.md` のホットスポット一覧を更新
   - 計測日を更新
   - 変更履歴に追記（前回比較の分析結果を含める）

## 実行

最新の計測結果ファイルを読み込み、`packages/rust-core/docs/performance/README.md` を更新してください。

主な更新項目:
- 「NPS計測結果」セクション（NNUE/Material両方の局面別NPS、平均NPS）
- 「並列探索効率」セクション（1T/8T NPS、スケール倍率、効率%）
- 「ホットスポット一覧」セクションのCPU%
- 計測環境の「計測日」
- 「変更履歴」に新しいエントリを追加

**並列探索効率セクションについて**:
- README.mdに「並列探索効率」セクションがない場合は新規作成
- 「NPS計測結果」セクションの後、「ホットスポット一覧」の前に配置
- 以下のフォーマットで記載:
  ```markdown
  ## 並列探索効率

  計測条件: `--threads 1,8 --tt-mb 256 --limit-type movetime --limit 20000`

  ### Material評価

  | スレッド | NPS | スケール | 効率 |
  |---------|----:|--------:|-----:|
  | 1 | 609,551 | 1.00x | 100.0% |
  | 8 | 4,665,227 | 7.65x | 95.7% |

  ### NNUE評価

  | スレッド | NPS | スケール | 効率 |
  |---------|----:|--------:|-----:|
  | 1 | 474,442 | 1.00x | 100.0% |
  | 8 | 3,671,383 | 7.74x | 96.8% |
  ```

注意:
- 調査完了項目（MovePicker等）の内容は変更しないこと
  - `### MovePicker (調査完了)` セクション内の計測値（CPU%等）は当時の調査時の値のまま残し、更新しない
- **PGO効果セクションは更新しないこと**
  - `### PGO (Profile-Guided Optimization) 効果` セクションは当時のPGOビルド計測値を記録したもの
  - `### LTO・PGO組み合わせ効果` セクションも同様
  - これらは `build_pgo.sh` を実行して再計測した場合のみ更新する
- **perf stat 結果について**
  - `--perf-stat` オプションを指定して計測した場合のみ、perf stat 関連のセクションを更新する
  - `--perf-stat` を指定せずに計測した場合は、perf stat の箇所（dTLB-load-misses, cache-misses, branch-misses等）はそのまま維持する
- CPU%の値は小数点2桁まで記載
- NPSの値はカンマ区切りで記載（例: 1,055,823）
- 関数名が変わっている場合は適切に更新

## 変更履歴の書き方

変更履歴には以下の情報を含める:

1. **基本情報**: 主要関数のCPU%（NNUE: MovePicker, network::evaluate/AffineTransform, refresh等、Material: eval_lv7_like, direction_of等）
2. **並列効率**: 8スレッド時の効率（%）に変動があれば記載（例: 71%→96%）
3. **改善点**: `**改善点**:` プレフィックスで、CPU%が減少した項目と減少率、原因の推測を記載
4. **相対変動**: 他の処理が高速化した結果、相対比率が上昇した項目があれば記載
5. **順位変動**: ホットスポットの順位が入れ替わった場合は記載

例:
```
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 9.52%, network::evaluate 3.74%...）。**改善点**: AffineTransformのループ逆転最適化により `network::evaluate` が4.74%→3.74%に約21%減少（外側ループを入力チャンクに変更）。NNUE推論高速化の結果、`MovePicker` が8.86%→9.52%に相対上昇 |
```

並列効率に大きな変動があった場合の例:
```
| 2025-12-26 | **並列探索効率大幅改善**: PDQSort最適化により8T効率がMaterial 71%→96%、NNUE 72%→97%に向上。MovePicker内の挿入ソートをPDQSortに切り替え、L3キャッシュ競合を解消 |
```
