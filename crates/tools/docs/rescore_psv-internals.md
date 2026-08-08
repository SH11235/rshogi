# rescore_psv internals — 実装詳細

[rescore_psv](rescore_psv.md) の内部実装メモ。ツールを使うだけなら読む必要はない。
挙動の保証（bit 一致・メモリ特性）の根拠や、下流ツール・保守で必要になる仕様を
まとめる。

## ONNX モードの供給パイプライン

ONNX 直推論モード（`--dlshogi-onnx-model` / `--onnx-model`）では、処理を
reader（PSV 読み込み + デコード、直列）→ producer（rayon 並列特徴量構築）→
GPU worker × `--onnx-sessions`（推論 + score 変換等の後処理）→ writer（バッチ順への
再整列 + ファイル書き出し）の各スレッドに分け、全段をオーバーラップして実行する。
GPU が次バッチの CPU 前処理を待ってアイドルする区間を潰し、GPU を連続的に飽和させる。
バッファは固定枚数の slot プールで再利用するため、ピークメモリは入力件数に依存しない。

`--onnx-sessions` ≥ 2 では ORT セッション（= CUDA ストリーム）を複数持ち、バッチを
round-robin で振り分けて in-flight を多重化する。単一セッションでは H2D →
compute → D2H が GPU 上で完全直列になる（`run_binding` が同期のため）のに対し、
複数セッションでは別バッチの転送と compute が重なり、転送分の GPU アイドルを回収できる。
VRAM（エンジン/コンテキスト）はセッション数に応じて増える。GPU が電力上限に達している
環境では 3 以上に増やしても伸びない（実測では 2 が最適）。

### 決定性の根拠

バッチ構成は reader の直列段階で確定し、どのセッションで推論しても同一エンジン
なら結果は同一、書き出しは writer がバッチ通番で再整列するため、出力はセッション数・
スレッド数に依存せず逐次実装と bit 一致する。cold cache 初回実行では 2 本目以降の
セッションが 1 本目の初回バッチ完了（= エンジン確定・キャッシュ書き込み）を待って
から開始するため、全セッションが同一エンジンを使う（実測: cold 初回でもエンジン
ビルドは 1 回のみ）。

TensorRT の cold cache **初回実行そのもの**は、実行途中のエンジン/プロファイル確定
（端数バッチ等の新 shape）により、キャッシュ済みエンジンでの再実行と最下位ビットが
一致しないことがある。これはセッション数に依らない TensorRT の挙動であり、厳密な
bit 再現の基準にはキャッシュ済み（warm）エンジンでの実行を使うこと。

### CUDA pinned メモリ

入力特徴と出力（推論結果）の host バッファは、GPU 推論時に確保できれば CUDA pinned
(page-locked) メモリを使う（`IoBinding` の入出力先を `AllocationDevice::CUDA_PINNED`
に設定）。pageable だと H2D（`cudaMemcpyAsync`）や `run_binding` 内の D2H が
pageable→pinned ステージング（CPU 介在）を伴い実質同期化するが、pinned 化でこれが
消えて真の async 転送になる。確保できない環境（CPU 推論 / 確保失敗）では pageable に
自動フォールバックする。pinned 化はメモリ場所のみの差で、出力は bit 一致する。
pinned 化により転送は推論と overlap され支配項ではなくなるため、入力 FP16 化による
転送量半減の効果も小さい。

### TensorRT FP32 を使わない理由

TensorRT FP32 は計測の結果 CUDA EP より遅い（カーネル最適化の効果よりセッション
初期化コストが大きい）。FP32 で推論する場合は `--onnx-tensorrt` を指定せず CUDA EP
を使う。FP16（TensorRT）は FP32 比 約 2.5〜2.8 倍高速だが、評価値に平均 12cp 程度の
差が出る（FP16 の方が系統的にやや高く出る傾向）。

## NNUE / 探索 / 外部エンジンモードのチャンクストリーミング

100 万件単位で「読み込み → 並列処理 → 入力順書き出し」を繰り返す。

- `--search-depth`: 各ワーカースレッドが専用の `Search` インスタンスを持ち、
  チャンク内の連続スライスを逐次処理する。`Search::go` は置換表をクリアせず世代
  更新のみ（履歴も持ち越し）のため、`Search` はチャンクをまたいで永続させる。
  局面割り当てが（チャンク長, スレッド数）だけで決まるため、同一設定での再実行は
  bit 一致する。
- `--engine`: チャンクを生存エンジンプロセスへ連続分割。エンジン死亡時は未評価
  レコードを同一チャンク内で再割り当てして評価し切ってから入力順に書き出す
  （出力順は維持される）。死亡エンジンは `quit`（wait で reap）してプールから除去
  し、以降のファイルに割り当てない。全滅時はエラー終了。
- `Ctrl-C` 中断時は最初の未評価レコードで書き出しを打ち切り、部分出力を入力の
  連続 prefix に保つ。

## 完了マーカー（`<rescore_output>.done`）の仕様

ONNX モードで各入力ファイルの処理完了時に `key=value` 形式の sidecar テキストを
atomic rename + `sync_all()` で書く。プロセス kill / panic に耐えるが、電源断・
カーネルパニックは非目標。

再実行時の判定:

- `.done` marker の fingerprint が現在の CLI と完全一致 + 出力サイズが記録と一致 → skip
- fingerprint 不一致 → rescore / expand / replacement 全出力を truncate して再生成
- `--out-scores` で `.done` なし → `.in-progress` fingerprint 一致時だけ
  レコード数ベース resume。不一致・欠落・破損時は sidecar を truncate
