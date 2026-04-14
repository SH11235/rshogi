# NNUE Threat 系特徴量実験の回顧

日付: 2026-04-15
ステータス: **現状 — Baseline (HalfKA_hm L1=1536) が最良、Threat 系特徴量追加は全て採用レベルに届かず**

## 目的

HalfKA_hm baseline に対して Threat (盤上駒の攻撃関係) および HandThreat
(手駒 drop 仮想脅威) 系の特徴量を追加する一連の訓練実験が行われてきた
経緯を、本日までの全試行を横断的に整理する。個別実験の失敗 (HandThreat
defensive) だけを記録するのではなく、**共通する失敗メカニズムと今後の
判断基準** を残すことが目的。

bullet-shogi 側の訓練実験番号 (vN) は private な実験記録ディレクトリ
(gitignore) に紐付くため、本ドキュメントでは一切使用しない。代わりに
記号定義を先頭で行い、以降は記号とアーキテクチャ構成文字列で参照する。

## 本ドキュメントで使用する記号

| 記号 | アーキテクチャ構成 | 備考 |
|---|---|---|
| **Baseline-1536** | HalfKA_hm のみ, L1=1536, FT 出力 1536 次元 | **現時点の最良 production 候補** |
| **Baseline-768** | HalfKA_hm のみ, L1=768 | L1 縮小版 baseline |
| **BoardThreat-Full-1536** | Baseline-1536 + Board Threat (全 pair, ~216,720 次元) | |
| **BoardThreat-Full-768** | Baseline-768 + Board Threat (全 pair) | |
| **BoardThreat-Full-512** | Baseline-? + Board Threat (全 pair), L1=512 | さらなる L1 縮小 |
| **BoardThreat-SameClass** | BoardThreat-Full-768 から同種ペア除外 (profile 1) | 次元削減版 |
| **BoardThreat-CrossSide** | BoardThreat-Full-512 から cross-side のみ (profile 10) | 次元削減版 |
| **HandThreat-Full** | Baseline-768 + HandThreat case A full drop-attack pair (121,104 次元) | |
| **HandThreat-Def** | Baseline-768 + HandThreat defensive (30,276 次元) | 本日の実験 |
| **PSQT-1536** | Baseline-1536 + PSQT shortcut | Threat 系ではないが同系統の cost-添加実験として参考記載 |

共通: いずれも L2=16, L3=32, optimizer = Ranger, wrm-in-scaling=340,
bucket-mode = progress8kpabs, dataset = DLSuisho15b_deduped_shuffled.bin。
訓練 step 数は実験により異なる (100 sb〜400 sb、途中停止含む)。

## 結論の要約

**現時点で Baseline-1536 を byoyomi (実戦条件) で上回った派生は存在しない。**

depth 固定での eval 品質比較では一部の派生 (BoardThreat-Full-768,
BoardThreat-SameClass) が Baseline を上回るが、byoyomi 条件 (実際の
対局で時間が決められている) では全て NPS 劣位による探索深さ差で負け越す、
もしくは同等にとどまる。HandThreat 系 (Full / Defensive) は eval 品質と
NPS の両方で劣勢。

## 系譜と結果

以下、各実験の設定・結果・本日時点の判定を整理する。数値は bullet-shogi
側の private 実験記録 (local のみ) からの引用。depth-fixed / byoyomi の
区別を明示する。

### PSQT 添加 (参考)

**PSQT-1536**:
- 結果: Baseline-1536 との depth 固定比較で **Elo -38 ±31** (v87-260 相当
  時点)。PSQT ショートカットの重みが有効に収束せず、180 sb 以降の学習
  進行も停滞。
- 判定: **不採用**。depth 固定で既に改善していない = eval 品質がむしろ
  悪化している。

### Board Threat 系 (L1=1536)

**BoardThreat-Full-1536**:
- 状態: 本格訓練・評価は未実施のまま、L1 縮小版 (L1=768) にリソース集中。
- 理由: Baseline-1536 + Board Threat 全 pair は NPS が厳しく、
  L1=1536 のままでは実戦運用が想定しにくいため、L1 を下げる方向に移行。

