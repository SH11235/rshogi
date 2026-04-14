# Experiments / 試行して採用しなかった実装と横断的な retrospective の記録

試行・計測した結果、本採用はしなかった実装、および複数の実験を横断して
整理した retrospective ドキュメントを保存する。同じ轍を踏まないため、
および再検討の際の出発点とするため、設計・測定・結論・今後の判断基準を
残す。

## ディレクトリの方針

- ファイル名は **タイムスタンプを先頭** に置く (`YYYYMMDD_<topic>.md`)。
  ファイル一覧が時系列で表示される
- 個別実験の詳細は **annotated tag** で保存する
  (`archive/<topic>` という命名規則、両リポジトリで同一名)
- 試行コード自体は main には残さず、tag 参照で復元可能にする
- 横断的な retrospective ドキュメントは main に commit し、今後の判断
  基準として参照できるようにする
- bullet-shogi 側の private 実験番号 (`docs/experiments/vN*.md`) は
  gitignore されているため、本リポジトリの commit docs からは **番号で
  参照しない**。代わりにアーキテクチャ構成文字列 + 条件で呼ぶか、
  doc 先頭で記号定義してから使う

## 一覧

| Date | Doc | 概要 | 関連 archive tag |
|---|---|---|---|
| 2026-04-15 | [20260415_nnue_threat_experiments_retrospective](./20260415_nnue_threat_experiments_retrospective.md) | NNUE Threat / HandThreat 系特徴量実験の横断的回顧。Baseline (HalfKA_hm L1=1536) が依然最良、全派生は byoyomi 実戦棋力で超えられず | rshogi: `archive/nnue-unadopted-features-20260415`、bullet-shogi: `archive/hand-threat-defensive` |

## 過去 archive の参照方法

```bash
# rshogi
git fetch --tags
git tag -l 'archive/*'            # archive tag 一覧
git checkout archive/nnue-unadopted-features-20260415
# → 該当時点の state に移動。ファイル内容確認後、元ブランチへ戻る
git checkout feat/threat-2a  # もしくは main 等

# bullet-shogi (別リポジトリ、HandThreat defensive 実装固有の tag)
cd /path/to/bullet-shogi
git fetch --tags
git checkout archive/hand-threat-defensive
```

remote 側から tag 一覧を見る場合は `git ls-remote --tags origin` または
GitHub の Tags タブを参照。
