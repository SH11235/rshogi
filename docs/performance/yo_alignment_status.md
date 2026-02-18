# YaneuraOu ノード数一致調査 ステータス

最終更新: 2026-02-18
コミット: 124fff7d (`fix-search-tt-6i7h` ブランチ)

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

#### d8 残存ノード差（-53）
- d8 ノード: rshogi 4571 vs YO 4624（差53、1.1%）
- cp は完全一致（348cp）
- 原因未特定（TT実装差、ムーブオーダリング差などの可能性）

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

1. 低 depth で差が出る局面を探す（start_sfens_ply32.txt から複数局面を depth 5-8 で比較）
2. line11818 d13 の PV を 8手進めた局面で低 depth 比較（乖離が早期に出る可能性）
3. 差が出る局面で root move 別ノード数比較し、分岐起点を特定
4. yo-compare スキルで該当コードパスの差分を確認
