//! WebSocket に紐づけるロール情報 (`WsAttachment`)。
//!
//! Cloudflare Workers の WebSocket Hibernation では、各 WebSocket に対して
//! `serialize_attachment` で JSON 互換の値を保存できる。この値は isolate が
//! 凍結されても復帰後に `deserialize_attachment` で取り出せるため、
//! 「この ws がどの対局者か」というマッピングを DO 内の in-memory 変数に
//! 頼らず保持できる。
//!
//! 本モジュールは attachment の形式と (de)serialize 規約だけを定義し、
//! worker ランタイムに依存しない。単体テストはホスト target で走る。

use serde::{Deserialize, Serialize};

/// WS 受信 1 メッセージあたりの最大バイト数 (https://github.com/SH11235/rshogi/issues/627)。
///
/// CSA LOGIN / CHAT / move 行 / lobby command (`LOGIN_LOBBY` /
/// `CHALLENGE_LOBBY` / `LOGOUT_LOBBY` 等) はいずれも数百バイト未満で収まる
/// 設計だが、Cloudflare WebSocket は最大 32 MiB のメッセージを許容するため、
/// アプリ層で明示的な上限を入れないと巨大ペイロードが parser / allocation に
/// 流れて CPU と memory を浪費する。
///
/// 4096 バイトはプロトコル正常系に対し 1 桁以上の余裕がある安全側の値。
/// 受信側ハンドラで `raw.len() > MAX_WS_LINE_BYTES` を満たした WS は
/// `1009 Message Too Big` で即時 close する契約。判定は `trim_end_matches`
/// で改行を削る **前** の元バイト数に対して行う。
pub const MAX_WS_LINE_BYTES: usize = 4096;

/// Spectator pending_queue に積める行数上限 (https://github.com/SH11235/rshogi/issues/627)。
///
/// snapshot 送信中に到着した broadcast 行を per-WS キューに積む経路で、
/// チャット flood 等で無制限に成長する DoS 経路を遮断する。1 局 ≤ 512 手 +
/// CHAT / START / 終局通知の余裕として 1024 を採る。
pub const MAX_SPECTATOR_QUEUE_ITEMS: usize = 1024;

/// Spectator pending_queue の累計バイト数上限 (https://github.com/SH11235/rshogi/issues/627)。
///
/// 行数上限とは独立に、長文 CHAT が積み上がるケースに備えて bytes 上限も
/// 課す。`pending_queue` の各行 (`String`) の `len()` の総和で判定する。
pub const MAX_SPECTATOR_QUEUE_BYTES: usize = 64 * 1024;

/// 先手・後手の別。`rshogi_csa_server::types::Color` が `serde::Serialize` を
/// 実装していないため、attachment 用には独自のタグ付き列挙を使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// 先手。
    Black,
    /// 後手。
    White,
}

impl Role {
    /// 相手の手番。
    pub fn opposite(self) -> Self {
        match self {
            Role::Black => Role::White,
            Role::White => Role::Black,
        }
    }

    /// `rshogi_csa_server::types::Color` へ変換する。
    pub fn to_core(self) -> rshogi_csa_server::types::Color {
        match self {
            Role::Black => rshogi_csa_server::types::Color::Black,
            Role::White => rshogi_csa_server::types::Color::White,
        }
    }

    /// `rshogi_csa_server::types::Color` から変換する。
    pub fn from_core(color: rshogi_csa_server::types::Color) -> Self {
        match color {
            rshogi_csa_server::types::Color::Black => Role::Black,
            rshogi_csa_server::types::Color::White => Role::White,
        }
    }
}

