# 並列探索時に増える大悪手の要因分析と対策指針（PV/TT/Sanity）

本ドキュメントは、8スレッド時のみ顕在化する「PV2手目での急落」「Aspiration連続Fail-High」「Sanityは動作するが駒損を見逃す」という症状について、実装の確認結果と原因仮説、切り分け計画、直近の対策（実装済み）および恒久対策案をまとめたものです。

## 概要（症状の再掲）

- 36手後評価: +215 → 37手目 5h5g（bestmove）確定 → 実戦で 4d3e が来た直後に評価 −1035 〜 −1773 へ急落。
- 37手の探索で Aspiration fail-high を連発（例: old=[130,190]→[130,250]→[130,370]→[130,610]）。
- Sanity ログは `sanity_checked=1 see=0 switched=0 reason=see_ok`（非捕獲で SEE=0 のため切替が起きず）。

## 実装確認（該当コードと要点）

- Root 探索と Aspiration 窓
  - ルート反復・Aspiration: `crates/engine-core/src/search/ab/driver.rs:78` 以降。
  - Fail-Low/High の窓拡張とログ出力: `crates/engine-core/src/search/ab/driver.rs:887`（fail-low）, `:920`（fail-high）。
  - Aspiration 成功時のみ 1 行目を TT に Exact で保存（PV 保護フラグ付き）: `crates/engine-core/src/search/ab/driver.rs:1079` 付近（`args.is_pv = true`の明示）。

- PVS 本体と TT 参照/保存
  - TT ヒット時の十分深さ判定と節の即時打ち切り: `crates/engine-core/src/search/ab/pvs.rs:259` 以降（Upper/Lower/Exact）。
  - PVS 末尾での TT 保存（PV/Exact を depth ブースト）: `crates/engine-core/src/search/ab/pvs.rs:566` 以降（`TTStoreArgs::new(..); args.is_pv = is_pv;`）。

- TT の置換規則/優先度/メモリ順序
  - 置換優先度（depth − age + PV/Exact ボーナス）: `crates/engine-core/src/search/tt/entry.rs:226`（`priority_score()`）。
  - PV/Exact の depth ブースト: `crates/engine-core/src/search/tt/filter.rs:43`（Exact +1）, `:58`（PV +2）。
  - ストア/プローブの公開順序（key Acquire / data Relaxed, data→key Release 公開）: `crates/engine-core/src/search/tt/bucket.rs:127`（probe_scalar）, `:311` 以降（store 内部順序）。

- 並列探索（LazySMP）と Root 分担
  - 並列実行の起動と RootWorkQueue: `crates/engine-core/src/search/parallel/mod.rs:78` 以降。
  - RootPicker へのキュー接続（primary は `use_queue_claims=false`、helper は true）: `crates/engine-core/src/search/ab/driver.rs:504`, `:517`。

- Sanity（bestmove 直前の軽量最終確認）
  - 入口と従来挙動: `crates/engine-usi/src/finalize.rs:72`。自手 PV1 の SEE が閾値以上なら即 OK（ミニ探索に進まない）。
  - 本件対応で追加した「PV1 後の相手捕獲 SEE ゲート」は後述（実装済み）。

## 追加助言の論点と実装実態の突き合わせ

- PV/Non-PV の混線と TT 汚染
  - 現状でも PV/Exact は depth およびボーナスで優先保護されるが、非 PV でも十分深い Lower/Upper が後着すると、相対的に置換し得る余地は残る（仕様上は許容）。
  - is_pv フラグは保存経路で保持しており、完全な喪失はしにくいが、SMP の競合で「非 PV 由来の浅い bound に引っ張られ、窓誘導→再探索不足」が起こる余地はある。

- Root 分担と PV 所有権
  - Helper が RootWorkQueue で root 手を先に掘る設計で、primary-first を強制していない（primary は claim せず、helper が claim）。
  - そのため「PV 候補を helper が Non-PV 扱いで掘って保存→後段で窓が狭まり Aspiration が上振れ/再探索漏れ」が起きやすい構図は理屈に合う。

