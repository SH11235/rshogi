---
name: selfplay
description: NNUE モデルの棋力評価と SPRT 検定。tournament ツールでエンジン間の総当たり対局、base-vs-N 対局、あるいは SPRT (逐次確率比検定) による有意差の早期判定を実行し、analyze_selfplay で集計・post-hoc 解析する。「評価して」「対局させて」「SPRT で検定して」「有意差を見て」「Elo 差を測って」等の棋力比較リクエストに使用する。
user-invocable: true
---

# 自己対局評価スキル

以下の指示に従い、指定されたエンジン間の対局を実行し結果を集計する。
統計規範 (SPRT bounds・試行規律・採否判定) は本 SKILL の各節に記す。
規範から逸脱する場合は実験 doc に理由を事前登録する。
モードは主に 3 つ:

1. **総当たり (round-robin)** — 固定局数で全ペアを対局させて Elo 差・勝率を測る。デフォルト。
2. **base-vs-N** — 基準エンジン 1 体と challenger N 体だけを対局させる (`--base-label`)。総当たりの無駄を省く。
3. **SPRT (逐次確率比検定)** — A/B 2 エンジンで有意差を逐次判定し、境界到達で自動早期停止する (`--sprt`)。ユーザーが「SPRT」「逐次検定」「有意差」「早期停止」等を要求した場合はこちらを使う。

## 入力パラメータ

ユーザーから以下の情報を `$ARGUMENTS` として受け取る。
情報が不足している場合は質問して補完すること。

### 必須情報
- **対象エンジン一覧**: 各エンジンの commit ハッシュ（短縮可）、バイナリパス、説明
- **確認ポイント**: 特に注目する比較（例: "E vs D: TT 16bit の棋力効果"）

### デフォルト値（指定がなければ以下を使用）
- **開始局面**: `--startpos-file data/startpos/start_sfens_ply32.txt`（**必須**。平手からの対局は序盤の偏りで正確な棋力を測れないため、必ず開始局面集を使用すること。他の候補: `data/startpos/start_sfens_ply24.txt`, `data/startpos/taya36.sfen`。これらは local 専用 (gitignored `data/` 配下) で、他の contributor の環境には存在しないので skill 側で必要なら個別に配置する）
- 秒読み: 1000ms
- スレッド: 1
- ハッシュ: 256MB
- 各方向の対局数: 100（双方向で200局/カード）
- NNUE: エンジンごとに `--engine-usi-option` で個別指定

### 対局条件は必ずユーザーに相談して決める (デフォルト値を勝手に採用しない)

以下の条件は **起動前に必ずユーザー確認**。推奨値とトレードオフを示した上で尋ね、
ユーザーが明示した場合のみその値を使う。前回の SPRT で使った値を**暗黙に踏襲しない**。

#### (a) 並列数 (`--concurrency`)

マシンの空き CPU は時々で異なり (学習・他ベンチが同時稼働していることが多い)、過剰
並列は時間制対局の実効 NPS/depth を下げて評価品質を落とす一方、過少だと無駄に時間が
かかる。確認時は「現在の CPU 負荷状況 (同時稼働中の学習/ベンチの有無とスレッド数)」を
添えて尋ねる。

Linux / WSL2 で複数プロセスを並走させる場合は、開始前に `df -h /dev/shm` で空き容量も
確認する。NNUE 共有重みの確保失敗は local heap へ無言でフォールバックし、片側だけの
fallback が A/B の偽差を生み得るため、満杯・僅少なら解消するまで起動しない。正常時の
engine stderr は `tournament` が内部 buffer に取り込むだけで永続化しないため、下記の
side 別 wrapper で rshogi の stderr を保存する。終了後の marker 確認は
「完了待ち・結果集計」の必須チェックに含める。

#### (b) time control: `--byoyomi` vs `--nodes`

**比較するエンジン構成によって選択則が異なる**:

