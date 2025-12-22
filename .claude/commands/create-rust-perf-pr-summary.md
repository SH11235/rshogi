# Rust パフォーマンス改善 PRサマリ作成

パフォーマンス改善PRのサマリをマークダウンファイルで出力します。

## 前提条件

以下が完了していること:

1. `packages/rust-core/scripts/perf_all.sh` を実行済み（必要に応じて `--perf-stat` オプション付き）
2. `packages/rust-core/docs/performance/README.md` を更新済み

**注意**: `--perf-stat` オプションを指定した場合のみ perf stat 結果が計測されます。

## 手順

### 1. ブランチ情報の取得

```bash
# 現在のブランチ名
git branch --show-current

# mainブランチとの差分コミット
git log main..HEAD --oneline

# 変更されたファイル一覧
git diff main..HEAD --stat
```

### 2. パフォーマンス計測結果の確認

以下のディレクトリから最新の計測結果を読み取る:

- `packages/rust-core/perf_results/` - perfプロファイリング結果
- `packages/rust-core/benchmark_results/` - ベンチマーク結果

### 3. 前後比較データの収集

`packages/rust-core/docs/performance/README.md` の変更履歴から:
- 最適化前の値（前回計測）
- 最適化後の値（今回計測）
- 改善率

### 4. PRサマリの生成

以下の形式でマークダウンファイルを生成:

```markdown
# [ブランチ名に基づくタイトル]

## 概要

[最適化の概要を1-2文で]

## 変更内容

- [主な変更点を箇条書きで]

## パフォーマンス計測結果

### ベンチマーク比較（NNUE評価）

| 局面 | 最適化前 NPS | 最適化後 NPS | 変化率 |
|------|-------------|-------------|--------|
| Position 1 | xxx,xxx | xxx,xxx | +x.x% |
| Position 2 | xxx,xxx | xxx,xxx | +x.x% |
| Position 3 | xxx,xxx | xxx,xxx | +x.x% |
| Position 4 | xxx,xxx | xxx,xxx | +x.x% |
| **総合** | **xxx,xxx** | **xxx,xxx** | **+x.x%** |

### プロファイリング結果（CPU%変化）

| 関数 | 最適化前 | 最適化後 | 変化 |
|------|---------|---------|------|
| `function_name` | x.xx% | x.xx% | -x.xx% |

## テスト結果

- `cargo test` - ✅ Pass
- `cargo clippy` - ✅ Pass

## 関連ドキュメント

- `packages/rust-core/docs/performance/README.md` - 更新済み
```

### 5. ファイル出力

PRサマリを以下のパスに出力:

```
packages/rust-core/pr_summaries/[ブランチ名].md
```

ディレクトリが存在しない場合は作成する。

※ このディレクトリは `.gitignore` 対象（一時作業用ファイル）

## 実行

1. gitブランチ情報とコミット履歴を取得
2. benchmark_results/の最新JSONファイル（最適化前後）を比較
3. perf_results/のプロファイリング結果を確認
4. docs/performance/README.mdの変更履歴を参照
5. 上記テンプレートに従ってPRサマリを生成
6. マークダウンファイルとして出力

## 出力例

ファイル名: `packages/rust-core/pr_summaries/nnue_affine_loop_inversion.md`

内容には以下を必ず含める:
- 最適化の技術的な説明
- 前後の数値比較（表形式）
- 改善率のパーセンテージ
- 変更されたファイル一覧