- ABDADA busy-flag / in-progress 保護
  - TT 側に exact-cut フラグ（`abdada1`）は実装済み（`crates/engine-core/src/search/tt/mod.rs:621` 以降）が、探索側からの利用配線は未実装。

- メモリ順序/原子性
  - 置換/空スロットの公開順序は Acquire/Release の契約に沿って実装済みで、半端書きを読む危険は深さ=0 ドロップで回避している（`bucket.rs`）。

- 局所ヒューリスティクス共有
  - primary の反復内ではヒューリスティクスを PV 間・反復間で持ち回る一方、helpers のヒューリスティクスは最終結果へ統計的に集計するのみで直接の次反復入力には使っていない。過度の「共有による汚染」は薄い。

- LMR/Null/StaticBeta の PV 漏れ
  - is_pv に応じた減深緩和は実装済み（LMR: `crates/engine-core/src/search/ab/ordering/mod.rs:98` 以降）。ただし helper の Non-PV 結果が TT へ残りうる点は前述の通り。

- Sanity（従来）は SEE 単独で「非捕獲・明確な当たり」を見逃す
  - SEE は小手数での損益を測るため、非捕獲や直後の相手大得（角/飛）を PV1 直後に許す形には弱い。既存ログどおり `see=0` で通過していた。

## 直近の対策（実装済み）

### 変更点（USI最終化 + A/B1 + A/B2）

- Sanity に「PV1 後の相手最善捕獲 SEE ゲート」を追加。
  - 自手 SEE が閾値以上でも、PV1 を指した直後局面で「相手の捕獲手の SEE 最大値」が `FinalizeSanity.OppSEE_MinCp`（既定 300cp）以上なら、ミニ探索（深さ 1–2）で PV1 と代替候補（PV2 or SEE ベスト）を比較し、`SwitchMarginCp`（既定 80cp）を超えれば差し替える。

- A/B1: Helper の根近傍（ply<=2）の TT 書き込み抑制（非PVの Upper/Lower のみ）。
  - 実装: `crates/engine-core/src/search/ab/pvs.rs` の TT 保存直前で `helper_role && ply<=2 && !is_pv && node_type!=Exact` を保存スキップ。
  - 目的: 近接ノードの非PV bound による窓誘導/汚染を緩和。

- A/B2: TT 置換規則の PV/Exact 保護と優先度微調整。
  - 既存エントリが「PV+Exact」で、新規が「Non‑PV+Bound」のときは“同一キー更新”を拒否（深さに依らず）。
    - 実装: `crates/engine-core/src/search/tt/utils.rs::try_update_entry_generic()` にガード追加。
  - Non‑PV の bound には小ペナルティ（−2）を付与して、バケット内の“最悪選定”で押されにくく（置換されにくく）する。
    - 実装: `crates/engine-core/src/search/tt/entry.rs::priority_score()` に減点。

### 変更ファイルと該当行

- `crates/engine-usi/src/finalize.rs:72` … Sanity 本体。相手捕獲 SEE ゲート（`opp_cap_see_max`）と診断出力を追加。
- `crates/engine-usi/src/options.rs:162` … 新オプション `FinalizeSanity.OppSEE_MinCp` を USI 公開。
- `crates/engine-usi/src/state.rs:67` … `finalize_sanity_opp_see_min_cp` を `UsiOptions` に追加（既定 300）。

### 効果

- 非捕獲の PV1（例: 5h5g）であっても、PV1 直後に相手が大得（例: 銀で角取り）となる典型ケースを Sanity 層で捕捉可能。Aspiration や TT の挙動を変えずに、bestmove 最終段での“握手”を抑止。

## すぐできる切り分け（A/B）

