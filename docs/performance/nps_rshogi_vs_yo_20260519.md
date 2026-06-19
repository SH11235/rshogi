# rshogi エンジン vs YaneuraOu NPS 計測 (2026-05-19)

> allfp16-r3 モデル評価の派生で実施した、rshogi エンジンと YaneuraOu の NPS 比較記録。
> きっかけ: YO 評価に使っていた YO バイナリが推論部最適化 (piece-list-cache) を
> 含むか確認 → 含まないと判明 → 最適化版をビルドして比較。

## piece-list-cache 最適化の発見

- YO 評価 (allfp16-r3 の sb-180/300/400 比較) で使った YO バイナリ
  `YaneuraOu-sfnnwop1536-v2` は **2026-03-26 20:46 ビルド**。
- piece-list-cache 最適化コミット `1710ade0`「perf(nnue): AccumulatorCache を
  piece_list 差分方式に置換」は **2026-04-16** → 旧バイナリは最適化を**含まない**
  （3 週間前のビルド、確定）。
- 棋力結果 (allfp16-r3 vs bullet-v102 の +14 nElo 等) は両エンジン同一バイナリ使用で
  最適化は対称に効くため**影響なし**。NPS の数字のみ最適化前の値だった。

## ビルド (最適化版)

`feat/piece-list-cache` ブランチ (HEAD `50ba5511`, 2026-04-24) からリビルド:

```bash
cd /path/to/YaneuraOu/source
make clean
make -j16 tournament \
  TARGET_CPU=AVX2 \
  YANEURAOU_EDITION=YANEURAOU_ENGINE_NNUE_SFNNwoP1536_V2 \
  COMPILER=clang++ PYTHON=python3
cp -p YaneuraOu-by-gcc YaneuraOu-sfnnwop1536-v2-plcache
```

- `YANEURAOU_ENGINE_NNUE_SFNNwoP1536_V2` = v102 互換 (1536x16x32 LayerStack +
  progress8kpabs)。`_V2_32x32` は 1536x32x32 用なので不可。
- 成果物: `/path/to/YaneuraOu/source/YaneuraOu-sfnnwop1536-v2-plcache`
  (id name `YaneuraOu NNUE 9.22git 64AVX2 TOURNAMENT`)。旧 `sfnnwop1536-v2` は温存。

## 計測条件

- ツール: `tournament` / `analyze_selfplay`、avg_nps を採取。
- モデル: allfp16-r3-400 / bullet-v102-400 (v102 1536x16x32, progress8kpabs)。
- byoyomi 1000ms / threads 1 / hash 256MB / startpos ply32。
- rshogi エンジン: `rshogi-usi-1536x16x32-v100v101cmp`（EvalFile 方式）。
- YO: `EvalDir` 方式（`<dir>/nn.bin` symlink）、FV_SCALE=28 / progress8kpabs /
  USI_OwnBook=false。

## 結果: concurrency 15（90局、訓練終了後）

| エンジン | avg_nps | avg_depth |
|---|---|---|
| rshogi-usi-1536x16x32-v100v101cmp | **~487K** | ~21.6 |
| YaneuraOu-sfnnwop1536-v2-plcache（最適化版）| **~584K** | ~20.8 |

→ 同条件（concurrency 15・訓練終了後・90局）で **最適化 YO が rshogi を約 20% 上回る**
raw NPS。

## 注意・交絡事項（重要）

- **旧 YO ~448K は比較に使えない**: allfp16-r3 の sb-400 評価 (4328局) 中に観測した
  旧 YO の avg_nps ~448K は、**訓練 (nnue-train) が並走**して CPU ~12.8 コアを消費
  する競合下の値。最適化版 ~584K は訓練終了後の測定。両者は競合条件が違うため
  「最適化で +30%」とは言えない（最適化効果＋訓練競合解消が混在）。piece-list-cache
  単体効果の isolation には、最適化前コミット `1710ade0^` を同一コマンドでビルドした
  A/B が必要（今回は未実施）。
- **NPS と探索深度は別物**: rshogi は低 NPS でも深く読む (depth 21.6 vs YO 20.8)。
  node 定義・枝刈りがエンジン間で異なるため、NPS 単独で「強さ・速さ」を語れない。
- **tail inflation**: self-play 90局を concurrency 15 で回すと約 6 wave。終盤の wave は
  同時対局数が減り競合が下がるため avg_nps がやや高めに出る。rshogi/YO とも同じ構造
  なので両者の相対比較は公平。
- **メモリ帯域律速**: concurrency 15 では NNUE 推論がメモリ帯域で律速し、コア数を
  増やしても NPS は伸びにくい。

## 方法論メモ: burst NPS vs sustained NPS

