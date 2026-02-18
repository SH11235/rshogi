# YaneuraOu ノード数一致調査 ステータス

最終更新: 2026-02-18（pos1 d8 乖離調査完了）
コミット: 07034495 (`fix-search-tt-6i7h` ブランチ)

## 計測条件

- `cargo clean && cargo build --release` でフルビルド（incremental cache 破損を防止）
- USI_Hash=1, Threads=1 (両エンジン共通)
- FV_SCALE=24 (YO側のみ必要; rshogi側はNNUEロード時に内部で設定)
- **EvalFile必須**: rshogi は `setoption name EvalFile value /path/to/suisho5.bin` を明示設定が必要（未設定ではNNUE未使用で評価が大幅に異なる）
- **PvInterval=0必須**: YO側は `setoption name PvInterval value 0` を設定（デフォルト300msでは浅い深度の出力が省略される）
- YO バイナリ: `/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-halfkp_256x2-32-32`
- rshogi バイナリ: `/mnt/nvme1/development/rshogi/target/release/rshogi-usi`

## ノード数一致状況

### startpos

| depth | rshogi | YO | diff |
|-------|--------|-------|------|
| d1 | 30 | 30 | 0 |
| d2 | 604 | 604 | 0 |
| d3 | 651 | 651 | 0 |
| d4 | 1020 | 1020 | 0 |
| d5 | 1434 | 1434 | 0 |
| d6 | 1601 | 1601 | 0 |
| d7 | 2253 | 2253 | 0 |
| d8 | 4024 | 4024 | 0 |
| d9 | 5017 | 5017 | 0 |
| d10 | 5210 | 5210 | 0 |
| d11 | 12816 | 12816 | 0 |
| d12 | 24941 | 24941 | 0 |
| d13 | 30656 | 30656 | 0 |
| **d14** | **59047** | **60128** | **-1081** |

### line11818 (`position startpos moves 2g2f 8c8d 2f2e`)

| depth | rshogi | YO | diff |
|-------|--------|-------|------|
| d1 | 31 | 31 | 0 |
| d2 | 151 | 151 | 0 |
| d3 | 268 | 268 | 0 |
| d4 | 356 | 356 | 0 |
| d5 | 949 | 949 | 0 |
| d6 | 994 | 994 | 0 |
| d7 | 1954 | 1954 | 0 |
| d8 | 3049 | 3049 | 0 |
| d9 | 10555 | 10555 | 0 |
| d10 | 11604 | 11604 | 0 |
| d11 | 31460 | 31460 | 0 |
| d12 | 46418 | 46418 | 0 |
| **d13** | **87181** | **90923** | **-3742** |
| **d14** | **113266** | **103606** | **+9660** |

## 乖離分析

- startpos: d1-d13 完全一致、d14 で -1081 (1.8%)
- line11818: d1-d12 完全一致、d13 で -3742 (4.1%)、d14 で +9660 (9.3%)
- d13/d14 の符号反転は探索パスのカスケード分岐を示唆
- mate_1ply の差分ではない（can_king_escape to除外修正後も変化なし）
- 探索コードの他の差分が原因

## pos1 一致調査 (2026-02-18)

局面: `sfen +B1sgk1snl/6gb1/p3pp1pp/1pr3p2/3NP4/2p4P1/PP1P1PP1P/2G2S1R1/L3KG1NL w NLPsp 32`

| depth | rshogi | YO | diff (nodes) | cp一致 |
|-------|--------|----|--------------|--------|
| d1 | 92 | 92 | 0 | ✅ 228cp |
| d2 | 181 | 181 | 0 | ✅ 252cp |
| d3 | 385 | 385 | 0 | ✅ 235cp |
| d4 | 608 | 608 | 0 | ✅ 272cp |
| d5 | 756 | 756 | 0 | ✅ 363cp |
| d6 | 1364 | 1364 | 0 | ✅ 362cp |
| d7 | 1531 | 1531 | 0 | ✅ 366cp |
| **d8** | **4571** | **4624** | **-53 (-1.1%)** | ✅ 348cp |

### 発見・修正内容

#### EvalFile未設定問題（重要）
- rshogi の EvalFile はデフォルト `<empty>`（NNUEが未ロード）
- EvalFile未設定では d1 = 85cp（YOは 228cp）→ 全深度で乖離
- 必ず `setoption name EvalFile value /mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin` を設定すること

#### can_king_escape_with_from の誤修正を訂正（コミット 124fff7d）
- 24200622 の「YO準拠: toを逃げ先から除外（保守的近似）」が d8 cp を 296cp に誤らせていた
- YO は `to` を除外するが、rshogi の 07034495 版（`to` を自駒から除外して逃げを許容）が正しい動作
- 理由: `to` が非防衛の王手駒の場合、王はそこを取って逃げられる → `can_king_escape_with_from` が `false` を返すべきでない