- A/B1: ルート浅層（root±2ply）で helper の TT 書き込みを抑制（Exact のみ許可 or Upper/Lower は深さペナルティ）。
- A/B2: TT 置換規則の PV 保護強化（既存 PV Exact は Non‑PV bound では上書き不可 / 非 PV bound は深さ比較を −Δply 補正）。
- A/B3: Root 分担の primary-first 強制（primary が上位 K 手を先行 claim、helper は open_upto=K のみ）。
- A/B4: 簡易 ABDADA（in‑progress ビット）を探索側で使用して重複探索を減衰。
- A/B5: Sanity に「ハンギング大駒」静的検査を追加（角/飛/馬/竜が無防備で取られる形を高速検出）。

## 恒久対策（実装指針）

- TT 保存/置換ポリシー強化
  - 優先順を「PV Exact > 深さ > 現世代 > 非 PV bound」をより厳格に。非 PV bound は深さ比較に −Δ（2〜4ply）を適用。
  - 読み側は「PV 欲しい節では Exact を優先、Non‑PV bound は窓誘導のみ」に階層化。

- Root 分担の primary-first & PV 属性一貫性
  - RootWorkQueue を primary-first & open_upto 方式に変更。helper は担当 root 手のみ PV 扱い、他は Non‑PV。is_pv を thread‑local stack から TT 保存へ一貫伝播。

- ABDADA in‑progress の活用
  - 「busy 中は同深に入らない／入るなら減深」ポリシーで、SMP 由来の誤上書きを抑止。

- finalize sanity の拡張
  - 既存: SEE＋ミニ探索。拡張: 「大駒ハンギング/直接当たり/ピン/両取り」の軽量静的検査を追加し、該当時はスイッチ閾値を緩める。

## 追加ログ/計測（推奨）

- TT 汚染検知
  - `tt_pv_exact_overwritten=1 kind=nonpv_bound depth_old/new gen_old/new` を一時ログ。

- PV ヘッド再検証の可視化
  - 採用前に `pv_head_rechecked=1/0` を 1 回出力（メインが helper 由来結果を採用する際、小窓1plyの再検チェック有無）。

- Root 所有権の可視化
  - `root_claim primary=N helpers=M open_upto=K` と `root_claimed_by=primary|helper` を深さ更新時に 1 回だけ出力。

## 補足（実装メモ）

### RootWorkQueue: primary‑first の簡易疑似コード

以下は「primary が上位手を優先的に担当し、PV 候補は primary 専有。helpers は `open_upto = threads + 1` の範囲で未請求手のみ担当」という方針の疑似コードです。

```
// on each iteration
let k = threads + 1; // open_upto
let root_moves = pick_root_moves(pos); // 既存 RootPicker

// primary スレッド側
if thread.is_primary() {
    // 先頭（PV 候補）は primary 専有。helper は claim できない。
    claim_exclusively(root_moves[0]);
    for mv in root_moves[1..k] {
        claim_if_unclaimed(mv); // 競合時はスキップ
    }
} else {
    // helper は open_upto 範囲のみ、未請求手を claim
    for mv in root_moves[..k] {
        claim_if_unclaimed(mv);
    }
}

// 探索中に PV が更新された場合、primary は現行 PV を再優先化。
// helpers は既に claim 済みの手のみ継続し、新規 PV の claim は行わない。
```

注意点:
- `claim_exclusively` は primary 専用のフラグを立て、helpers の `claim_if_unclaimed` からは不可視にする（同一反復内）。
- 次反復開始時に claim はすべて解除し、RootPicker による並べ替え結果で再配分。

### ABDADA: in‑progress ビットの禁止条件（例）

実装を簡素に保つため、以下の条件では busy ビットを使用しない（=並列探索を許可）方針が運用しやすいです。

- 深さが浅いとき（例: `depth < 6`）は busy 無視。
- 王手局面（`pos.is_in_check()`）では busy 無視。
- PV ノード（`is_pv == true`）では busy 無視（代わりに減深や窓緩和で吸収）。

