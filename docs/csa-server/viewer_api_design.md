# 対局 viewer 配信 API/経路設計

ramu-shogi (Web / Desktop) から rshogi csa-server-workers の R2 棋譜を閲覧するための
配信経路と HTTP / WebSocket API を定義する設計ドキュメント。

親 Issue: [#542](https://github.com/SH11235/rshogi/issues/542)
親 Epic: [#541](https://github.com/SH11235/rshogi/issues/541)

## 1. ゴール / 非ゴール

### ゴール

- ramu-shogi viewer が **対局済み** CSA 棋譜を 1 局単位で取得できる
- ramu-shogi viewer が **棋譜一覧** を新着順かつページング可能な形で取得できる
- ramu-shogi viewer が **進行中対局** を WebSocket 経由でほぼリアルタイムに観戦できる
- 既存の `KIFU_BUCKET` / `FLOODGATE_HISTORY_BUCKET` への書き込みパスを変更しない

### 非ゴール (将来拡張)

- 評価値・PV など探索情報の配信 (CSA 単独には含まれない。別 Issue で議論)
- 検索 / フィルタ (handle / 期間絞り込み)
- 棋譜編集・削除
- 観戦者間チャット
- public な R2 直公開 (cost / アクセス制御の柔軟性で劣るため非採用)

## 2. 前提と既存資産

### csa-server-workers 側 (本リポ)

- R2 bucket binding:
  - `KIFU_BUCKET` (`crates/rshogi-csa-server-workers/src/config.rs:40`)
  - `FLOODGATE_HISTORY_BUCKET` (`config.rs:44`)
- 終局時の書き込み (`crates/rshogi-csa-server-workers/src/game_room.rs:1075` 付近):
  - **日付パスキー**: `<YYYY>/<MM>/<DD>/<game_id>.csa` (一次正本)
  - **ID 逆引きキー**: `kifu-by-id/<encoded_game_id>.csa` (`x1_paths.rs:32`)
  - 双方に同じ CSA V2 本文を書く (二重化)。
- Floodgate 履歴: `floodgate-history/<YYYY>/<MM>/<DD>/<HHMMSS>-<game_id>.json`
  (`crates/rshogi-csa-server-workers/src/floodgate_history.rs`)
- 既存ルーティング (`crates/rshogi-csa-server-workers/src/router.rs`):
  - `GET /` / `GET /health` (識別 / ヘルス)
  - `GET /ws/<room_id>` (WebSocket Upgrade、CSA プロトコル対局)

### ramu-shogi 側 (別リポ、public)

- `apps/web/wrangler.toml` で既に **Service Binding パターン** を採用:
  - `[[services]] binding = "BACKEND" service = "ramu-shogi-backend"`
  - `worker/index.ts` が `/api/*` を BACKEND にプロキシしている。
- 同じ Cloudflare アカウント (`sh11235.workers.dev`) に staging / production が共存。

## 3. 配信経路の選定

### 候補比較

| # | 案 | 概要 | 長所 | 短所 |
|---|---|---|---|---|
| 1 | **同一アカウント Service Binding** (採用) | ramu-shogi の Web Worker から `[[services]] binding = "RSHOGI_KIFU"` で csa-server-workers を呼ぶ | 既存パターンと同形。inter-Worker 通信は無料・低レイテンシ。アクセス制御を csa-server 側で完結 | csa-server-workers に HTTP API ルートを追加する必要がある |
| 2 | ramu-shogi 側に R2 readonly binding | ramu-shogi の Worker が直接 `KIFU_BUCKET` を read | 経路最短 | bucket 命名規則を ramu-shogi が知る必要があり結合が増える。書き込み権限分離が wrangler binding 単位になる |
| 3 | 公開 R2 + 署名 URL | bucket を public 化、または S3 互換の presigned URL | viewer 側コードが最薄 | アクセス制御が粗い (handle / room の private 化が困難)。一覧の listing が R2 API direct 露出になる |
| 4 | 別 配信専用 Worker | csa-server とは別 Worker を立てて R2 を読む | 関心の分離 | Worker 数とデプロイパイプラインが増える。既存と二重メンテ |

**採用**: **#1 同一アカウント Service Binding**。

### 理由

- ramu-shogi-backend の既存パターンを踏襲でき、wrangler.toml と `worker/index.ts` の追加修正が最小。
- viewer 側のアクセス制御 (private 棋譜・admin 限定オプション) を csa-server-workers で集中管理できる。
- 同一アカウント Service Binding は Cloudflare 上で課金/ネットワーク負担が無く、cross-account 構成のような認可ヘッダ設計も不要。

将来 cross-account 構成が必要になった場合 (例: ramu-shogi が別アカウントで運用される) は、
本設計の HTTP API 形状はそのまま流用でき、Service Binding を `fetch(url)` ベースに置き換えるだけで対応可能。

## 4. R2 オブジェクトキー設計

### 既存 (変更しない)

- `<YYYY>/<MM>/<DD>/<game_id>.csa` — 日付パス (一次正本、一覧の listing 元)
- `kifu-by-id/<encoded_game_id>.csa` — ID 逆引き (単局取得用)
- `floodgate-history/<YYYY>/<MM>/<DD>/<HHMMSS>-<game_id>.json` — Floodgate メタ

### 追加する補助インデックス (検討)

一覧取得を効率化するため、以下のいずれかを採用する。最終決定は実装フェーズで行う。

- **A. R2 list で十分案 (採用優先)**: `list({prefix: "YYYY/MM/DD/", limit: N, cursor})` を
  日付前方から逆順で走査する。Floodgate 履歴側は既に `list_recent(N)` 実装あり
  (`floodgate_history.rs`)。書き込みパスを増やさず実装できる。
- **B. 追加インデックスオブジェクト案**: `index/games-latest.json` に直近 N 件のメタ概要を
  pre-aggregate。読み込みは O(1) だが書き込み側で更新ロジックが追加で必要。

実装は A から始め、性能課題が顕在化した時点で B を追加する (YAGNI)。

### メタデータ抽出

一覧に必要な (handle / clock / 結果 / 開始時刻) は CSA V2 本文から都度パースする。
回帰の少ないシンプルな経路で開始し、性能課題が出たら R2 metadata field
(`putOptions.customMetadata`) に切り出す方向でフォローアップ。

## 5. API 仕様

ramu-shogi 側からは Service Binding 経由 (`env.RSHOGI_KIFU.fetch()`) で呼ぶ前提。
csa-server-workers 側ではすべて `/api/v1/...` プレフィクス下に配置し、既存の `/ws/*` 経路と
名前空間を分離する。

### 5.1 一覧取得

```
GET /api/v1/games?cursor=<opaque>&limit=<N>
```

**Query**:
- `limit`: 1〜100 (default 50)
- `cursor`: 直前レスポンスの `next_cursor` を引き継ぐ (新規アクセスでは省略)

**Response 200 (JSON)**:
```json
{
  "games": [
    {
      "game_id": "lobby-cross-fischer-...-1777391025209",
      "started_at_ms": 1777391025209,
      "ended_at_ms": 1777392877244,
      "black_handle": "alice",
      "white_handle": "bob",
      "result": "WIN_BLACK",
      "moves_count": 142,
      "clock": {"kind": "fischer", "total_sec": 300, "byoyomi_sec": 10}
    }
  ],
  "next_cursor": "<opaque>" | null
}
```

**実装方針**: `KIFU_BUCKET.list({prefix, cursor, limit})` を当日 day-shard から逆順走査。
返却項目は CSA 本文の最小パース (header) で組み立てる。

### 5.2 単局取得

```
GET /api/v1/games/<game_id>
```

**Response 200**:
```json
{
  "game_id": "...",
  "csa": "V2\nN+alice\n...",
  "meta": { /* 5.1 と同じ shape */ }
}
```

**Response 404**: 該当 game_id のキーが両方 (date / by-id) で存在しない場合。

**実装方針**: `kifu_by_id_object_key` で逆引き → 取れなければ `KIFU_BUCKET.get` の date path
を試す (date が分からないので fallback としては listing 走査か、game_id に時刻情報を含める
既存規約を活用する。実装時に決定)。

### 5.3 live 観戦 (WS)

```
GET /api/v1/spectate/<game_id>  (Upgrade: websocket)
```

進行中対局を観戦するための WebSocket。詳細仕様は ramu-shogi#26 (live Issue) で詳細化するが、
配信側として最低限以下を提供する:

- 接続時に **初期スナップショット** (現在までの指し手 + 残り時間 + clock 設定) を送る。
- 各 1 手ごとに `{type: "move", csa_move: "+7776FU", elapsed_ms: 1234, remaining: {...}}` を push。
- 終局時 `{type: "end", result: "WIN_BLACK"}` を送って close。
- 対局未開始 / 終了済みは 404 / 410 相当で reject。

**実装方針**: 既存の `GameRoom` DO に spectator slot を追加し、`spectator_control.rs` の
存在から見るに既に部分実装の足場がある。Hibernation pattern と整合する形で公開する。

### 5.4 認可

- staging: 無認可 (CORS は ramu-shogi 系 origin に絞る)
- production: 既定無認可 (Floodgate 棋譜は公開前提) + ADMIN-only エンドポイントは別パスで分ける
- private 棋譜が将来必要になった場合: `Authorization: Bearer <token>` ベースを後付けで追加可能な構造を維持

### 5.5 CORS

ramu-shogi の `apps/web/worker/index.ts` 経由 (Service Binding) で呼ぶ場合、CORS は
ramu-shogi 側の Worker が処理する。csa-server-workers 直接 fetch は許可しない方針で、
`Origin` チェックを既存の origin allowlist (`origin.rs`) と統合する。

## 6. ramu-shogi 側の取り合わせ

ramu-shogi の Web は既に `worker/index.ts` で `/api/*` を BACKEND service binding に
プロキシしている。同様に rshogi csa-server に対しては:

- 新 binding: `[[services]] binding = "RSHOGI_KIFU" service = "rshogi-csa-server-workers"`
- ルーティング: `/api/rshogi/*` を `RSHOGI_KIFU` に転送 (path rewriting で `/api/v1/...` に変換)

Desktop アプリは Tauri なので Web の Worker を経由せず直接 `fetch` する。
本番 URL (`https://rshogi-csa-server-workers.sh11235.workers.dev/api/v1/...`) を
環境変数で切り替え可能に保つ。

## 7. 進行順 / マイルストーン

1. **本ドキュメントレビュー** (本 PR) — 経路 + API shape 合意
2. **csa-server-workers に `/api/v1/games` 追加** (一覧 + 単局)
   - R2 list ロジック実装
   - CSA header からのメタ抽出
   - `cargo test` (wasm32 ビルドチェック含む) グリーン
3. **ramu-shogi#24 (viewer MVP)** が単局 API スタブ → 実 API へ移行可能になる
4. **ramu-shogi#25 (一覧)** が一覧 API に依存して実装される
5. **ramu-shogi#26 (live) と本ドキュメント 5.3 の WS 仕様確定** を同時に進める
6. ADMIN / private エンドポイント等の拡張 (必要が顕在化したら)

## 8. オープンクエスチョン

- [ ] 単局 fallback (by-id 失敗時の date 走査) の game_id 規約 — 既存命名規則の整理が必要
- [ ] 一覧の order key を「終局時刻」「開始時刻」「matched_at」のどれにするか
  - R2 list は lexicographic 順なので、key prefix の選び方が結果に直結する
- [ ] Floodgate 履歴と通常棋譜を一覧で混在させるか分けるか
- [ ] live spectator の最大同時接続上限と Hibernation の relationship