| 比較軸 | 推奨 time control | 理由 |
|---|---|---|
| **同 FS 同 arch、重み差のみ (recipe / 量子化 / SPSA 差等)** | **`--nodes <N>`** (固定ノード) | NPS 差なし、CPU 競合の影響も排除して clean に重み差を抽出 |
| **search / 異 FS / 異 arch / 速度が変わる変更** (探索変更、feature-set 差、dim 差、PSQT 有無差等) | **`--byoyomi <ms>`** (固定時間) | 実戦強度 = eval 品質 × NPS。NPS 差を含めた total strength を測る |
| (補助) 異 FS で eval 品質を切り分けて測りたい | 両方 (固定時間 + 固定ノード) | featureset-sweep 実験ログ (rshogi-nnue docs/experiments) §10-F / §10-E pattern |

search / FS / arch / 速度が変わる対局を固定ノードでやると、本来実戦強度に効く NPS 差 (例: HalfKP の avg_nodes
は HalfKA_HM_merged より +6-14% 多い) を切り捨ててしまい、デプロイ実態と乖離する。
**「前回固定ノードだったから今回も」と暗黙踏襲は禁止**、比較軸ごとに毎回相談する。

#### (c) SPRT 仮説と上限局数

- 標準 bounds は用途別 2 種 (α=β=0.05):
  - **gainer (強くする変更)**: `--sprt-nelo0 0 --sprt-nelo1 10`
  - **simplification / non-regression**: `--sprt-nelo0 -10 --sprt-nelo1 0`
- simplification の `<-10, 0>` は -5 nElo 級の退行を検出せず通し得る。簡略化 SPRT を
  乱発せず、保守性が必要なら期待局数を見積もり、より狭い bounds を事前登録する。
- **4 パラメータ (`--sprt-nelo0 / --sprt-nelo1 / --sprt-alpha / --sprt-beta`) は常に明示する**。
  CLI 既定値 (H0=0 / H1=+5) は規約標準と異なるため、省略すると別の検定が走る。
  標準以外の bounds を使う場合は実験 doc に理由を事前登録した上で。
- `--games`: 上限局数。**各方向の局数**であり、標準上限は `--games 5000` = 総 10,000 局。
  上限到達で境界未達なら inconclusive = **その patch は採用しない** (ただし accept_h0 と
  異なりアイデア族の終了ではない。再挑戦はコード・パラメータ・仮説・条件のどれをなぜ
  変えたかの事前記録が条件で、同一条件の引き直しや別 seed でのやり直しは不可)。上限延長は事前宣言
  した場合のみ、**到達前に** `control.json` の `target_games` で行う (到達後は延長不能)。
- **走行中の SPRT を途中停止しない** (LLR の途中値は停止理由にならない。例外は
  致命的実装不具合・設定誤り・game error の許容超過の 3 種のみ)。
- **試行規律のチェック**: 正式 SPRT は同一アイデア族で
  最大 3 回 / 同一 patch・同一条件の引き直し禁止 / `accept_h0` でそのアイデア族は終了 /
  全試行を実験 doc に記録 (採用 run だけ残さない)。
- **union patch (落ちたが正寄りだった変更の抱き合わせ再試験) の規律**: 最大 2 要素、
  数値調整級・密接な相互作用が説明できるものだけ (独立した概念の追加/削除は単独で通す) /
  `accept_h0` になった 2 案の結合救済は禁止 / union test も各要素のアイデア族の試行回数に
  算入する / 通っても寄与の切り分けが不明である旨を記録する。

#### (d) 開始局面選択の seed (`--seed`)

棋力評価では `--seed` の明示指定を必須とし、値を実験 doc の事前登録節に記録する
(再現性要件)。省略時に entropy 生成値が起動ログと `meta.json` に
出るのは、デバッグ等の非採用 run 向けの挙動であり、採否判定に使う run では
省略しない。独立 seed での再確認 (SPSA endpoint 等) では過去 run と異なる seed を
明示的に選ぶ。

#### (e) 実験 doc の事前登録

正式な棋力評価は、起動前に以下を実験 doc へ記録する: engine SHA、NNUE (ファイルと
FV_SCALE)、USI options、startpos ファイルと hash、seed、TC (nodes / byoyomi)、threads、
hash、SPRT bounds と上限局数。加えて実行機・backend・raw log の所在も記録する。