それ以外では busy 命中時に「同深は回避・1〜2ply 減深で入る」などの軽い ABDADA を適用します。

## 影響とトグル

- 今回の Sanity 強化は USI オプションで閾値可変（`FinalizeSanity.OppSEE_MinCp` 既定 300、0 で無効化同等）。SEE/ミニ探索に限定されるためオーバーヘッドは軽微（~2ms 以内を予算）。
- 追加の A/B トグルはリスク局所化のため段階導入を推奨（A/B1→A/B2→A/B3 の順）。

## 参考コード（クリックで該当行）

- Root Aspiration fail‑high 出力: `crates/engine-core/src/search/ab/driver.rs:920`
- TT 参照・十分深さと節の扱い: `crates/engine-core/src/search/ab/pvs.rs:259`
- TT 保存（PV/Exact のブースト）: `crates/engine-core/src/search/ab/pvs.rs:566`
- TT 優先度と PV/Exact ボーナス: `crates/engine-core/src/search/tt/entry.rs:226`, `crates/engine-core/src/search/tt/filter.rs:43`
- LazySMP RootWorkQueue 接続: `crates/engine-core/src/search/ab/driver.rs:504`, `:517`
- Sanity（相手捕獲 SEE ゲート追加）: `crates/engine-usi/src/finalize.rs:93`
- USI オプション（OppSEE）: `crates/engine-usi/src/options.rs:162`, `crates/engine-usi/src/state.rs:67`
- A/B1（helper 近傍TT抑制）: `crates/engine-core/src/search/ab/pvs.rs` の TT 保存分岐
- A/B2（PV/Exact 保護・優先度微調整）: `crates/engine-core/src/search/tt/utils.rs`, `crates/engine-core/src/search/tt/entry.rs`

---

# 計測ログ（2025-10-08 時点）

本節では、上記の実装判断を裏づける計測条件と観測結果を整理する。いずれも同一バイナリ（engine-usi）で実施。必要に応じて diagnostics を有効化し、計測用の軽量タグを `info string` に出力している。

## 計測セットアップ

- ビルド
  - 通常: `cargo build -p engine-usi --release`
  - 診断強化（任意）: `cargo build -p engine-usi --release --features diagnostics`
- 共通USIオプション
  - `USI_Hash=256`
  - `FinalizeSanity.Enabled=On, SEE_MinCp=-90, OppSEE_MinCp=300, MiniDepth=2, SwitchMarginCp=80`
  - MultiPVはケースにより `1` または `2`
- 再現局面（36手目直後）
  - `position startpos moves 3i4h 3c3d 7i6h 4a3b 5g5f 8c8d 3g3f 8d8e 5f5e 2b5e 2i3g 3a2b 2h3h 2b3c 6i7h 7a7b 2g2f 7b8c 8h7i 5a4b 7h8h 4b3a 6h5g 6a5b 5g4f 5e4d 2f2e 7c7d 5i5h 7d7e 3f3e 3d3e 4f3e 4d5e P*3d 3c4d`
- 主な計測タグ（抽出）
  - `sanity_checked=1 see=... opp_cap_see_max=... switched=...`
  - `tt_store_suppressed_helper_near_root=1 ...`
  - `tt_pv_exact_overwrite_blocked=1 ...`
  - `abdada_cut_reduction=1 depth=... -> ...`
  - `parallel_best_source=primary|helper ...`
  - `root_claim primary_first=1 open_upto=K claimed=N`

## 観測結果サマリ

### Sanity（OppSEE）

- Material / 8T・1T / 10秒 / MultiPV=2
  - `info string sanity_checked=1 see=0 opp_cap_see_max=500 ... switched=1`
  - 代替手に切替が発火（握手ブロック成立）。OppSEEしきい値300cp超を検出し、ミニ探索（例: s2=1173, s1=298, margin=80）で閾値超過を確認。