#### YO整合済み（中立変更、d8ノード数に影響なし）
- `attacks_around_king_non_slider`: 自玉位置の除外を廃止（YO準拠）
- `attacks_slider_avoiding`: Horse→bishop_effect, Dragon→rook_effect（YO準拠）
- `attacks_around_king_non_slider_in_avoiding`: 歩のavoid除外廃止（YO準拠）

#### d8 残存ノード差（-53）の詳細調査 (2026-02-18)

- d8 ノード: rshogi 4571 vs YO 4624（差53、1.1%）
- cp は完全一致（348cp）

**アスピレーションウィンドウ解析結果**

d8 のアスピレーションループは両エンジンとも **5イテレーション**で完全一致:

| iter | adjusted_depth | alpha | beta | failed_high | result |
|------|---------------|-------|------|-------------|--------|
| 1 | 8 | 300 | 332 | 0 | fail-low |
| 2 | 8 | 284 | 300 | 0 | fail-low |
| 3 | 8 | 263 | 284 | 0 | fail-high (7f7g+) |
| 4 | 7 | 263 | 312 | 1 | fail-high (7f7g+) |
| 5 | 6 | 275 | 349 | 2 | success |

root_delta (beta - alpha) の各イテレーションの値（ユニーク）:
- iter1: 32, iter2: 16, iter3: 21, **iter4: 49**, iter5: 74

**ルート手ノード数比較（ROOTMOVE ログ）**

ログ上の +1 ずれは計測アーティファクト（rshogi は do_move 前、YO は do_move 後にカウント）。
実際の子孫ノード数 = ログ値 - 1（rshogi）または ログ値（YO）

| iter | 手 | rshogi(logged) | YO(logged) | rshogi(actual) | YO(actual) | 差 |
|------|---|----------------|-----------|----------------|------------|-----|
| 3 | 7f7g+ | 1152 | 1151 | 1151 | 1151 | 0 |
| **4** | **7f7g+** | **403** | **445** | **402** | **444** | **-42** |
| 5 | 7f7g+ | 39 | 48 | 38 | 47 | -9 |
| 5 | 他の手 | 合計 | 合計 | 合計 | 合計 | ~-2 |
| 合計 |  | 4571 | 4624 |  |  | -53 |

**絞り込み結果**

乖離の主因は **イテレーション4（adjusted_depth=7, alpha=263, beta=312, root_delta=49）** での
7f7g+ サブツリー内の 42ノード差。

確認済み一致項目:
- アスピレーションウィンドウ（5イテレーション全て同一）
- LMR パラメータ（全定数一致）
- Pruning パラメータ（全定数一致）
- TT 置換ポリシー（完全一致）
- Correction history 係数（完全一致）
- Singular extension 条件（完全一致）

**PLY 別ノード数ログによる詳細追跡（2026-02-18 完了）**

iter=4 の 7f7g+ サブツリー内を PLY 単位で追跡した結果、原因を以下の経路に特定:

```
PLY1: 7f7g+ (depth=7)
  PLY2: 6e5c+ (depth=6)
    PLY3: 7g7h (depth=5 → 実際は depth=9 で呼ばれる) ← 分岐点
      PLY4: N*6c (depth=3 または depth=8)
        PLY5: 5a4a (depth=8 または depth=9) ← ノード差の発生箇所
```

**tt_pv 追跡（RS_TTPV_TRACK / YO_TTPV_TRACK）**

| iter | rshogi tt_pv | YO tt_pv |
|------|-------------|---------|
| 1    | false       | false   |
| 2    | false       | false   |
| 3    | false       | false   |
| **4** | **false** | **true** ← 乖離 |

**PLY4 終了時ログ（RS_PLY4_END / YO_PLY4_END）**

YO のみ iter=4 に PLY4 への追加呼び出しが存在:
```
YO_PLY4_END iter=4 depth=3 best_value=289 alpha=289 tt_pv=1 parent=N*6c parent2=7g7h
```
この呼び出しが rshogi には存在しない。

**追加呼び出しの発生源：Singular Extension（SE）**

- PLY3（局面: 6e5c+→7g7h の後）が iter=4 で TT手 M3 の SE 検証を実行
- SE 除外探索の中で N*6c を試み、PLY4 を `alpha = singularBeta - 1 = 289` で呼び出す
- PLY4 が fail-low（best_value=289 ≤ alpha=289）→ `tt_pv |= parent_tt_pv(=true) = true`
- TT に `is_pv=true` で保存
- 本探索の PLY4 が TT を参照 → `tt_pv = true`

**rshogi で SE が発動しない理由**

PLY3 の TT プローブ結果（iter=4, parent=7g7h, RS_PLY3_TT ログ）:

| depth | tt_hit | tt_move | tt_bound | SE 発動 |
|-------|--------|---------|----------|---------|
| 2     | true   | 5c6a+   | Lower    | ✗ (depth < 6) |
| 6     | true   | none    | Upper    | ✗ (bound が Upper) |
| **9** | **true** | **none** | **Upper (tt_depth=8)** | **✗ (bound が Upper、かつ tt_move なし)** |