- readyok チェックの短い `go`（fair-time オプション未設定で ~100ms で打ち切られる）の
  `nps` は、単独実行・短窓・浅い探索・cold TT のため**実測の ~6-7 倍**高く出る
  （例: 単独 burst ~3M 級 vs 実戦 avg_nps ~0.5M 級）。
- 実戦相当の持続 NPS は **tournament の `avg_nps`**（フル 1 秒探索 × 数千手 ×
  多様な局面 × 並列競合込み）で測ること。

## 結果: concurrency 5 直接対決（16局）

rshogi vs 最適化YO の直接対決（両者 allfp16-r3-400、16局、concurrency 5）。同一対局・
同一局面で両エンジンの NPS / depth を取得した最もクリーンな比較。

| エンジン | avg_nps | avg_depth | avg_nodes |
|---|---|---|---|
| rshogi-usi-1536x16x32-v100v101cmp | ~657K | 22.19 | ~599K |
| YaneuraOu-sfnnwop1536-v2-plcache | **~781K** | **23.86** | ~652K |

→ 低競合 (concurrency 5) でも **最適化 YO が rshogi を NPS で約 19% 上回り、探索深度も
深い**（23.9 vs 22.2）。同一局面での比較なので depth も直接比較可能で、YO が「速くかつ
深い」。concurrency-15 で見えた「rshogi が深い」は、自己対局の局数サンプル差（rshogi run の
平均手数が長く終盤局面が多かった）による交絡で、同一局面の本対決では解消。

（対局結果は rshogi 6勝 / YO 9勝 / 1分 = Elo -66 ±173。両者同一モデルなので純粋な探索
強度差だが、16局では誤差過大で有意差なし。NPS 計測が主目的のため参考値。）

## 総括

| 条件 | rshogi avg_nps | 最適化YO avg_nps | YO/rshogi |
|---|---|---|---|
| concurrency 1（6局・直接対決・無競合）| ~715K | ~837K | YO +17% |
| concurrency 5（16局・直接対決）| ~657K | ~781K | YO +19% |
| concurrency 15（90局・自己対局・訓練終了後）| ~487K | ~584K | YO +20% |

- **最適化 YO は rshogi より全並列度で約 17〜20% 速い**（raw NPS）。並列度が上がるほど差は
  わずかに拡大。
- **並列による NPS 崩落**: concurrency 1→15 で rshogi −32%（715K→487K）、YO −30%
  （837K→584K）。各プロセスが NNUE 重みの独立コピーを持つことによるメモリ帯域・L3 競合。
  崩落の大半は c5→c15 区間（c1→c5 は rshogi −9% / YO −7% と小さい）。
- 当初の暫定値「rshogi ~470–487K vs 旧YO ~448K」で rshogi 優勢に見えたのは、旧 YO 値が
  「piece-list-cache 最適化前 × 訓練並走の競合下」の二重ハンデだったため。最適化版・
  同条件で測ると全並列度で YO が優位、が正しい結論。
- piece-list-cache 最適化の単体寄与（GitHub 言及の +7〜8%）は、最適化前コミットを同一
  ビルドで測る A/B を実施していないため本記録では分離不可。
- 注: c1 は 6 局のみで depth は標本ノイズ大（NPS は ~760 手平均で比較的安定）。

## 関連: NNUE プロセス間パラメータ共有（YaneuraOu commit 2a4cb3d）

yaneurao/YaneuraOu の commit `2a4cb3d`「NNUE、プロセス間パラメーター共有を実装」は、
本記録で観測した「並列 NPS 崩落（−30%級）」を直接潰す変更:

- 機構: `SystemWideSharedConstant<NnueNetworks>` — 処理済み重み（FeatureTransformer +
  Network[9]）を内容ハッシュ命名の共有メモリに配置し、同一 eval を読む全プロセスが
  1 つの物理コピーを参照。ロード後 const ＝読み取り専用共有でロック不要。自動・
  USI オプション追加なし・確保失敗時はローカルフォールバック。
- 効果: N プロセスでも重みは 1 コピー → 共有 L3 に 1 回だけ載る → 帯域競合が激減し
  高並列でも NPS が落ちにくい。メモリ常駐も N× → 1×。
- 影響範囲: マルチプロセス運用（トーナメント harness、N プロセス×1スレッドのバッチ
  解析、クラスタ探索）限定。単一対局・スレッド並列には無関係。
- 本記録の YO バイナリ（fork `feat/piece-list-cache` 04-24 ビルド）はこのコミットを
  含まない → 本計測は rshogi/YO とも独立コピーで公平。ただし YO がこれを取り込むと
  多プロセス計測・対局では YO の実効優位が拡大する。