#### (f) startpos / engine USI option

これらはモデル / 評価目的に依存。startpos は `data/startpos/start_sfens_ply32.txt`
が default だが他にもある。USI option (FV_SCALE / LS_BUCKET_MODE / LS_PROGRESS_COEFF 等)
はモデルごとに必要なものが異なる。デフォルト 1 つに固執せず、対象モデルから判断 +
不明なら確認。

## 永続出力先 gate（起動前に必ず実行）

対局 JSONL は後で教師データに使う可能性があるため、出力先はユーザー相談で変える項目ではない。
**本体 checkout の絶対パス `rshogi/runs/selfplay/` に固定**し、linked worktree には出さない。

- 「本体 checkout」は branch 名では決めない。`main` branch 用でも `rshogi-main` 等は
  disposable worktree であり、保存先として禁止。
- `git worktree list --porcelain` の先頭 record（primary worktree）を canonical root とし、
  directory 名が厳密に `rshogi` かつ実在する通常 directory であることを確認する。
  条件を満たさなければ起動せず user に確認する。
- run 名は実在日時の `YYYYMMDD-HHMMSS-PURPOSE`。`YYYYMMDD_HHMMSS` や不正日時は禁止。
- `--out-dir` は必ず canonical root の直下
  `<canonical-rshogi>/runs/selfplay/YYYYMMDD-HHMMSS-PURPOSE` を絶対パスで渡し、起動前には
  run directory が存在しないことを確認する。
- Windows の junction / symlink、Linux の symlink を永続 root に使わない。
- engine の build は worktree でよいが、`xtask` が作る標準
  `engines/rshogi-usi-<edition>` を使う。`sprt-runtime-*` 等の独自 runtime directory を作らない。
- 起動 script では build repo と log root を同じ変数に畳み込まない。log root は上記 canonical
  root からのみ組み立てる。

出力 directory を作る前に OS に合う bundled checker を通す:

```powershell
# Windows native
powershell -NoProfile -ExecutionPolicy Bypass `
  -File .claude/skills/selfplay/scripts/assert_output_root.ps1 `
  -Repo $PWD -OutDir $OUT
```

```bash
# Linux / WSL2
bash .claude/skills/selfplay/scripts/assert_output_root.sh "$PWD" "$OUT"
```

checker が失敗したら **起動禁止**。起動後に違反へ気付いても user 承認なしに job を kill しない。
状況を報告して指示を待つ。局数・並列数だけの変更は kill/restart せず `control.json` を使う。

## ビルドの注意 (pre-flight 必須 3 点)

engine build / feature 構成 / format 互換性の詳細は
[`edition-build` SKILL](../edition-build/SKILL.md) を参照。selfplay 起動前に
必ず以下を確認:

1. **同一 build profile** で全 engine を揃える (release / production 混在は NPS 比較に
   バイアス、`<binary>.meta.toml` で確認)
2. **NNUE format 互換性**: 使う ckpt の `nnue_ver` (例: `0x7af32f21` = 最新) を engine が
   読めること。`usi` → `isready` で `readyok` 確認、`Unknown NNUE version: 0x...` が
   出たら engine 再 build (`cargo run --release -p xtask -- build --edition <name>
   --profile production`)
3. **preset edition** 経由 (`xtask build`) が最も安全。手動 `cargo build` は feature
   漏れ / 過剰でバイアス源になる (過剰 feature は `readyok` 通過しても余分な accumulator
   field 残留で NPS に出る)

build trace は `engines/<binary>.meta.toml` に edition / profile / commit / built_at が
記録されるので、流用前に必ず確認。

## 実行手順

### 1. engine binary 準備

build / preset edition / feature 構成の詳細は
[`edition-build` SKILL](../edition-build/SKILL.md) を参照。selfplay 観点での最低要件:

- **モデル比較なら preset 経由で build**: `cargo run --release -p xtask -- build
  --edition <name> --profile production`。preset 一覧は `xtask list-editions`
- **既存 binary を流用するなら meta.toml で edition / profile / commit 確認**
  (features は記録されないので edition 名から逆引き)、ckpt format との整合が
  取れない (`Unknown NNUE version: 0x...`) なら rebuild