- full PSV で marker なし + expand / replacement / `--qsearch-leaf-label` いずれも無効 →
  従来のレコード数ベース resume にフォールバック
- full PSV で marker なし + 上記いずれか有効 → 全出力を truncate して最初から処理

fingerprint に含まれる項目:

- モデルパス（canonicalize 済み）、モデルサイズ、モデル mtime（ns）
- 入力パス、入力サイズ、入力 mtime（ns）
- `process_count`（`--limit` 適用後）
- `--out-scores`、`--skip-in-check`、`--score-clip`、`--onnx-eval-scale`
  （`f32::to_bits()` の hex）、`--onnx-tensorrt`
- AobaZero モデル時のみ `--onnx-draw-ply`
- `--qsearch-leaf-label`、および有効時のみ `--max-ply` と葉探索用 `--nnue` の
  パス・サイズ・mtime（葉 PV = 出力が NNUE に依存するため差し替えを検知する）
- expand 有効時: `--expand-threshold`（to_bits hex）、親/子王手 skip フラグ、
  `--expand-output-dir` の canonicalize 済みパス
- replacement 有効時: `--qsearch-leaf-replacement-output` の canonicalize 済みパス
  とその出力サイズ

後方互換: 旧 marker の `use_tensorrt` キー欠落は unknown として読み、現在値の
`true` / `false` のどちらとも fingerprint を一致させない。このためキー無し旧 marker は
更新後の初回実行で出力を truncate して一度だけ再生成され、再生成後の marker には同キーが入る。
`out_scores` キー欠落は従来どおり `false` 扱い（旧 sidecar 出力は存在しないため安全）。
旧 marker の `--qsearch-leaf-label` / `replacement` キー欠落も `false` 扱い。
`--qsearch-leaf-label=true` だが
葉探索 NNUE キーを持たない旧 marker は NNUE メタを `None` として読み、現設定と
fingerprint 不一致になって再生成される。

### パスに使える文字の制約

marker は `key=value\n` テキストのため、round-trip を保証できない文字を含むパス
（model / input / expand 出力）は起動時エラーで弾く: `=`（セパレータ衝突）、
`\n` / `\r`（レコードセパレータ衝突）、非 UTF-8 バイト列。

## パス安全チェック（ONNX モード）

起動時 / ファイルごとに以下を検証し、truncate によるデータ破壊を防ぐ:

- `--output-dir` と `--expand-output-dir` / `--qsearch-leaf-replacement-output` の
  同一ディレクトリ指定 → エラー
- 入力ファイル = 予定出力パス（未作成でも parent canonicalize で検出）→ エラー
- 既存出力が symlink → エラー（symlink 越しの truncate で入力を破壊しない）
- Unix のみ: 既存出力が入力と同じ inode（hardlink）→ エラー
- marker 不一致で旧 expand / replacement artifact を削除する前に、旧 artifact が
  現在の入力と同一実体でないことを検証 → 同一なら削除せずエラー（多段パイプライン対策）

## leaf-REPLACEMENT arm のレコード仕様

`--qsearch-leaf-label` + `--qsearch-leaf-replacement-output` の replacement 側出力
（`--apply-qsearch-leaf` → DL rescore の 2 工程と bit 一致）:

| フィールド | 値 |
|---|---|
| `sfen` | 葉局面の packed sfen（局面を葉に置換） |
| `score` | 葉の DL 評価（符号反転なし = 葉手番視点）。`--score-clip` 適用 |
| `move16` | 0 |
| `game_ply` | root の `game_ply` |
| `game_result` | 葉で手番反転時のみ `-game_result` |
| `padding` | 0 |

leaf-LABEL arm（`--output-dir` 側）は root sfen + root 手番視点へ符号反転した
score + root の `game_result`。両 arm は同一ループで 1:1 lockstep に書き出すため
レコード数が一致する（`--skip-in-check` は両 arm から同様に除外）。

## 進捗表示のフォーマット仕様

`stderr` の TTY 判定で表示を切り替える:

- TTY・複数ファイル: 全体集約バー + 処理中ファイルバーの 2 段
- TTY・単一ファイル: 1 段
- 非 TTY（`nohup` / CI）: ログ行を 15 秒 or 進捗 5% のどちらか早い方ごと + 終了時 1 行

```
[rescore] overall 38.4% 3.84M/10.0M shard 2/4 (shard_003 52.3%) 8.3k pos/s elapsed 00:01:23 remaining 12:40 ETA 06/22 14:43
[rescore] overall done 10.0M/10.0M (4 files) 8.3k pos/s took 00:20:05
```

TTY のバーは日本語表記、非 TTY のログ行は grep しやすい英語表記
（`elapsed` / `remaining` / `ETA`）。全体進捗の分母は起動時に
`metadata().len() / 40` の合算で算出（全件 load しない）。`--limit` は各ファイルに
適用され、`.done` skip / レジューム分も全体進捗に反映される。速度確定前
（ウォームアップ中）の ETA は `--/-- --:--` 表示。破損 / パース不能レコードや、
読み込めたレコード数が起動時の推定件数（`file_size / 40`）に満たないファイル
（実行中の切り詰め・空ファイル等）があると進捗が 100% に届かないことがあるが
表示上の挙動で、rescore 出力（有効レコードのみ）には影響しない。