- **rshogi にも実装可能**（POSIX shm + 内容ハッシュ命名、処理済み重みを明示共有。
  v102 形式は FT 重みが LEB128 圧縮のため生ファイル mmap では不可、展開後を共有する
  方式が必要）。効くのは rshogi 自己対局・SPRT スループット（1 対局の棋力には無関係）。
  実装判断は崩落幅（本記録の −32%）を踏まえ measurement-first で。

## 実装ブリーフ: rshogi への移植（着手当時の設計メモ）

> **✅ 2026-05-19 実装完了（PR #714）。最新の成果・実測値は後述の「実装結果」節を参照。**
> 本節は着手当時の設計メモとして履歴的に残す。

> 新規セッション用の着手ブリーフ。「rshogi リポジトリで起動し、本ドキュメントの
> 『実装ブリーフ』節に沿って NNUE プロセス間パラメータ共有を実装して」で開始できる。

### 目標

rshogi エンジンに NNUE 評価パラメータのプロセス間共有を実装し、多プロセス時の
NPS 崩落（本記録: concurrency 1→15 で −32%）を回復する。受益は自己対局・SPRT・
評価パイプラインのスループット。**1 対局の棋力には無関係**（探索結果は不変であること）。

参照: YaneuraOu commit `2a4cb3d`（本ドキュメント「## 関連」節）。機構は
`SystemWideSharedConstant<NnueNetworks>` — 処理済み重みを内容ハッシュ命名の共有メモリに
置き、同一 eval を読む全プロセスが 1 物理コピーを参照、read-only const でロック不要。

### 着手ポイント（rshogi-core）

- `crates/rshogi-core/src/nnue/network_layer_stacks.rs` — `NetworkLayerStacks`（YO の
  `NnueNetworks` 相当、共有対象本体）
- `feature_transformer.rs` / `layer_stacks.rs` — 重み中身（`FeatureTransformer` /
  `LayerStacks` / `LayerStackBucket`）
- `leb128.rs` — ロード時 LEB128 展開（**生ファイル mmap 不可**の根拠。展開後を共有する）
- eval ファイル読み込みエントリ（`evaluator.rs` 等。`NetworkLayerStacks` を構築する
  関数を特定すること）

### 設計スケルトン

1. **前提確認（最初にやる）**: 共有対象の重み構造体が POD 化可能か。`Vec`/`Box`/参照を
   内部に持つなら直接 shm 配置不可 → 固定長配列ベースの POD 表現が必要。const generics
   ベース（`NetworkLayerStacks<const ...>` / `LayerStackBucket<const ...>`）なので固定長
   配列で完結している可能性が高いが要検証。`#[repr(C)]` とレイアウト安定性を確認。
2. **shm holder**: POSIX `shm_open` + `ftruncate` + `mmap(MAP_SHARED)`。Rust は `nix`/
   `libc` か `memmap2`。展開後の重みを書き込み、以後 read-only。
3. **内容ハッシュ命名**: eval ファイル内容（または展開後重み）のハッシュでセグメント名
   （例 `/rshogi-nnue-<hash>`）。同一 eval → 同一名 → 自動共有。
4. **create-or-attach の競合処理**: `O_CREAT|O_EXCL` で作成を試み、勝者が展開・書き込み・
   ready マーク、敗者は既存を開いて ready を待つ。ready フラグ or ファイルロック。
5. **フォールバック**: shm 確保失敗（権限 / /dev/shm 不足 / 非対応）時はローカル heap
   alloc に必ずフォールバック。共有が壊れても従来動作を維持。

### 制約（standing rules）

- **unsafe**: shm/mmap FFI で必要。CLAUDE.md の許可カテゴリ（性能上必須、置換表と同様）。
  各 `unsafe` ブロックに「なぜ安全か / 守るべき不変条件」のコメント必須。
- **worktree**: `git worktree add` でブランチを切る（`git checkout` 不可）。
- **measurement-first**: 実装前後で concurrency 15 の NPS を計測（本記録の `tournament` /
  `analyze_selfplay` フロー）。メモリ常駐（RSS）も before/after 比較。−32% 崩落のうち
  どれだけ回復したかを定量化。
- **正当性検証**: 共有重みパスとローカル重みパスで**評価値ビット一致**を確認
  （`verify-nnue` スキル活用可）。探索結果が不変であること。
- `cargo fmt && cargo clippy --fix --allow-dirty --tests` 警告ゼロ、`cargo test`。
- **PR 前にローカル Codex レビュー（`codex:codex-rescue` agent）で Approve 必須**。
  `@codex` remote review は禁止。

### スコープ外

単一対局・スレッド並列（1 プロセス内は元々共有）には無関係。本機能は多プロセス運用
専用のスループット最適化。

## 実装結果 (2026-05-19)