/// 1 WebSocket に紐づく attachment 値。
///
/// # バリアント
///
/// - [`WsAttachment::Pending`]: LOGIN 到着前の匿名接続。`websocket_message`
///   ハンドラは最初に受信した行を LOGIN として解釈しようとする。
/// - [`WsAttachment::Player`]: 認証済みプレイヤ。色・ハンドル・game_name を保持する。
/// - [`WsAttachment::Spectator`]: 観戦者。`room_id` で観戦対象の部屋を特定する。
///   観戦系メッセージ (`%%MONITOR2ON/OFF`, `%%CHAT`) の経路判定と broadcast
///   fanout の対象判定に使う。
///
/// serde タグ付き形式を使い、新 variant を追加しても既存 attachment を
/// 読み壊さない前方互換性を確保する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsAttachment {
    /// LOGIN 未完了の匿名接続。
    Pending,
    /// 認証済みプレイヤ。
    Player {
        /// 割り当てられた手番色。
        role: Role,
        /// CSA LOGIN の `<handle>` 部分。プレイヤ識別子として使う。
        handle: String,
        /// CSA LOGIN の `<game_name>` 部分。マッチング時の同名性チェックに使う。
        game_name: String,
    },
    /// 観戦者。`/ws/<room_id>/spectate` から接続したセッションに付与する。
    ///
    /// Player との違いは「盤面を動かす権限を持たず、broadcast を一方向受信する」点。
    /// `room_id` は観戦対象の部屋 ID で、`GameRoom` DO が broadcast fanout 時に
    /// `WsAttachment::Spectator` 持ちセッション全てへ配信する判定で使う。
    Spectator {
        /// 観戦対象の部屋 ID。
        room_id: String,
        /// snapshot 送信中かどうか (`Monitor2On` Accept 経路に入ると `true`、
        /// `##[MONITOR2] END` 送出後に `false`)。`true` の間はこの ws への
        /// 指し手 broadcast を per-ws pending queue に積み、snapshot 完了後に
        /// flush する race-resolution 用フラグ。
        ///
        /// 設計上は in-memory のみ扱いだが、DO の WebSocket Hibernation 経由で
        /// 異なる handler 呼び出し間で参照する必要があるため attachment 経由で
        /// 永続化する (= `serialize_attachment` に乗る)。Hibernation 後に「snapshot
        /// 送信中」状態が復帰してしまうのを防ぐため、`#[serde(default)]` で
        /// `false` を既定値として復元する規則 (= 万一 hibernation 中に snapshot
        /// 送信処理が中断したら、復帰後の DO は queue を空 / フラグ false で
        /// 開始する)。
        #[serde(default)]
        snapshot_in_progress: bool,
        /// snapshot に含めた最終 ply (1 始まり、初手前なら 0)。
        ///
        /// snapshot 完了後に pending queue を flush する際、`ply > last_ply_in_snapshot`
        /// の broadcast 行のみ送出して重複を排除する。`snapshot_in_progress = false`
        /// に戻った後も値は保持する (queue 経由で挙動を共有しないため副作用は無いが、
        /// 攻撃的に reset しないことで race の窓を狭くする)。
        #[serde(default)]
        last_ply_in_snapshot: u32,
        /// snapshot 送信中に到着した broadcast 行を「行 + その手の ply」の形で
        /// 保持する pending queue。snapshot 完了後に順次 flush する。
        ///
        /// `Vec<(String, Option<u32>)>`: 第 1 要素が CSA 行、第 2 要素が手数
        /// (`None` は START / 終局通知 / CHAT 等の非指し手 broadcast で、queue
        /// 経由でも常に flush 対象)。
        ///
        /// MVP では上限を設けない (1 局 ≤ 512 手のため pending queue は数十行
        /// 程度に収まる想定)。性能課題が顕在化したら別 Issue で gating する。
        #[serde(default)]
        pending_queue: Vec<(String, Option<u32>)>,
    },
}

impl WsAttachment {
    /// プレイヤ attachment を構築する補助関数。
    pub fn player(role: Role, handle: impl Into<String>, game_name: impl Into<String>) -> Self {
        Self::Player {
            role,
            handle: handle.into(),
            game_name: game_name.into(),
        }
    }

