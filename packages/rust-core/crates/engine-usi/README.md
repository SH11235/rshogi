# engine-usi

USIプロトコルエンジンと、自己対局ハーネス（`engine_selfplay`）を含むクレートです。

## 自己対局ハーネスの使い方

バイナリ: `engine_selfplay`

主なオプション:
- `--games` 対局数（デフォルト1）
- `--max-moves` 最大手数（plies、デフォルト512）
- 時間設定: `--btime --wtime --binc --winc --byoyomi`
- USIオプション: `--threads --hash-mb --network-delay --network-delay2 --minimum-thinking-time --slowmover --ponder`
- 入力局面: `--startpos-file <file>` または `--sfen <sfen>`（未指定なら平手）
- ログ: `--log-info` で info.jsonl を出力
- 出力先: `--out <path>`（未指定なら `runs/selfplay/<timestamp>-selfplay.jsonl`）
- エンジン指定: 共通バイナリは `--engine-path`、先手/後手で分ける場合は `--engine-path-black` / `--engine-path-white`（未指定時は実行ディレクトリ近辺や `engine-usi` を探索）

### よく使うコマンド例

- 1秒秒読みで数をこなす（infoログなし、デフォルト出力先）  
  `cargo run -p engine-usi --bin engine_selfplay -- --games 10 --max-moves 300 --byoyomi 1000`

- 5秒秒読み + network-delay2=1120、infoログ付きで指定パスに出力  
  `cargo run -p engine-usi --bin engine_selfplay -- --games 2 --max-moves 300 --byoyomi 5000 --network-delay2 1120 --log-info --out runs/selfplay/byoyomi5s.jsonl`

- 特定SFENの再現（startposファイルを用意して1局だけ）  
  `cargo run -p engine-usi --bin engine_selfplay -- --games 1 --max-moves 300 --byoyomi 5000 --startpos-file sfen.txt --log-info`

- 先手・後手で別エンジンを指定して対局（片方を旧バージョンにして差分確認などに）
`cargo run -p engine-usi --bin engine_selfplay --release -- --games 10 --max-moves 200 --byoyomi 1000 --threads 1 --hash-mb 256 --engine-path-black target_before/release/engine-usi --engine-path-white target/release/engine-usi`
`cargo run -p engine-usi --bin engine_selfplay --release -- --games 10 --max-moves 200 --byoyomi 1000 --threads 1 --hash-mb 256 --engine-path-black target/release/engine-usi --engine-path-white target_before/release/engine-usi`

### 出力ファイル

- 対局ログ: JSONL（デフォルト `runs/selfplay/<timestamp>-selfplay.jsonl`）
  - 1行目: `meta`
    - `engine_cmd` に per-side のパス/ソース/引数（`path_black`/`path_white` など）が記録されます
  - 各手: `move`（sfen_before, move_usi, elapsed_ms, think_limit_ms, timed_out など）
  - 終局: `result`（outcome, reason, plies）
- infoログ（`--log-info`有効時）: 同名 `.info.jsonl`
- KIF: 同名 `.kif`（複数局なら `_gXX.kif` 分割）