- **`usi` → `isready` で全 binary x 全 ckpt の load test を pre-flight**:
  ```bash
  echo -e "usi\nsetoption name EvalFile value {MODEL_PATH}\n{OTHER_OPTIONS}\nisready\nquit" \
    | timeout 30 {BINARY_PATH} 2>&1 | grep -E 'readyok|Error|Unknown NNUE version|panic'
  ```
  `readyok` 確認できれば selfplay 起動 OK

### 2. 出力ディレクトリの作成

上記 gate で検証した絶対パスだけを作成し、実験ごとのログ混入を防ぐ。

```bash
CANONICAL_RSHOGI=/absolute/path/to/rshogi
OUT="$CANONICAL_RSHOGI/runs/selfplay/$(date +%Y%m%d-%H%M%S)-{PURPOSE}"
mkdir -p "$CANONICAL_RSHOGI/runs/selfplay"
bash .claude/skills/selfplay/scripts/assert_output_root.sh "$PWD" "$OUT"
mkdir -p "$OUT"
```

- `{PURPOSE}` はユーザーの実験目的を短く要約したもの（例: `tt-16bit`, `lmr-tuning`）
- 検証済みの絶対パス `$OUT` を以降の `--out-dir` オプションで使用する
- 起動前の wrapper 準備は許容するが、既存の対局 JSONL / meta.json / control.json がある OUT は tournament が拒否する。再開・上書きは未対応。

#### Linux / WSL2: rshogi engine stderr の永続化

複数の rshogi process を起動する run では、各 side の実 binary を `exec` する wrapper を
起動前に生成し、`tournament --engine` には実 binary でなく wrapper を渡す。wrapper は
process ごとに `$OUT/engine-stderr/<side>-<unique>.log` を作るので、正常終了した process の
共有重み marker も失われない。既存 process log が 1 件でもある `$OUT` では生成を拒否する。
その場合は旧 run を再利用せず、新しい run directory を作る。

```bash
make_rshogi_wrapper() {
  side=$1
  case "$side" in
    '' | . | .. | *[!A-Za-z0-9._-]*)
      printf 'invalid engine side: %s\n' "$side" >&2
      return 1
      ;;
  esac
  for old_log in "$OUT"/engine-stderr/*.log; do
    if test -e "$old_log"; then
      printf 'existing engine stderr log found; use a fresh OUT: %s\n' "$OUT" >&2
      return 1
    fi
  done
  if ! real_bin=$(readlink -f -- "$2") || ! test -x "$real_bin"; then
    printf 'rshogi binary is not executable: %s\n' "$2" >&2
    return 1
  fi
  side_dir="$OUT/engine-stderr/$side"
  mkdir -p "$OUT/engine-stderr" || return 1
  if ! mkdir "$side_dir"; then
    printf 'engine side already exists or cannot be created (%s); use a fresh OUT: %s\n' \
      "$side" "$OUT" >&2
    return 1
  fi
  printf '%s\n' "$real_bin" > "$side_dir/real-bin" || return 1
  cat > "$side_dir/run" <<'SH' || return 1
#!/bin/sh
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
log_dir=$(dirname -- "$script_dir")
side=$(basename -- "$script_dir")
IFS= read -r real_bin < "$script_dir/real-bin"
log_file=$(mktemp "$log_dir/$side-XXXXXX.log")
exec "$real_bin" "$@" 2>> "$log_file"
SH
  chmod 700 "$side_dir/run" || return 1
  printf '%s\n' "$side_dir/run"
}

BASE_REAL="$PWD/engines/rshogi-usi-{BASE_EDITION}"
TEST_REAL="$PWD/engines/rshogi-usi-{TEST_EDITION}"
BASE_ENGINE=$(make_rshogi_wrapper base "$BASE_REAL") || exit 1
TEST_ENGINE=$(make_rshogi_wrapper test "$TEST_REAL") || exit 1
```