    /// 観戦者 attachment を構築する補助関数。
    ///
    /// `snapshot_in_progress` / `last_ply_in_snapshot` / `pending_queue` は
    /// すべて default 値で初期化する。snapshot 送信経路に入る際に DO 側で
    /// `snapshot_in_progress = true` に切り替え、`##[MONITOR2] END` 送出後に
    /// `false` に戻す契約。
    pub fn spectator(room_id: impl Into<String>) -> Self {
        Self::Spectator {
            room_id: room_id.into(),
            snapshot_in_progress: false,
            last_ply_in_snapshot: 0,
            pending_queue: Vec::new(),
        }
    }
}

/// `LOGIN <handle>+<game_name>+<color> <password>` 形式の LOGIN 名を分解する。
///
/// TCP 版 (`crates/rshogi-csa-server-tcp/src/server.rs::parse_handle`) と
/// 同一のコンベンションを採用する。Floodgate 以来の慣習で、クライアントが
/// 希望する手番色まで名前に埋めてくる。
///
/// # 戻り値
/// `(handle, game_name, role)` のタプルを返す。形式が崩れていれば `None`。
pub fn parse_login_handle(raw: &str) -> Option<(String, String, Role)> {
    let mut it = raw.split('+');
    let handle = it.next()?.to_owned();
    let game_name = it.next()?.to_owned();
    let color_s = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let role = match color_s.to_ascii_lowercase().as_str() {
        "black" | "b" | "sente" => Role::Black,
        "white" | "w" | "gote" => Role::White,
        _ => return None,
    };
    if handle.is_empty() || game_name.is_empty() {
        return None;
    }
    Some((handle, game_name, role))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_roundtrips_via_json() {
        let att = WsAttachment::Pending;
        let s = serde_json::to_string(&att).unwrap();
        let back: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, back);
    }

    #[test]
    fn player_roundtrips_via_json() {
        let att = WsAttachment::player(Role::Black, "alice", "floodgate-600-10");
        let s = serde_json::to_string(&att).unwrap();
        let back: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, back);
    }

    #[test]
    fn player_json_has_expected_shape() {
        let att = WsAttachment::player(Role::White, "bob", "gamename");
        let s = serde_json::to_string(&att).unwrap();
        // `#[serde(tag = "type")]` により `type` フィールドが付く想定。
        assert!(s.contains("\"type\":\"Player\""));
        assert!(s.contains("\"role\":\"White\""));
        assert!(s.contains("\"handle\":\"bob\""));
        assert!(s.contains("\"game_name\":\"gamename\""));
    }

    #[test]
    fn role_conversion_is_bijective() {
        for r in [Role::Black, Role::White] {
            assert_eq!(Role::from_core(r.to_core()), r);
            assert_eq!(r.opposite().opposite(), r);
        }
    }

    #[test]
    fn parse_login_handle_basic() {
        assert_eq!(
            parse_login_handle("alice+game1+black"),
            Some(("alice".to_owned(), "game1".to_owned(), Role::Black))
        );
        assert_eq!(
            parse_login_handle("bob+game1+W"),
            Some(("bob".to_owned(), "game1".to_owned(), Role::White))
        );
        assert_eq!(
            parse_login_handle("charlie+floodgate-600-10+SENTE"),
            Some(("charlie".to_owned(), "floodgate-600-10".to_owned(), Role::Black))
        );
    }

    #[test]
    fn parse_login_handle_rejects_malformed() {
        assert!(parse_login_handle("alice").is_none());
        assert!(parse_login_handle("alice+game1").is_none());
        assert!(parse_login_handle("alice+game1+purple").is_none());
        assert!(parse_login_handle("+game1+black").is_none());
        assert!(parse_login_handle("alice++black").is_none());
        assert!(parse_login_handle("alice+game1+black+extra").is_none());
    }

    #[test]
    fn spectator_roundtrips_via_json() {
        let att = WsAttachment::spectator("room-20260101-0001");
        let s = serde_json::to_string(&att).unwrap();
        let back: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, back);
    }

    #[test]
    fn spectator_json_has_expected_shape() {
        let att = WsAttachment::spectator("room-xyz");
        let s = serde_json::to_string(&att).unwrap();
        // `#[serde(tag = "type")]` の下では variant 名が `type` 値に入る。
        assert!(s.contains("\"type\":\"Spectator\""));
        assert!(s.contains("\"room_id\":\"room-xyz\""));
    }

    #[test]
    fn spectator_snapshot_state_round_trips_via_serde() {
        // snapshot 送信中の attachment が serialize → deserialize で完全復元
        // されること。in-memory 値だが Hibernation 経由で他 handler から見える
        // 必要があるため永続化する設計。
        let att = WsAttachment::Spectator {
            room_id: "room-xyz".to_owned(),
            snapshot_in_progress: true,
            last_ply_in_snapshot: 7,
            pending_queue: vec![
                ("+5756FU,T2".to_owned(), Some(8)),
                ("##[CHAT] alice: hi".to_owned(), None),
            ],
        };
        let s = serde_json::to_string(&att).unwrap();
        let restored: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, restored);
    }

    #[test]
    fn spectator_legacy_attachment_defaults_snapshot_fields() {
        // 旧 schema (snapshot_in_progress / last_ply_in_snapshot / pending_queue
        // 導入前) で永続化された attachment を deserialize した場合に、新 field
        // が default (false / 0 / Vec::new()) で復元されること。Hibernation 復帰時
        // の互換性として固定する。
        let legacy = r#"{"type":"Spectator","room_id":"room-xyz"}"#;
        let restored: WsAttachment = serde_json::from_str(legacy).unwrap();
        match restored {
            WsAttachment::Spectator {
                room_id,
                snapshot_in_progress,
                last_ply_in_snapshot,
                pending_queue,
            } => {
                assert_eq!(room_id, "room-xyz");
                assert!(!snapshot_in_progress);
                assert_eq!(last_ply_in_snapshot, 0);
                assert!(pending_queue.is_empty());
            }
            other => panic!("expected Spectator, got {other:?}"),
        }
    }

    #[test]
    fn player_and_spectator_are_distinct_types() {
        // 同一ハンドル / ID でも Player と Spectator は別 variant として比較される。
        let player = WsAttachment::player(Role::Black, "alice", "room-1");
        let spec = WsAttachment::spectator("room-1");
        assert_ne!(player, spec);
    }

    /// https://github.com/SH11235/rshogi/issues/627: 受信メッセージサイズ上限の値が CSA プロトコル正常系に対し
    /// 十分余裕を持っていること、かつ Cloudflare WS の最大 32 MiB より
    /// 大幅に小さいことの sanity check。値変更時にビルド時 regression を検出する。
    /// `const { assert!(..) }` を使うことで run-time コストゼロで固定する。
    #[test]
    fn ws_message_size_limit_is_sane() {
        const _: () = {
            // 通常の CSA 行 (LOGIN / move / CHAT 等) は数百バイト未満で収まる。
            // 1024 バイトを下回ると正常系を弾く恐れがあるため最低限の floor を設ける。
            assert!(MAX_WS_LINE_BYTES >= 1024);
            // 1 MiB を超えると DoS 防御として機能しないため天井も設ける。
            assert!(MAX_WS_LINE_BYTES <= 1024 * 1024);
        };
    }

    /// https://github.com/SH11235/rshogi/issues/627: spectator pending_queue 上限値が 1 局の通常運用に対し
    /// 十分余裕を持っていることの sanity check (ビルド時固定)。
    #[test]
    fn spectator_queue_limits_are_sane() {
        const _: () = {
            // 1 局の指し手は最大 512 ply 程度。CHAT / START / 終局通知の余裕を
            // 含めても 512 を下回ると正常系を弾く恐れがある。
            assert!(MAX_SPECTATOR_QUEUE_ITEMS >= 512);
            // 1 行 ≤ MAX_WS_LINE_BYTES * (件数上限) の理論上限よりは小さくて良いが、
            // bytes 上限が件数上限 * 32 byte を下回ると、通常対局でも先に bytes 側で
            // 詰まる。
            assert!(MAX_SPECTATOR_QUEUE_BYTES >= MAX_SPECTATOR_QUEUE_ITEMS * 32);
        };
    }
}