### Board Threat 系 (L1=768/512、NPS 改善目的の L1 縮小)

**BoardThreat-Full-768**:
- depth 固定 (180 sb / 280 sb): Baseline 比 **Elo +29 / +38**
  (eval 品質が改善している確証あり)
- byoyomi 1000ms (140 sb / 280 sb): Baseline 比 **Elo -20 / -45**
  (NPS 劣位により、eval 改善分を打ち消して実戦棋力で負け越し)
- NPS gap: Baseline 比 74.5% (= Baseline の 75% 程度の NPS)
- 探索深さ差が **約 83 Elo** 相当で効いていると見積もり
- 判定: **採用不可**。eval 質の改善はあるが、NPS 劣位が大きすぎて
  byoyomi で回収不能。

**BoardThreat-Full-512**:
- 目的: L1 をさらに縮小して NPS を改善する
- 結果: Baseline-768 (同構成の L1=768 版) との直接対決で **互角**。
  NPS 改善は +2.7pp のみで、eval 容量低下が大きく相殺。
- 判定: **不採用**。L1=768 を超えるメリットなし。160 sb で停止。

**BoardThreat-SameClass** (同種ペア除外 profile):
- 次元削減: BoardThreat-Full-768 から同種攻撃 pair (攻撃側と被攻撃側が
  同クラス) を除外してテーブルを縮小
- depth 10 比較 (20 sb 時点):
  - BoardThreat-Full-768 比: **Elo -17 ±?** (eval 品質やや劣化)
  - Baseline 比: **Elo +30 ±?** (Baseline よりは depth 固定で良い)
- 判定: **未完**。depth で eval 改善は見られるが byoyomi 評価前に停止。

**BoardThreat-CrossSide** (cross-side profile, L1=512):
- 次元削減: 味方→敵 / 敵→味方の異種 pair のみ保持。テーブルサイズ
  106MB → 47MB に半減。
- NPS: Baseline 比 +12-13% 改善
- eval: depth 10 で Baseline 比 **Elo -28〜-45** (NPS 改善では補えない
  eval 劣化)
- 判定: **不採用**。same-side の連携情報を落とすと eval 品質が大きく
  崩れる。200 sb で停止。

### HandThreat 系 (持ち駒 drop 仮想脅威)

**HandThreat-Full**:
- 次元: 121,104 (drop_owner × hand_class × attacked_side × attacked_class
  × drop_sq × attack_to_sq の全 pair)
- 意味論: 「両者が手駒を drop して両者の駒を攻撃する仮想脅威」を全方向
  列挙
- depth 9 比較 (20 sb 時点):
  - Baseline 20 sb 比: **Elo -76 ±49** (同一学習段階で劣勢、統計有意)
  - Baseline 400 sb 比: **Elo -177 ±55** (学習段階差 + HandThreat 寄与の
    合算)
- NPS gap: Baseline 比 **-84.7%** (Baseline の約 15% NPS)
- 学習速度: Baseline の約 29% (174K pos/sec)
- 判定: **不採用**。eval 品質も NPS も両方劣勢で回復見込みなし。
  41 sb で停止。

**HandThreat-Def** (本日の実験):
- 次元: 30,276 = HandThreat-Full の正確に 1/4 (drop_owner=enemy かつ
  attacked_side=friend のみ符号化)
- 目的: HandThreat-Full の NPS 問題を次元削減で改善する
- 実装: rshogi / bullet-shogi 双方に feature flag 追加、非対称 emission
  API (`SparseInputType::map_features_split`) を追加
- cross-validation: rshogi `verify_nnue_accumulator --moves 500` PASS、
  bullet-shogi `shogi_layerstack_eval --integer-forward` と rshogi
  `eval diag` で 5 sample が bit-exact 一致
- active 数実測 (rshogi rebuild path counter):
  - startpos: 26.0 avg (HandThreat-Full: 148.7, **1/5.7**)
  - midgame: 115.6 avg (HandThreat-Full: 383.7, **1/3.3**)