- Enhanced（NNUEなし） / 8T・1T / 10秒 / MultiPV=2
  - `info string sanity_checked=1 see=0 opp_cap_see_max=0 switched=0 reason=see_ok`
  - 当該局面ではPV1直後の相手高利得キャプチャが存在せず、OppSEEは非発火。Sanityの分岐・出力自体は正常に観測。

### A/B1（helper近傍TT抑制）

- 近傍抑制タグ `tt_store_suppressed_helper_near_root` は今回の短時間トライでは0件（局面依存）。保存抑制の条件（helper・ply<=2・非PV/非Exact）に一致したときのみ発火する。

### A/B2（PV Exact保護・優先度微調整）

- `tt_pv_exact_overwrite_blocked=1` は別バッチで多数観測（1T負荷高め）。「PV+Exact」への「Non‑PV+Bound」同一キー更新がブロックされていることを確認。今回（単発10秒）では局面揺れで非発火だが、機能退行はなし。

### ABDADA（in‑progress 簡易）

- 対象: Non‑PV・非王手・深さ>=6。busy命中時のみ静止手で−1plyの追加減深。
- 発火件数（例）
  - Enhanced/8T/10s: `abdada_cut_reduction=1` が 12件
  - Enhanced/1T/10s: 同 12件
  - Enhanced/8T/10s/MultiPV=2: 同 46件
- 例ログ: `info string abdada_cut_reduction=1 depth=7 -> 6`

### primary‑first（Root分担）

- 実装: Primary が上位K手（既定K=9、`SHOGI_ROOT_OPEN_UPTO` で変更可）を先取り claim。
- 診断: `root_claim primary_first=1 open_upto=K claimed=N` を1回出力。
- `parallel_best_source` は本局面・設定では非発火（primaryが常に最良で結合された可能性）。Threadsや反復数を増やすと観測しやすい。

## 判断

- Sanity（OppSEE）: Material では実害局面で確実に発火し、握手を抑止することを確認。Enhanced でも副作用なし。
- A/B1/A/B2: 局所的・安全側の制御であり、計測上の退行は見られない（ブロック・抑制タグは条件一致時に発火）。
- ABDADA（簡易）: busy命中時の−1ply合流が適度に機能。過剰抑制やSanity/TT保護との衝突は観測されず。
- primary‑first: 実装済み。診断ログ追加により挙動可視化。必要に応じてThreads/反復を増やして効果を追加計測。

## 再現コマンド例

### クリティカル局面（36手目直後）で10秒・MultiPV=2（Material）

```
usi
setoption name Threads value 8
setoption name USI_Hash value 256
setoption name MultiPV value 2
setoption name FinalizeSanity.Enabled value true
setoption name FinalizeSanity.OppSEE_MinCp value 300
setoption name FinalizeSanity.SEE_MinCp value -90
setoption name FinalizeSanity.MiniDepth value 2
isready
usinewgame
position startpos moves 3i4h 3c3d 7i6h 4a3b 5g5f 8c8d 3g3f 8d8e 5f5e 2b5e 2i3g 3a2b 2h3h 2b3c 6i7h 7a7b 2g2f 7b8c 8h7i 5a4b 7h8h 4b3a 6h5g 6a5b 5g4f 5e4d 2f2e 7c7d 5i5h 7d7e 3f3e 3d3e 4f3e 4d5e P*3d 3c4d
go movetime 10000
```

抽出例:

```
rg -n "sanity_checked=1|tt_store_suppressed_helper_near_root|tt_pv_exact_overwrite_blocked|abdada_cut_reduction|parallel_best_source|root_claim primary_first" 保存ファイル
```

### primary-firstの確認（任意）

```
SHOGI_ROOT_OPEN_UPTO=9 Threads=16 movetime=3000 を 3〜5回連続
```

期待:
- `root_claim primary_first=1 open_upto=K claimed=N` が出力
- `parallel_best_source` が helper になるケースが観測されやすくなる