side 名は `base` / `test` に限らず engine label と対応する一意な名前にする。YaneuraOu など
`nnue shared weights` marker を実装しない外部 engine は wrapper / marker 検査の対象外で、
そのまま実 binary を `--engine` に渡す。Linux / WSL2 以外で wrapper が不要な場合は、
`BASE_ENGINE=engines/rshogi-usi-{BASE_EDITION}` のように xtask 成果物を直接指定する。

### 3. tournament バイナリで総当たり自己対局を実行

全エンジンに `--engine-label` を明示し、同一 run 内で重複しない短いラベルを付ける。
比較する差分に応じて選ぶ:

- NNUE 比較: モデルの basename（拡張子なし）を基本に、`step100000` / `step200000` など。
- 探索変更: `baseline` / `pass-root-bonus` など変更内容を表す名前。
- 設定比較: `threads1` / `threads8` など設定差を表す名前。

同名モデル・同名バイナリや wrapper を使う場合も、実験上の役割・変更名で区別する。
ファイル名でも読みやすい ASCII 英数字・`_`・`-` の48文字以内を推奨する。
`--base-label` / `--sprt-base-label` / `--sprt-test-label` は選んだ元ラベルに揃える。
実 binary のパス・SHA・NNUE・USI 設定は上記の実験 doc に記録し、ファイル名に詰め込まない。
tournament の meta には指定したコマンドパス・ラベル・USI 設定が保存されるが、wrapper 内の
実 binary や SHA は推定されない。出力名の整形規則は `crates/tools/docs/tournament.md` を参照する。

`tournament` バイナリ1コマンドで、全ペアの総当たり対局を並列実行する。
`--engine` を複数指定すると自動で C(N,2) ペアの対局を生成する。
Linux / WSL2 で複数の rshogi process を使う場合、`{ENGINE_A}` 等には上で生成した
side 別 wrapper のパスを指定する。

```
cargo run -p tools --release --bin tournament -- \
  --engine {ENGINE_A} --engine-label {SIDE_A} \
  --engine {ENGINE_B} --engine-label {SIDE_B} \
  [--engine {ENGINE_C} --engine-label {SIDE_C} ...] \
  --games {GAMES} --byoyomi {BYOYOMI} --hash-mb {HASH} --threads {THREADS} \
  --concurrency {CONCURRENCY} \
  --seed {SEED} \
  --startpos-file data/startpos/start_sfens_ply32.txt \
  --usi-option {NNUE} \
  --out-dir "$OUT"
```

- `--concurrency`: 並列対局数（デフォルト1）。CPUコア数に応じて調整。
- `--report-interval`: N局ごとに進捗を表示（デフォルト10）。
- `--engine-usi-option "INDEX:Name=Value"`: エンジン個別の USI オプション（0始まりインデックス）。
  デフォルトでは共通 `--usi-option` にマージされ、同じキーは engine 個別指定が上書きする。
  旧挙動の完全置換が必要な場合は `--strict-engine-usi-option` を併用する。
- **基準エンジンとの比較（1v1 含む）では `--base-label` を必ず付ける**。base-vs-N モードに
  なるだけでなく、meta 行に `base_label` が記録され、後から `analyze_selfplay --sprt` を
  ラベル指定なしで実行しても base/test の役割が自動推定される。`--engine` の指定順に
  役割の意味は無い（ファイル名・meta の label_black/white は指定順のまま）。
- 出力は以下の2種類が `{out-dir}` に自動生成される:
  - `pair-{i}-{j}__{label_i}-vs-{label_j}.jsonl`（--engine 指定順の 0 始まり index + 整形した表示ラベル）: ペア別の棋譜ログ（各対局の指し手・評価値・結果）。例: `pair-0-1__baseline-vs-pass-root-bonus.jsonl`
  - `meta.json`: 対局設定・エンジン情報をまとめたファイル。対局条件の確認・再現に利用可能。

**注意:** `run_in_background: true` で起動し、`TaskOutput` で完了を監視すること。

#### 実行中の動的制御（再起動不要）

