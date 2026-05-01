# Clock 既定差分 (staging / production)

`rshogi-csa-server-workers` の staging / production 環境で `[vars]` 既定値が
意図的に異なる箇所のうち、対局時計 (`CLOCK_KIND` 系) に関する差分を集約する
ops 仕様 single source of truth。両環境の wrangler コメントは self-contained
だが、cross-environment 比較・Floodgate 互換性の判断・ops 判断基準は本 doc
に集約する。

## 環境別 clock 既定値

| 環境 | `CLOCK_KIND` | 持ち時間 | 秒読み / Increment | `Time_Unit` |
|---|---|---|---|---|
| staging | `countdown_msec` | 10 秒 (`TOTAL_TIME_MS=10000`) | 100ms (`BYOYOMI_MS=100`) | `1msec` (本リポ独自拡張) |
| production | `countdown` | 10 分 (`TOTAL_TIME_SEC=600`) | 10 秒 (`BYOYOMI_SEC=10`) | `1sec` (本家 Floodgate 互換) |

## 差分の意図

- **production**: 本家 Floodgate と同じ秒単位 countdown 既定。Floodgate
  互換棋力統計を取れる構成。
- **staging**: 短時間 E2E (CI / smoke) の feedback を維持するため独自拡張
  `countdown_msec` (`Time_Unit:1msec`) を採用。1 局を数秒〜十数秒で完走
  させる速度を狙う。

両者の用途が異なるため統一は機能的損失となる。staging は短時間 E2E の
feedback、production は Floodgate 互換棋力統計をそれぞれの既定として保つ。

## CSA Floodgate との互換性

- production の `countdown` (`Time_Unit:1sec`) は本家 Floodgate と互換。
- staging の `countdown_msec` (`Time_Unit:1msec`) は **本リポ独自拡張** で
  本家 Floodgate には存在しない。
- **staging で取れた R2 棋譜は本家 Floodgate 互換棋力統計には流用不可**
  (`Time_Unit:1msec` の独自拡張で記録されているため、棋力統計汚染リスク
  を伴う)。

## ops 判断基準

- smoke / E2E 用途 → staging (短時間 feedback を活用)。
- 棋力統計用途 → production (Floodgate 互換)。

## production smoke 運用注意

- production の 1 局 **完走待ち** は 25〜30 分を見込む (Cloudflare Worker
  の cold start とは別、対局時間自体が長いため)。
- worst case 計算: `total_time_sec=600 × 2 + byoyomi_sec=10 × max_moves=256`
  ≈ 約 85 分。
- production 完走待ちの timeout 見積もりは上記レンジで運用する。

## `game_name` 別 override 経路 (`CLOCK_PRESETS`)

両環境で `CLOCK_PRESETS` のコード経路は配線済。現状は両 toml で
`CLOCK_PRESETS = "[]"` (空配列、strict mode 未発動 = 後方互換動作) のため、
上記 default が全 game_name に適用される。1 件以上登録すれば strict mode
に切り替わり、未登録 game_name の `LOGIN_LOBBY` は
`LOGIN_LOBBY:incorrect unknown_game_name` で拒否される。

ad-hoc 上書きを必要とする運用 (例: production で短時間対局を試したい /
staging で Floodgate 互換 1sec の通電確認をしたい) は、対象環境の
`CLOCK_PRESETS` に preset 1 件以上を登録することで default を変えずに
game_name 別に振り分けられる。preset 命名規則:

- `byoyomi-<total_sec>-<byoyomi_sec>` (countdown 系)
- `fischer-<total_sec>-<increment_sec>F` (fischer 系)

## 関連 config / 関連 doc

- `crates/rshogi-csa-server-workers/wrangler.production.toml` の
  `CLOCK_KIND = "countdown"` セクション (production 実値)
- `crates/rshogi-csa-server-workers/wrangler.staging.toml` の
  `CLOCK_KIND = "countdown_msec"` セクション (staging 実値)
- [`protocol-reference.md`](protocol-reference.md) §9.4 (`Time_Unit:1msec`
  の独自拡張定義)
- [`staging-e2e.md`](staging-e2e.md) §0 (staging E2E runbook 文脈)
- [`lobby_e2e_runbook.md`](lobby_e2e_runbook.md) §6.1 (lobby E2E 文脈)
- 実装位置: `crates/rshogi-csa-server/src/game/clock.rs::MillisecondsCountdownClock::format_summary`
  (`Time_Unit:1msec` を `Game_Summary` に出す経路)
