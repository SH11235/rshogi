# 対局時計 (clock) の設定

`rshogi-csa-server-workers` が提供する対局時計の方式と、配信運用者が複数の
clock 設定を併用するための `CLOCK_PRESETS` 仕様をまとめたユーザ向けドキュメント。

## サポートする clock 方式

`CLOCK_KIND` (グローバル既定) または `CLOCK_PRESETS` (`game_name` 別 override) で
以下のいずれかを選択する。

| `kind` | 概要 | `Time_Unit` |
|---|---|---|
| `countdown` | CSA 2014 改訂互換、整数秒切り捨て | `1sec` (本家 Floodgate 互換) |
| `countdown_msec` | 1ms 粒度の短時間対局向け拡張 | `1msec` (本リポ独自) |
| `fischer` | 1 手ごとに増分加算 | `1sec` |
| `stopwatch` | 分単位切り捨ての shogi-server 旧挙動 | `1min` |

各方式が参照する `[vars]` と、未設定時の値は以下。`CLOCK_KIND` 自体の既定は
`countdown`。数値は `u32` の範囲で指定する。

| 方式 | 本体時間 | 秒読み / 増分 |
|---|---|---|
| `countdown` | `TOTAL_TIME_SEC=600` | `BYOYOMI_SEC=10`（秒読み） |
| `countdown_msec` | `TOTAL_TIME_MS=600000` | `BYOYOMI_MS=10000`（秒読み） |
| `fischer` | `TOTAL_TIME_SEC=600` | `BYOYOMI_SEC=10`（増分） |
| `stopwatch` | `TOTAL_TIME_MIN=10` | `BYOYOMI_MIN=1`（秒読み） |

これはパーサの既定値であり、同梱の環境別設定は後述の preset を使う。

## グローバル既定 (`CLOCK_KIND`)

`CLOCK_PRESETS` を未設定 / 空文字 / 空配列にすると、`CLOCK_KIND` と上記の `[vars]` から
組み立てた global clock が **すべての** `game_name` に適用される（後方互換モード）。

最小構成例 (`countdown` / 10 分 + 10 秒読み):

```toml
CLOCK_KIND = "countdown"
TOTAL_TIME_SEC = "600"
BYOYOMI_SEC = "10"
CLOCK_PRESETS = "[]"
```

## 複数 clock 併用 (`CLOCK_PRESETS`)

1 つの worker で「短時間対局」「中時間対局」「Floodgate 互換 1sec」のように
複数の clock を併用したい場合は `CLOCK_PRESETS` に 1 件以上の preset を登録する。

`CLOCK_PRESETS` は **JSON 配列文字列** で渡す。各要素は `game_name` と
`ClockSpec` (`kind` + 各方式の値フィールド) の組。任意の `max_moves` で
その preset の手数上限も指定できる（省略時は 256 手）:

```toml
CLOCK_PRESETS = '''[
  {"game_name":"byoyomi-msec-10-100","kind":"countdown_msec","total_time_ms":10000,"byoyomi_ms":100},
  {"game_name":"byoyomi-120-5","kind":"countdown","total_time_sec":120,"byoyomi_sec":5},
  {"game_name":"floodgate-600-10","kind":"countdown","total_time_sec":600,"byoyomi_sec":10,"max_moves":256}
]'''
```

クライアントは LOGIN handle の `<game_name>` 部分で preset を選択する:

```text
LOGIN alice+floodgate-600-10+black password
```

### strict mode

`CLOCK_PRESETS` に **1 件でも** preset を登録すると strict mode が有効化され、
未登録の `game_name` を含む LOGIN は `LOGIN_LOBBY:incorrect unknown_game_name`
（lobby 経路）または GameRoom DO 側のマッチ不成立で拒否される。

そのため、本リポジトリ同梱の Worker を deploy する際は、運用上必要な
`game_name` をすべて preset として宣言すること。SETBUOY で利用する
`game_name` も同じ preset 表に登録する必要がある。

### 命名規則 (推奨)

ユーザが clock 内容を `game_name` から推測できるよう、以下の命名で揃えるのが
本リポジトリ同梱例の方針:

| 方式 | 形式 | 例 |
|---|---|---|
| `countdown` | `byoyomi-<total_sec>-<byoyomi_sec>` | `byoyomi-600-10` |
| `countdown` (Floodgate 互換 alias) | `floodgate-<total_sec>-<byoyomi_sec>` | `floodgate-600-10` |
| `countdown_msec` | `byoyomi-msec-<total_sec>-<byoyomi_ms>` | `byoyomi-msec-10-100` |
| `fischer` | `fischer-<total_sec>-<increment_sec>F` | `fischer-300-10F` |
| `stopwatch` | `stopwatch-<total_min>-<byoyomi_min>M` | `stopwatch-10-1M` |

### バリデーション

`parse_clock_presets` (`crates/rshogi-csa-server-workers/src/config.rs`) が以下を
検査する:

- JSON として parseable であること
- `game_name` の重複なし
- `total_time_*` が `> 0` (sudden death だけは byoyomi/increment が `0` でも OK)
- `max_moves` は省略、または `> 0`

不正な preset は Lobby と GameRoom で扱いが異なる。Lobby は
`invalid_clock_presets` を記録し、未登録名の拒否を一時的に無効化する。
GameRoom は設定を解決できず、`clock_spec_resolution_failed` と
`##[ERROR] clock spec for '<game_name>' could not be resolved` でマッチを不成立にする。
Worker 全体が起動に失敗するわけではない。設定変更後は対局が開始できることまで
確認し、上記ログが出る場合は JSON、フィールド名、重複を修正する。

## Floodgate 互換性

- `countdown` (`Time_Unit:1sec`) は本家 Floodgate と互換。Floodgate 互換棋力
  統計に流用するなら `countdown` または `floodgate-` 命名 alias を使う。
- `countdown_msec` (`Time_Unit:1msec`) は本リポ独自拡張で本家 Floodgate には
  存在しない。短時間対局を高速に回したい用途専用。

## 同梱 deploy 例

`wrangler.staging.toml` / `wrangler.production.toml` は同じ preset を登録している:

| `game_name` | 用途 |
|---|---|
| `byoyomi-msec-10-100` | 数秒〜十数秒で 1 局を回す高速 smoke / dev loop |
| `byoyomi-120-5` | 切断 → 再接続検証など 2 〜 5 分規模の中時間対局 |
| `fischer-60-5F` | 本体 60 秒 + 1 手 5 秒加算 |
| `floodgate-600-10` | Floodgate 互換 10 分 + 10 秒読みの本番対局 |
| `floodgate-600-10-moves256` | 同じ時計で手数上限 256 手 |
| `floodgate-600-10-moves512` | 同じ時計で手数上限 512 手 |

別 clock を追加するときは `CLOCK_PRESETS` に entry を増やし、
[デプロイ手順](deployment.md) に従って対象環境へ反映する。
既に成立した対局の時計は変更されず、新しい対局から適用される。

## 関連 doc

- [`protocol-reference.md`](protocol-reference.md) §9.4 — `Time_Unit:1msec`
  独自拡張の Game_Summary 表記定義。
- 実装: `crates/rshogi-csa-server/src/game/clock.rs::ClockSpec`
- 設定パーサ: `crates/rshogi-csa-server-workers/src/config.rs::parse_clock_presets`

ライブ着手通知と観戦履歴の `T` も上表の `Time_Unit` 単位で送信する。
CSA V2 棋譜の保存時は秒へ変換するため、時計方式によらず棋譜の単位は秒となる。