長時間 run（SPRT 数千局・base-vs-N 等）の最中に、**再起動せず**対局境界で並列度や対局数を
変えたいときは `<out-dir>/control.json` を書き換える。producer ループが 500ms 間隔で
ポーリングし、次の対局境界で反映する。background task で回している最中でも、別途
ファイルを書くだけで介入できる（Ctrl-C → パラメータ変更 → 再起動で進行中の対局結果を
無駄にする / out-dir が分割されて集計が面倒になる、を回避）。

```bash
# <out-dir>/control.json （存在するフィールドだけ反映。両方任意）
echo '{"target_games": 300, "concurrency": 8}' > "$OUT/control.json"
```

- **`target_games`**: 各方向・各ペアあたりの目標対局数（CLI `--games` と同じ単位）。
  - 増やす → 既存ペアに追加チケットを供給して続行
  - 減らす → 進行中ペアは 2 局目まで完結させた上で新規供給を停止し drain（pentanomial の
    片側ペア残りを防ぐ）
- **`concurrency`**: ワーカー数。増やすと即座に追加 spawn、減らすと対象ワーカーが現局面
  完了後に退役する。`0` 以下は無視。
- 変更は `<out-dir>/control_history.jsonl` に追記され、`pair_index` 整合は維持されるので
  `analyze_selfplay` の集計と矛盾しない。

留意点:
- 変更は**対局境界**でのみ反映。進行中の 1 局は途中で再ターゲットされない。
- `concurrency` 増加は worker 追加ごとに engine spawn（NNUE ロード相当）のコストがかかる。
- SPRT の**途中トグル**（実行中に SPRT を有効化）は未対応。SPRT は起動時 `--sprt` で指定する。
- 不正な JSON / 読み込み失敗 / 履歴追記失敗は警告のみで実行継続（対局は落とさない）。

#### 外部エンジンとの対局例

rshogi と YaneuraOu のように異なるエンジンを対局させる場合、
エンジンごとに必要な USI オプションが異なるため `--engine-usi-option` を使う:

```
cargo run -p tools --release --bin tournament -- \
  --engine "$BASE_ENGINE" --engine-label base \
  --engine /path/to/YaneuraOu-binary --engine-label test \
  --engine-usi-option "0:EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin" \
  --engine-usi-option "1:EvalDir=/path/to/eval" \
  --engine-usi-option "1:BookFile=no_book" \
  --games 100 --byoyomi 3000 --hash-mb 256 --threads 1 \
  --concurrency 5 --seed {SEED} \
  --startpos-file data/startpos/start_sfens_ply32.txt \
  --base-label base \
  --out-dir "$OUT"
```

### 4. 完了待ち・結果集計

Background task の完了を `TaskOutput` で検知する。
完了後、`analyze_selfplay` ツールで対局ログを集計しサマリを生成する:

```
cargo run -p tools --release --bin analyze_selfplay -- "$OUT"/*.jsonl
```

`analyze_selfplay` は JSONL ファイルを読み込み、勝率・Elo差・手数分布などを集計して標準出力に表示する。
直接対決セクションでは trinomial Elo 差に加え、**pentanomial nElo（正規化 Elo）** を併記する。
nElo はペア単位 (同一開始局面・先後入替) で集計し、開始局面・先後の交絡を除去した
より正確な棋力差推定を提供する（SPRT の nelo と同じ指標）。

```
  A(engine-base) vs B(engine-test) | 2045局 | ... | Elo差:-36 ±15 | nElo:-37 ±15
```

この出力を元に、以下の内容をマークダウンファイル（`docs/performance/` 配下）に出力する:

1. **対局条件**: 秒読み・スレッド・ハッシュ・対局数・NNUE
2. **総合結果表**: 各カードの勝敗・勝率・Elo差
3. **確認ポイントの評価**: ユーザーが指定した比較ポイントについての分析
4. **総括**: 全体的な傾向と推奨事項

採否判定の前に以下も確認する:

- **`max_moves` 到達率**: `analyze_selfplay` の専用計数が未実装の間は、JSONL の
  `reason="max_moves"` を直接集計する。到達率 0.1% 未満は現状維持、0.1% 以上は長手数局を
  調査し、1% 以上は bounds の解釈より先に上限または千日手等のルール処理を見直す。