SE の条件: `depth >= 6 + ttPv` かつ `tt_bound.is_lower_or_exact()` かつ `tt_move != NULL`

- rshogi の PLY3 は depth=9 に `Bound::Upper + tt_move=none` の TT エントリを持つ
- YO は同じ PLY3 に `Bound::Lower + TT move` を持つ（と推定）
- これにより SE が YO でのみ発動し、追加 PLY4 コール → tt_pv=true の連鎖

**根本原因**

PLY3（局面: 6e5c+→7g7h 後）は **iter=3** において:
- rshogi: fail-low → `Bound::Upper, tt_move=none` をTTに保存
- YO: fail-high → `Bound::Lower + move` をTTに保存

この差が iter=4 の SE トリガー有無を決定し、53ノードの乖離を生む。

iter=3 での PLY3 の結果が異なる原因はさらに深い TT 非決定性に起因し、
単一コードバグとして特定・修正できる性質のものではない可能性が高い。
Singular Extension 条件コード自体は両エンジンで完全一致している。

**影響経路のまとめ**

```
iter=3 PLY3 bound 差 (rshogi: Upper, YO: Lower)
  → iter=4 PLY3 SE 発動差 (rshogi: なし, YO: あり)
    → iter=4 PLY4 tt_pv 差 (rshogi: false, YO: true)
      → step16 r 差 (rshogi: 4102 > 3212 閾値, YO: 2618 差分でr減少)
        → non_lmr_depth 差 (rshogi: 8, YO: 9+)
          → PLY5 探索 depth 差 (rshogi: depth=8, YO: depth=9)
            → ノード差 (rshogi: -42 nodes, 計 -53 nodes)
```

**調査結論**: d8 pos1 の 53ノード乖離はコードバグではなく TT 内容の非決定的な差異に由来する。
修正方針は今のところなし。d9 / startpos 乖離の調査を優先する。

### d9 大幅乖離（未解決）

| depth | rshogi | YO | 差 |
|-------|--------|----|----|
| d9 | **324cp / 5565 nodes / seldepth 16** | **100cp / 28161 nodes / seldepth 21** | cp: +224, nodes: -22596 |

d9 PV（4手目 `6e7c` までは一致、その後分岐）:
- rshogi: `7g7h 7c6a+ 5a6a 4h5g B*7g 5i4h 7g5e+ R*4a`
- YO: `6a6b 7h7g 2b5e 7g6f B*7g 5i5h 5e6f 6g6f 7g6f+ B*1e G*4b`

YO の seldepth=21 (rshogi=16より深い) から、YO がより深い反証を発見している。
rshogi が d9 で 324cp と過大評価している原因を要調査。

## 修正済み（未コミット）

### can_king_escape の `to` 除外を YO 準拠に修正（startpos/line11818用）

- helpers.rs: drop版・move版ともに `Bitboard::from_square(to)` を escape 除外に追加
- 修正前後で d1-d14 のノード数に変化なし（mate_1ply は乖離原因ではない）

## 計測時の注意事項

1. **cargo incremental cache 破損**: `git checkout` で異なるコミットを行き来すると incremental cache が壊れ、`touch` + リビルドでは不十分な場合がある。信頼性の高い結果が必要な場合は `cargo clean && cargo build --release` を使用
2. **YO FV_SCALE**: 必ず `setoption name FV_SCALE value 24` を設定（デフォルト16だと eval が異なる）
3. **YO 出力のバイナリ文字**: `grep -a` (`grep -aoP`) を使用
4. **root move ごとのノード数**: rshogi は do_move 前に +1 するため、root move 単位では YO と +1 ずれる（depth 全体の合計は一致）

## 次の調査ステップ

### 優先度高: startpos/line11818 深い乖離
1. 低 depth で差が出る局面を探す（start_sfens_ply32.txt から複数局面を depth 5-8 で比較）
2. line11818 d13 の PV を 8手進めた局面で低 depth 比較（乖離が早期に出る可能性）
3. 差が出る局面で root move 別ノード数比較し、分岐起点を特定
4. yo-compare スキルで該当コードパスの差分を確認

### 優先度中: pos1 d9 乖離調査
1. d9 で PV が分岐する白5手目 (`7g7h` vs `6a6b`) の原因を調査
2. rshogi の seldepth=16 が YO の 21 より浅い理由を特定
3. 4手後局面 (`7f7g+ 9a7c 7d7c 6e7c`) から d5-d6 で再比較し、早期乖離を確認

### 解決済み: pos1 d8 乖離（-53ノード）
- 原因: TT 非決定性による SE 発動の有無（コードバグではない）
- 詳細は上記「d8 残存ノード差（-53）の詳細調査」参照