実装ブリーフに沿って NNUE プロセス間パラメータ共有を実装した（ブランチ
`feat/nnue-shared-weights`）。設計は Codex レビューを 4 ラウンド（rev1→rev4）で
Approve、実装コードも Codex レビュー 2 ラウンドで Approve 済み。

### 実装概要

- `crates/rshogi-core/src/nnue/shared_weights.rs`（新規）: POSIX 共有メモリ機構。
  FNV-1a 128bit content hash で命名、`flock` 規律 + `ready` atomic で create-or-attach、
  attach 時に local 展開済み blob と memcmp して一致時のみ採用。失敗時は local heap に
  フォールバック。Linux 専用（他ターゲットは no-op）。
- `AlignedBox` を heap / 共有メモリ borrow の 2 backing 対応に拡張（`DerefMut` は
  共有 backing で panic、Drop で `munmap`）。
- 全 NNUE アーキ（LayerStacks / HalfKP / HalfKA / HalfKA_hm）の FeatureTransformer
  重みを共有対象に配線。1 重み blob = 1 shm セグメント。

### 検証結果

| 検証項目 | 結果 |
|---|---|
| 評価値ビット一致 | depth-14 探索が shm on/off で nodes/PV/bestmove 完全一致（score cp -106, nodes 114552 が完全一致）。round-trip 統合テストも pass |
| 共有発火 | LayerStacks / HalfKP モデルとも `shared (created)` → 別プロセスで `shared (attached)` を確認 |
| フォールバック | `RSHOGI_NNUE_SHARED_WEIGHTS=0` で `local (disabled by env)` |
| メモリ常駐 (PSS) | 8 プロセスの合計 PSS が **3780MB → 2276MB（−1504MB）**。FT 重み 215MB が 8 コピー → 1 物理コピー化（≒215MB×7 削減）。per-process 472MB → 284MB |
| NPS (並列 go ベンチ) | 同一局面・同一 movetime の A/B（32 コア機、15 並列 single-thread）で aggregate avg_nps **358K → 383–385K（+6.8〜7.4%、2 回計測）**。探索深度は同一 |

PSS 計測が機構の直接証明（重み常駐 N×→1×）。

### 自己対局 avg_nps 計測 (2026-05-20)

`tournament` で shm-on vs shm-off（同一バイナリ、`RSHOGI_NNUE_SHARED_WEIGHTS` のみ差）の
head-to-head 自己対局を並列度別に実施（各 20 局、byoyomi 1000 / hash 256 / threads 1、
model allfp16-r3-400、production + `layerstack-only,nnue-progress-diff` ビルド）。
`analyze_selfplay` の avg_nps:

| concurrency | プロセス数 | shm-off avg_nps | shm-on avg_nps | Δ |
|---|---|---|---|---|
| 5 | 10 | 631,364 | 637,608 | +0.99% |
| 12 | 24 | 572,635 | 586,078 | +2.35% |
| 16 | 32（=コア数）| 486,164 | 509,642 | **+4.83%** |
| 20 | 40（コア超過）| 440,298 | 433,427 | −1.56% |

- c5→c16 で効果が**単調増加**（並列度が上がり帯域競合が増すほど共有の利得が拡大）。
  c16（プロセス数＝コア数、CPU 余裕あり・メモリ帯域律速の領域）で **+4.83%**。
- c20 は 40 プロセス／32 コアで CPU オーバーサブスクライブ。律速が CPU スケジューリングへ
  移り共有メモリの利得が出にくく、20 局の計測ノイズ（avg_nps の SE ≈ ±1% 規模）に
  埋もれて −1.56%。**機能が遅くするわけではない**（評価値ビット一致で探索は同一、
  共有領域の読み出しは heap と同速）。
- 本 A/B は同一マシン上で shm-off 側の private コピー（c20 で 20×225MB）が L3／帯域を
  汚染するため、利得を**過小評価する保守的計測**。全プロセスが共有する実運用では
  並列 go ベンチの +6.8〜7.4% に近い値が期待値。

総括: 重み重複コピーに起因する帯域競合ぶんを回復。メモリ帯域律速の領域（〜c16）で
+1〜+4.8%、CPU 律速へ移る超過領域では中立。PSS の N×→1× が直接効果。
（本記録冒頭の −32% 崩落のうち重み重複ぶんを回復。TT 等の他要因の競合は残る。）

### 運用メモ

- 健全セグメントは unlink しない（content-hash 命名で run をまたいで warm 再利用）。
  distinct な eval 内容ごとに最大数百 MB が `/dev/shm` に残留する。手動 cleanup は
  `rm /dev/shm/rshogi-nnue-*`。
- キルスイッチ: `RSHOGI_NNUE_SHARED_WEIGHTS=0`（`off` / `false` も可）で無効化。