- **NNUE 共有重み** (Linux / WSL2 の複数 rshogi process run): wrapper を使った各 rshogi
  side について、全 process log に `nnue shared weights [...]` の `shared` marker があり、
  `local` marker が無いことを確認する。missing / `local` が 1 件でもあれば結果を suspect
  扱いにし、原因を解消して再走する。marker 非対応の外部 engine は対象に含めない。

  ```bash
  check_rshogi_shared_weights() {
    side=$1
    found=0
    for log in "$OUT"/engine-stderr/"$side"-*.log; do
      test -e "$log" || continue
      found=1
      if ! grep -Eq 'nnue shared weights( \[[^]]+\])?: shared' "$log" ||
        grep -Eq 'nnue shared weights( \[[^]]+\])?: local' "$log"; then
        printf 'suspect shared weights log: %s\n' "$log" >&2
        return 1
      fi
    done
    if test "$found" -ne 1; then
      printf 'missing shared weights log for side: %s\n' "$side" >&2
      return 1
    fi
  }

  check_rshogi_shared_weights base || exit 1
  check_rshogi_shared_weights test || exit 1
  ```

  外部 engine との例では rshogi 側だけ（例: `base`）を検査する。

### 5. SPRT モード（逐次確率比検定）

2 エンジン間で「有意差があるか」を早期判定したい場合は、`tournament --sprt`
を使う。境界に達した時点で新規チケット供給を停止し、進行中ゲームは完了待ちで
drain する。ライブで判定が走るので、従来の固定 `--games` より**大幅に時間を節約できる**
ケースが多い（一方で、差が微妙な場合は `--games` 上限まで走る）。

典型例 (gainer 標準): `base` エンジンと `test` エンジンで、H0=0 / H1=+10 nelo、
α=β=0.05、上限 `--games 5000` (総 10,000 局)。TC は比較軸選択則 (上記 (b)) に従い
固定ノードか固定時間を選ぶ。

```
cargo run -p tools --release --bin tournament -- \
  --engine "$BASE_ENGINE" --engine-label base \
  --engine "$TEST_ENGINE" --engine-label test \
  --games 5000 {--nodes N | --byoyomi MS} --concurrency 8 \
  --startpos-file data/startpos/start_sfens_ply32.txt \
  --seed {SEED} \
  --base-label base \
  --sprt --sprt-base-label base --sprt-test-label test \
  --sprt-nelo0 0 --sprt-nelo1 10 --sprt-alpha 0.05 --sprt-beta 0.05 \
  --out-dir "$OUT"
```

simplification / non-regression の検定は `--sprt-nelo0 -10 --sprt-nelo1 0` に差し替える。

ポイント:

- **`--base-label`**: base-vs-N モード（基準エンジン 1 個固定）。`--engine` を複数指定しても
  base と他のエンジンのペアだけが対局対象になる。3 エンジン以上で総当たり不要なとき便利。
- **`--sprt-test-label` は必須**。SPRT は challenger 視点で集計されるため、どちらが
  H1 側（検定したい側）かを明示する。
- **`--games` は上限の保険**。SPRT が境界に達したら自動で止まるので十分大きく取ってよい。
- **`--concurrency`**: 並列対局でも pentanomial ペアリングは `pair_index` で一意化される
  ため、完了順が前後しても正しく集計される。
- ログファイルには `pair_index` / `pair_slot` が自動で書き込まれるので、後から
  `analyze_selfplay --sprt` で再判定できる。
- 走らせている最中に並列度を変えたい / `--games` 上限を増減したいときは、再起動せず
  `<out-dir>/control.json` で調整できる（上記「実行中の動的制御」を参照）。SPRT の
  途中有効化のみ未対応。上限延長は事前宣言した場合のみ。