- NPS 比較 (同一条件 1 SB × 100 batch minimal training, L1=768, movetime 8s):

  | Block | NPS startpos | NPS midgame | vs Baseline-768 (midgame) |
  |---|---|---|---|
  | Baseline-768 | 836,969 | 547,771 | 1.00x (baseline) |
  | BoardThreat-Full-768 | 453,237 | 353,191 | **1.55x 遅** |
  | HandThreat-Full | 262,650 | 57,888 | **9.46x 遅** |
  | **HandThreat-Def** | **283,748** | **130,492** | **4.20x 遅** |

- 判定: **採用不可、本格訓練断念**。HandThreat-Full よりは midgame で
  NPS 2.25x 改善するが、Baseline-768 の 4.20x 遅は実戦要件を満たさない。
  eval 品質は訓練前だが、情報量削減 (1/4 次元) の影響で HandThreat-Full
  を超える期待値は低い。
- 実装コードは archive tag で保存 (下記「参照」セクション)

## 横断的な観察

### 1. depth 固定と byoyomi の乖離

**BoardThreat-Full-768** が典型で、depth 固定では Baseline に Elo +29〜+38
勝っているのに、byoyomi では NPS 劣位により Elo -20〜-45 負けている。
この差 (約 60-80 Elo) が **NPS による探索深さ劣位** の分。

**教訓**: 新 feature の評価は depth 固定と byoyomi の両方で行い、
byoyomi の結果だけを最終判定に使う。depth 固定の改善だけで採用を決めると
実戦で逆転する。

### 2. 次元削減の限界

Board Threat / HandThreat の次元削減 profile (BoardThreat-SameClass /
BoardThreat-CrossSide / HandThreat-Def) は、いずれも NPS を改善するが
eval 品質が同時に劣化し、結果として両立できない。

**教訓**: 「NPS を上げるため次元を削る」アプローチは本質的に eval 容量を
犠牲にしており、採用レベルに達するのは難しい。delta が小さい profile
(テーブル削減だけで意味論を壊さない) でも baseline を明確に超えるケース
は確認できていない。

### 3. 反実仮想 (counterfactual) feature の構造的コスト

Board Threat (盤上実在駒の攻撃関係) は active 数が O(駒数) で bound され
自然に小さく収まるが、HandThreat (仮想 drop × 攻撃対象の列挙) は
O(空マス × 手駒クラス数 × drop 後 attack 可能数) で桁違いに大きい。
HandThreat-Def で drop_owner/attacked_side を各 1 方向に絞っても、
active 数は Baseline (HalfKA_hm) の 1.5〜3 倍に残り、NPS で 3〜4x 遅い。

**教訓**: 「存在しない駒の仮想状況」を feature として列挙する設計は、
1 feature あたりの計算量が小さくても active 数の絶対値で NPS を破壊
しやすい。実在する物体 (盤上駒・既に持っている手駒の数値) の特徴量
設計を優先する。

### 4. 学習信号の希薄化

次元を増やすほど 1 feature あたりの更新頻度が下がり、同じ学習予算
(sb 数) での収束が悪化する。HandThreat-Full は 121,104 次元あり、
1 sample 中に fire する feature の割合が極めて低いため、各 weight への
gradient 信号が希薄。Baseline-1536 と同じ 20 sb では明らかに収束
不足で、400 sb まで回しても採用レベルに届く保証がない。

**教訓**: 次元数を増やすなら、その分 training budget を線形以上に
スケールする必要がある。「とりあえず既存 budget で試す」は負け筋。

## 今後の判断基準

新しい feature block を Baseline に追加して実戦採用を目指す場合、以下の
事前チェックを推奨する。全て yes でなければ、実装・訓練開始前に再設計
するべき。

### (a) NPS 事前見積もり

- [ ] 設計段階で 1 sample あたりの active feature 数 (両 perspective 合計)
      を概算
- [ ] Baseline HalfKA_hm の active 数 (typical 80) に対して **2 倍以内**
      に収まっているか確認
- [ ] 差分更新時の 1 手あたりの index 変化数が **10 以内** に収まる設計か
      (HalfKA: 2-4/手、HandThreat-Full: 27-64/手)

### (b) feature 意味論

- [ ] feature が参照する対象が **物理的に盤面に存在** するか、
      **少数の数値 (手駒カウント等)** で表現できるか