- **run 終了時に error ペアの状況を確認する**: 最終サマリと `meta.json` の
  `error_pairs` / `retried_pairs` / `exhausted_pairs` を見る。規約は「error を含む
  論理ペアは丸ごと破棄し、**同一 startpos・同一先後割当で最大 2 回再対局**」。
  `exhausted_pairs > 0`
  (meta の `invalid: true`) の run は**統計を採用しない** (統計的敗北にも変換しない)。
  invalid は「再試行 2 回でも error が残った」という機械判定であり原因分類ではないので、
  ログから error の帰属を調査して分類に応じて扱う (インフラ起因 → 再試行、枯渇なら
  invalid のまま統計的敗北に変換しない / 原因不明 → invalid 寄りに倒し、成功局だけを
  選んで集計し直さない)。**candidate (または base) 起因の crash は SPRT 以前の品質
  gate 失敗**なので、run 中に気付いたら枯渇を待たず中断して調査する。
- **二段検定は例外運用**: 標準は単段。release-critical な変更や SPSA endpoint 等の
  選抜結果のみ、**独立 seed** (過去 run と異なる `--seed`) で同 bounds を再確認する。

境界到達時の標準出力例:

```
[SPRT pair=2000 | test vs base] LLR=+2.979 (bounds -2.94..+2.94)  nelo=+14.01 ± 10.77  penta=[438, 20, 1000, 30, 512]  state=accept_h1
[SPRT] terminal decision reached; draining 6 in-flight game(s)...
...
=== SPRT Summary (test vs base) ===
bounds: LLR ∈ [-2.944, +2.944]  (alpha=0.05, beta=0.05)
nelo hypotheses: H0=+0.0  H1=+10.0
stopped_at:  pairs=2000, LLR=+2.979, decision=accept_h1
             nelo=+14.01 ± 10.77  penta=[438, 20, 1000, 30, 512]
final:       pairs=2006, LLR=+2.977, decision=accept_h1
             nelo=+13.98 ± 10.75  penta=[439, 20, 1004, 30, 513]
================================
```

### 6. SPRT post-hoc 判定（完了済みログに対して再計算）

既に `tournament` で回し終わったログから SPRT 判定を再現・再検討したいときは
`analyze_selfplay --sprt` を使う。`pair_index` が書かれているログには
無損失で復元できる。**SPRT モードで起動していない通常 run のログでも使える**。

```
cargo run -p tools --release --bin analyze_selfplay -- \
  "$OUT"/*.jsonl --sprt
```

base/test のラベルは以下の順で自動推定され、推定時は根拠が stderr に表示される:

1. CLI 明示（`--sprt-base-label` / `--sprt-test-label`。片方だけ指定すれば他方は補完）
2. meta の SPRT 情報（`tournament --sprt` で生成したログ）
3. meta の `base_label` 記録（`tournament --base-label` で生成したログ）
4. ラベル名に "base" を含む側を base と判断
5. 最後の手段として label_black を test とみなす（警告付き）

推定された役割が逆だったらラベルを明示して再実行する。レポートには
`{test} (test) vs {base} (base)` と nelo/elo の視点が明記されるので、
取り違えはレポート自体から判別できる。Wald パラメータは
CLI → meta → 既定値 (nelo0=0, nelo1=5, α=β=0.05) の順で解決される
(この既定値は規約標準の bounds と異なるので、meta に SPRT 情報が無いログの
再判定では 4 パラメータを明示する)。

通常の集計出力の末尾に SPRT レポートが追加される（`--json` 併用時は JSON 出力に
`sprt` フィールドが追加される）。

閾値を後から振り直して「どこで打ち切れたか」を検証するのにも使える (これは
診断・探索専用。採否判定は事前登録した bounds で行い、post-hoc の振り直しで
覆さない)。

## 入力例

```
/selfplay エンジン:
- A: 3526b075 engines/rshogi-usi-{A_EDITION} ベースライン
- B: 232d847d engines/rshogi-usi-{B_EDITION} move ordering完了
- D: 4778e1c6 engines/rshogi-usi-{D_EDITION} LMR修正（TT変更前）
- E: 5806777e engines/rshogi-usi-{E_EDITION} TT 16bit（最新）

確認ポイント:
1. E vs D: TT 16bit の棋力効果
2. E vs A: 全修正+TT の総合効果
3. B vs D: Step14+LMR が move ordering 完了時点より良いか悪いか
```