- [ ] 反実仮想 (「もし X したら」) の列挙ではないか
- [ ] 反実仮想が必要な場合、**対象を極端に絞る** (king 近傍のみ、等)
      ことで active 数を bound できているか

### (c) 訓練予算と収束の見積もり

- [ ] 次元数の増加に対して、収束に必要な sb 数を事前に見積もる
- [ ] 実行可能な予算内 (wall clock) で収束するか確認
- [ ] 収束しない場合、budget 確保 or 設計簡略化の判断が必要

### (d) 段階的評価の決めごと

- [ ] 低 sb (10-20 sb) 段階で **depth 固定と byoyomi の両方** で Baseline
      同世代との対決を行う
- [ ] depth 固定で **Elo -20 以下** なら即停止検討
- [ ] depth 固定で正、byoyomi で負の場合、NPS 差を分解して **回収可能な
      次元か** 判断 (回収不可なら停止)

## 参照

### 2026-04-15 cleanup 前の実装 snapshot (archive tag)

本 retrospective の作成と同時に、不採用特徴量 (HandThreat 系全般,
BoardThreat-CrossSide profile, LayerStack bucket mode KingRank9/Ply9) を
main branch から削除した。削除前の実装は annotated tag で保存されており、
`git checkout` で復元可能:

```bash
# rshogi
git fetch --tags
git checkout archive/nnue-unadopted-features-20260415
# → 2026-04-15 cleanup 前の rshogi source tree (commit 3e1f1b0d)
# この tag は HandThreat 全般 / BoardThreat-CrossSide /
# LayerStack KingRank9/Ply9 を含む当時のソース全体を保存する

# bullet-shogi (HandThreat defensive 実装のみ固有)
cd /path/to/bullet-shogi
git fetch --tags
git checkout archive/hand-threat-defensive
# → bullet-shogi の HandThreat defensive 実装時点 (commit ea48f67)
# SparseInputType::map_features_split 非対称 emission API と
# ShogiHalfKaHmHandThreatDefensive input type を含む
```

**注意**: rshogi と bullet-shogi で tag 名が異なる。rshogi 側は cleanup
全体をカバーする広い名前、bullet-shogi 側は HandThreat defensive 固有の
変更を保存するので狭い名前になっている。

### 主要な過去 experiment 記録 (private、gitignore)

bullet-shogi 側の `docs/experiments/` 配下に各実験の詳細 (訓練コマンド、
loss 推移、selfplay 結果) がまとめられているが、これらは local 閉じた
ドキュメントであり、本 retrospective からは直接参照できない。個々の
実験の詳細は該当 local doc を参照。

### 今セッションで main に残された最適化改善 (HandThreat-Full に対するもの)

HandThreat-Def は撤退したが、開発過程で HandThreat-Full に対する以下の
最適化は採用可能な改善として main に残る:

- refresh path 診断 instrumentation (計測 counter)
- within-mirror king move の incremental 化
- diff fallback 全 case の incremental 化 (INCREMENTAL_OK 100%)
- HandThreat 専用 Tier 2 multi-ply walk (Fix B)
- active / diff change 分布計測 counter

ただしこれらの改善があっても HandThreat-Full 自体は NPS で採用レベルに
届かないため、実質的には「将来 HandThreat 系を再検討する場合の基盤」
としての価値にとどまる。

## 注記

### v-番号について

bullet-shogi 側の訓練実験番号 (v87, v88, ... のような prefix) は
`docs/experiments/` 配下の private (gitignore) ドキュメントに紐付く
ローカル識別子であり、本 retrospective では一切使用しない。
アーキテクチャ構成を明示的に記述し、必要な場合は本ドキュメント先頭で
定義した記号で参照する。

### search 側改修との独立性

rshogi 側の探索実装の改修 (sfcache, Tier 2 Fix B 等) は、bullet-shogi
側の訓練実験番号とは**独立**。それぞれ別の時系列・コミット系譜を持ち、
「訓練実験 N の頃に search 改修 M が入った」という対応関係を直接的に
紐付けないこと。search 改修は実装コミット履歴で追う。
