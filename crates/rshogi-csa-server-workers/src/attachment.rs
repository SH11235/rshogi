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
        /// `%%ADMIN <token>` で `verify_admin_token_str` を通過した session のみ
        /// `true` (https://github.com/SH11235/rshogi/issues/621)。`%%SETBUOY` /
        /// `%%DELETEBUOY` 等の admin 権限要求コマンドで参照する。LOGIN 時点では
        /// 必ず `false` で初期化し、`%%ADMIN` を経由した同一 session 内でのみ
        /// `true` に昇格する (handle 自称 + ADMIN_HANDLE 平文露出を絶つ目的)。
        ///
        /// `#[serde(default)]` により Hibernation 経由で復帰した旧 schema の
        /// attachment は `false` で復元される (admin 権限はセッション再開で
        /// 必ず再認可するべき性質なので保守的既定が妥当)。
        #[serde(default)]
        is_admin: bool,
        /// この接続へ送信済みの終局通知行。休眠後の再送を抑止する。
        #[serde(default)]
        terminal_sent: Vec<String>,
        /// 送信結果が不明なまま再送することを防ぐ。
        #[serde(default)]
        terminal_in_flight: Option<String>,
        /// 配信を打ち切った接続を再利用しないために残す。
        #[serde(default)]
        terminal_aborted: bool,
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
        /// 永続化する (= `serialize_attachment` に乗る)。
        ///
        /// `#[serde(default)]` は **field が欠落した旧 schema** (snapshot 3 field
        /// 導入前) の attachment を deserialize したときに `false` で復元する
        /// ための既定値である。`true` で serialize 済みの値を復元時に `false` へ
        /// 戻す効果は **無い** (serde default は field 不在時にのみ適用され、
        /// `true`→`true` で round-trip する。`spectator_snapshot_state_round_trips_via_serde`
        /// が固定)。したがって snapshot 送信がエラーで中断してこの flag が `true`
        /// のまま永続化されると、以降の broadcast が `send_to_spectators` の per-ws
        /// pending queue に積まれ続け、観戦者が無音フリーズする。中断時は送信経路側
        /// が明示的に `false` へ戻す責務を持つ (`GameRoom::send_spectator_snapshot`
        /// のエラー経路が [`WsAttachment::reset_spectator_snapshot`] で flag を落とし
        /// queue を空にしたうえで ws を close する)。
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
        /// この接続へ送信済みの終局通知行。
        #[serde(default)]
        terminal_sent: Vec<String>,
        /// 送信結果が不明なまま再送することを防ぐ。
        #[serde(default)]
        terminal_in_flight: Option<String>,
        /// 配信を打ち切った接続を再利用しないために残す。
        #[serde(default)]
        terminal_aborted: bool,
    },
}

/// 再開時に裁定から告知を作り直すと、終局手や REJECT が欠けるため保存する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalizingBroadcast {
    /// 復帰後も元の宛先を維持する。
    pub target: rshogi_csa_server::BroadcastTarget,
    /// 裁定だけでは復元できない終局手も保持する。
    pub line: String,
    /// snapshot に含まれる指し手の重複を除くために使う。
    pub ply: Option<u32>,
}

impl From<&rshogi_csa_server::BroadcastEntry> for FinalizingBroadcast {
    fn from(entry: &rshogi_csa_server::BroadcastEntry) -> Self {
        Self {
            target: entry.target,
            line: entry.line.as_str().to_owned(),
            ply: entry.ply,
        }
    }
}

impl WsAttachment {
    /// 送信結果が不明でも再送しないよう、行ごとの配信権を先に永続化する。
    pub fn deliver_terminal<E>(
        &mut self,
        entries: &[FinalizingBroadcast],
        mut send: impl FnMut(&str) -> Result<(), E>,
        mut persist: impl FnMut(&Self) -> Result<(), E>,
        mut close: impl FnMut(u16, &str),
    ) -> Result<(), E> {
        if self.terminal_aborted() {
            return Ok(());
        }
        if self.terminal_in_flight_mut().is_some_and(|line| line.is_some()) {
            return self.abort_terminal(&mut persist, &mut close);
        }
        for entry in entries {
            if !self.addresses(entry.target) {
                continue;
            }
            let before = self.clone();
            let Some(sent) = self.terminal_sent_mut() else {
                continue;
            };
            if sent.contains(&entry.line) {
                continue;
            }
            // snapshot 送信中はキューに積んでも届いた保証にならない。snapshot 完了後の
            // 再開で直接送り、途中で中断すれば未配信として 1011 で閉じられる。
            if matches!(
                self,
                Self::Spectator {
                    snapshot_in_progress: true,
                    ..
                }
            ) {
                return Ok(());
            }
            let in_snapshot = matches!(self, Self::Spectator { last_ply_in_snapshot, .. }
                if entry.ply.is_some_and(|ply| ply <= *last_ply_in_snapshot));
            if in_snapshot {
                self.terminal_sent_mut().unwrap().push(entry.line.clone());
                if let Err(error) = persist(self) {
                    *self = before;
                    return Err(error);
                }
                continue;
            }
            *self.terminal_in_flight_mut().unwrap() = Some(entry.line.clone());
            if let Err(error) = persist(self) {
                *self = before;
                return Err(error);
            }
            if send(&entry.line).is_err() {
                return self.abort_terminal(&mut persist, &mut close);
            }
            let in_flight = self.clone();
            self.terminal_sent_mut().unwrap().push(entry.line.clone());
            *self.terminal_in_flight_mut().unwrap() = None;
            if let Err(error) = persist(self) {
                // 保存前に送信済みなので、再開時も結果不明として扱う。
                *self = in_flight;
                return Err(error);
            }
        }
        Ok(())
    }

    /// 正常終了の close で配信失敗を覆い隠さないために使う。
    pub fn terminal_aborted(&self) -> bool {
        matches!(
            self,
            Self::Player {
                terminal_aborted: true,
                ..
            } | Self::Spectator {
                terminal_aborted: true,
                ..
            }
        )
    }

    fn terminal_in_flight_mut(&mut self) -> Option<&mut Option<String>> {
        match self {
            Self::Player {
                terminal_in_flight, ..
            }
            | Self::Spectator {
                terminal_in_flight, ..
            } => Some(terminal_in_flight),
            Self::Pending => None,
        }
    }

    fn addresses(&self, target: rshogi_csa_server::BroadcastTarget) -> bool {
        use rshogi_csa_server::BroadcastTarget;
        match self {
            Self::Player { role, .. } => {
                matches!(target, BroadcastTarget::All | BroadcastTarget::Players)
                    || matches!(
                        (role, target),
                        (Role::Black, BroadcastTarget::Black)
                            | (Role::White, BroadcastTarget::White)
                    )
            }
            Self::Spectator { .. } => {
                matches!(target, BroadcastTarget::All | BroadcastTarget::Spectators)
            }
            Self::Pending => false,
        }
    }

    /// 自分宛ての終局行をすべて配信済みか。確定時の close を正常終了にしてよいかの判定に使う。
    pub fn terminal_complete(&self, entries: &[FinalizingBroadcast]) -> bool {
        let sent = match self {
            Self::Player { terminal_sent, .. } | Self::Spectator { terminal_sent, .. } => {
                terminal_sent
            }
            Self::Pending => return true,
        };
        entries
            .iter()
            .filter(|entry| self.addresses(entry.target))
            .all(|entry| sent.contains(&entry.line))
    }

    /// 配信を打ち切った印を保存して 1011 で閉じる。以後の試行はこの接続を飛ばす。
    pub(crate) fn abort_terminal<E>(
        &mut self,
        persist: &mut impl FnMut(&Self) -> Result<(), E>,
        close: &mut impl FnMut(u16, &str),
    ) -> Result<(), E> {
        let before = self.clone();
        match self {
            Self::Player {
                terminal_aborted, ..
            }
            | Self::Spectator {
                terminal_aborted, ..
            } => *terminal_aborted = true,
            Self::Pending => return Ok(()),
        }
        let saved = persist(self);
        if saved.is_err() {
            *self = before;
        }
        close(1011, "terminal delivery failed");
        saved
    }

    fn terminal_sent_mut(&mut self) -> Option<&mut Vec<String>> {
        match self {
            Self::Player { terminal_sent, .. } | Self::Spectator { terminal_sent, .. } => {
                Some(terminal_sent)
            }
            Self::Pending => None,
        }
    }

    /// プレイヤ attachment を構築する補助関数。`is_admin` は `false` で初期化
    /// する (admin 権限は `%%ADMIN <token>` を経由したときのみ
    /// [`Self::with_admin`] で `true` に上げる契約)。
    pub fn player(role: Role, handle: impl Into<String>, game_name: impl Into<String>) -> Self {
        Self::Player {
            role,
            handle: handle.into(),
            game_name: game_name.into(),
            is_admin: false,
            terminal_sent: Vec::new(),
            terminal_in_flight: None,
            terminal_aborted: false,
        }
    }

    /// 既存の Player attachment を admin 昇格状態にする。Player 以外の variant
    /// に対しては変更せず元の値を返す (`Spectator` / `Pending` は admin に
    /// しない契約)。
    pub fn with_admin(self) -> Self {
        match self {
            Self::Player {
                role,
                handle,
                game_name,
                is_admin: _,
                terminal_sent,
                terminal_in_flight,
                terminal_aborted,
            } => Self::Player {
                role,
                handle,
                game_name,
                is_admin: true,
                terminal_sent,
                terminal_in_flight,
                terminal_aborted,
            },
            other => other,
        }
    }

    /// admin 昇格済みの Player か。Player 以外は常に `false`。
    pub fn is_admin(&self) -> bool {
        matches!(self, Self::Player { is_admin: true, .. })
    }

    /// 観戦者 attachment を構築する補助関数。
    ///
    /// 接続直後から最初の snapshot 完了まで配信を保留する。
    /// MONITOR2ON を待っている間にも終局し得るため flag は true で始める。
    pub fn spectator(room_id: impl Into<String>) -> Self {
        Self::Spectator {
            room_id: room_id.into(),
            snapshot_in_progress: true,
            last_ply_in_snapshot: 0,
            pending_queue: Vec::new(),
            terminal_sent: Vec::new(),
            terminal_in_flight: None,
            terminal_aborted: false,
        }
    }

    /// Spectator の snapshot 送信状態を初期状態 (flag=false / queue 空) に戻す。
    ///
    /// snapshot 送信経路がエラーで中断すると、`snapshot_in_progress = true` の
    /// まま attachment が残り、`send_to_spectators` が以降の broadcast を per-ws
    /// pending queue に積み続けて観戦者が無音フリーズする。エラー経路でこの
    /// helper を通して flag を落とし queue を空にする。`last_ply_in_snapshot` は
    /// flag=false では参照されないため `0` に戻す。Spectator 以外の variant は
    /// 変更せず元の値を返す。
    pub fn reset_spectator_snapshot(self) -> Self {
        match self {
            Self::Spectator {
                room_id,
                terminal_sent,
                terminal_in_flight,
                terminal_aborted,
                ..
            } => Self::Spectator {
                room_id,
                snapshot_in_progress: false,
                last_ply_in_snapshot: 0,
                pending_queue: Vec::new(),
                terminal_sent,
                terminal_in_flight,
                terminal_aborted,
            },
            other => other,
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
    fn terminal_delivery_failure_matrix_is_at_most_once() {
        use crate::attachment::FinalizingBroadcast;
        use rshogi_csa_server::BroadcastTarget;
        let entries: Vec<_> = ["-5251OU,T3", "#SENNICHITE", "#DRAW"]
            .into_iter()
            .map(|line| FinalizingBroadcast {
                target: BroadcastTarget::All,
                line: line.into(),
                ply: None,
            })
            .collect();
        for (persist_failure, send_failure) in (0..6)
            .map(|i| (Some(i), None))
            .chain((0..3).map(|i| (None, Some(i))))
            .chain([(None, None)])
        {
            for cold in [false, true] {
                let mut att = WsAttachment::player(Role::Black, "black", "game");
                let mut saved = att.clone();
                let mut attempted = Vec::new();
                let mut received = Vec::new();
                let mut closed = Vec::new();
                let mut saves = 0;
                let first = att.deliver_terminal(
                    &entries,
                    |line| {
                        let index = attempted.len();
                        attempted.push(line.to_owned());
                        if send_failure == Some(index) {
                            return Err("send");
                        }
                        received.push(line.to_owned());
                        Ok(())
                    },
                    |value| {
                        let index = saves;
                        saves += 1;
                        if persist_failure == Some(index) {
                            return Err("persist");
                        }
                        saved = value.clone();
                        Ok(())
                    },
                    |code, _| closed.push(code),
                );
                let expected_sends = persist_failure
                    .map(|i: usize| i.div_ceil(2))
                    .or_else(|| send_failure.map(|i| i + 1))
                    .unwrap_or(3);
                let expected_received = send_failure.unwrap_or(expected_sends);
                assert_eq!(
                    first,
                    if persist_failure.is_some() {
                        Err("persist")
                    } else {
                        Ok(())
                    }
                );
                assert_eq!(attempted.len(), expected_sends);
                assert_eq!(received.len(), expected_received);
                assert_eq!(
                    closed,
                    if send_failure.is_some() {
                        vec![1011]
                    } else {
                        vec![]
                    }
                );
                if cold {
                    att = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
                }
                let ambiguous = persist_failure.is_some_and(|i| i % 2 == 1);
                for _ in 0..2 {
                    att.deliver_terminal(
                        &entries,
                        |line| {
                            attempted.push(line.to_owned());
                            Ok::<_, &str>(())
                        },
                        |_| Ok(()),
                        |code, _| closed.push(code),
                    )
                    .unwrap();
                }
                let total_sends = if ambiguous || send_failure.is_some() {
                    expected_sends
                } else {
                    3
                };
                assert_eq!(attempted.len(), total_sends);
                for entry in &entries {
                    assert_eq!(
                        attempted.iter().filter(|line| **line == entry.line).count(),
                        usize::from(entries.iter().take(total_sends).any(|e| e.line == entry.line))
                    );
                }
                assert_eq!(
                    closed,
                    if ambiguous || send_failure.is_some() {
                        vec![1011]
                    } else {
                        vec![]
                    }
                );
            }
        }
    }

    #[test]
    fn terminal_in_flight_resumes_without_resending_and_then_skips_connection() {
        for mut att in [
            WsAttachment::player(Role::Black, "b", "g"),
            WsAttachment::spectator("g"),
        ] {
            *att.terminal_in_flight_mut().unwrap() = Some("#DRAW".into());
            let mut att: WsAttachment =
                serde_json::from_str(&serde_json::to_string(&att).unwrap()).unwrap();
            let entries = [FinalizingBroadcast {
                target: rshogi_csa_server::BroadcastTarget::All,
                line: "#DRAW".into(),
                ply: None,
            }];
            let mut closed = Vec::new();
            let mut saved = None;
            att.deliver_terminal(
                &entries,
                |_| panic!("再送"),
                |value| {
                    saved = Some(value.clone());
                    Ok::<_, ()>(())
                },
                |code, reason| closed.push((code, reason.to_owned())),
            )
            .unwrap();
            assert_eq!(closed, [(1011, "terminal delivery failed".into())]);
            assert!(att.terminal_aborted());
            let mut restored: WsAttachment =
                serde_json::from_str(&serde_json::to_string(&saved.unwrap()).unwrap()).unwrap();
            for value in [&mut att, &mut restored] {
                value
                    .deliver_terminal::<()>(
                        &entries,
                        |_| panic!("再送"),
                        |_| panic!("保存"),
                        |_, _| panic!("再切断"),
                    )
                    .unwrap();
            }
        }
    }

    #[test]
    fn terminal_abort_persistence_failure_remains_retryable_without_resending() {
        let mut att = WsAttachment::player(Role::Black, "b", "g");
        let entries = [FinalizingBroadcast {
            target: rshogi_csa_server::BroadcastTarget::All,
            line: "#DRAW".into(),
            ply: None,
        }];
        let mut saved = att.clone();
        let mut closed = Vec::new();
        let first = att.deliver_terminal(
            &entries,
            |_| Err("send"),
            |value| {
                if value.terminal_aborted() {
                    return Err("persist");
                }
                saved = value.clone();
                Ok(())
            },
            |code, _| closed.push(code),
        );
        assert_eq!(first, Err("persist"));
        assert_eq!(closed, [1011]);
        for mut restored in [att, saved] {
            restored
                .deliver_terminal(
                    &entries,
                    |_| panic!("再送"),
                    |_| Ok::<_, ()>(()),
                    |code, reason| assert_eq!((code, reason), (1011, "terminal delivery failed")),
                )
                .unwrap();
            assert!(restored.terminal_aborted());
        }
    }

    #[test]
    fn terminal_delivery_respects_targets_and_snapshot_queue() {
        use crate::attachment::FinalizingBroadcast;
        use rshogi_csa_server::BroadcastTarget;
        let entries: Vec<_> = [
            BroadcastTarget::Black,
            BroadcastTarget::White,
            BroadcastTarget::Players,
            BroadcastTarget::Spectators,
            BroadcastTarget::All,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, target)| FinalizingBroadcast {
            target,
            line: format!("REJECT:{i}"),
            ply: Some(i as u32),
        })
        .collect();
        for (mut att, expected) in [
            (WsAttachment::player(Role::Black, "b", "g"), vec![0, 2, 4]),
            (WsAttachment::player(Role::White, "w", "g"), vec![1, 2, 4]),
            (WsAttachment::spectator("g").reset_spectator_snapshot(), vec![3, 4]),
            (WsAttachment::Pending, vec![]),
        ] {
            let mut received = Vec::new();
            att.deliver_terminal(
                &entries,
                |line| {
                    received.push(line.to_owned());
                    Ok::<_, ()>(())
                },
                |_| Ok(()),
                |_, _| panic!("close"),
            )
            .unwrap();
            assert_eq!(
                received,
                expected.iter().map(|i| format!("REJECT:{i}")).collect::<Vec<_>>()
            );
        }
        let mut att = WsAttachment::spectator("g");
        // MONITOR2ON 前も snapshot 送信中と同じく保留し、cold start でも維持する。
        att = serde_json::from_str(&serde_json::to_string(&att).unwrap()).unwrap();
        for _ in 0..2 {
            att.deliver_terminal(
                &entries,
                |_| panic!("snapshot 中の送信"),
                |_| -> Result<(), ()> { panic!("snapshot 中の記録") },
                |_, _| panic!("close"),
            )
            .unwrap();
        }
        assert!(!att.terminal_complete(&entries));
        let WsAttachment::Spectator { pending_queue, .. } = att else {
            unreachable!()
        };
        assert!(pending_queue.is_empty());
    }

    #[test]
    fn terminal_move_already_in_snapshot_is_not_sent_again() {
        let mut att = WsAttachment::spectator("game").reset_spectator_snapshot();
        if let WsAttachment::Spectator {
            last_ply_in_snapshot,
            ..
        } = &mut att
        {
            *last_ply_in_snapshot = 12;
        }
        let entries = [FinalizingBroadcast {
            target: rshogi_csa_server::BroadcastTarget::All,
            line: "-5251OU,T3".into(),
            ply: Some(12),
        }];
        let mut saves = 0;
        for _ in 0..2 {
            att.deliver_terminal(
                &entries,
                |_| panic!("snapshot と重複"),
                |_| {
                    saves += 1;
                    Ok::<_, ()>(())
                },
                |_, _| panic!("close"),
            )
            .unwrap();
        }
        assert_eq!(saves, 1);
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
        assert!(s.contains("\"is_admin\":false"));
    }

    #[test]
    fn player_with_admin_marks_attachment_as_admin() {
        let att = WsAttachment::player(Role::Black, "alice", "g").with_admin();
        assert!(att.is_admin());
    }

    #[test]
    fn with_admin_is_noop_for_non_player() {
        // Spectator / Pending は admin 昇格対象外。`with_admin` 呼び出しは値を
        // 変えない (型 invariants として保護する)。
        let pending = WsAttachment::Pending;
        assert_eq!(pending.clone().with_admin(), pending);
        let spec = WsAttachment::spectator("room-1");
        assert_eq!(spec.clone().with_admin(), spec);
    }

    #[test]
    fn player_admin_round_trips_via_json() {
        // is_admin = true な Player が serde 経由で完全復元されること。
        let att = WsAttachment::player(Role::Black, "alice", "g");
        assert!(!att.is_admin());
        let att = att.with_admin();
        let s = serde_json::to_string(&att).unwrap();
        assert!(s.contains("\"is_admin\":true"));
        let back: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, back);
        assert!(back.is_admin());
    }

    #[test]
    fn player_legacy_attachment_defaults_is_admin_false() {
        // 旧 schema (is_admin 導入前) で永続化された Player attachment を
        // deserialize した際、is_admin が default = false で復元されること。
        // Hibernation 復帰時に admin 権限が黙って引き継がれない契約。
        let legacy = r#"{"type":"Player","role":"Black","handle":"alice","game_name":"g"}"#;
        let restored: WsAttachment = serde_json::from_str(legacy).unwrap();
        assert!(!restored.is_admin());
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
            terminal_sent: Vec::new(),
            terminal_in_flight: None,
            terminal_aborted: false,
            pending_queue: vec![
                ("+5756FU,T2".to_owned(), Some(8)),
                ("##[CHAT] alice: hi".to_owned(), None),
            ],
        };
        let s = serde_json::to_string(&att).unwrap();
        let restored: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, restored);
        let pending: WsAttachment = serde_json::from_str(r#"{"type":"Pending"}"#).unwrap();
        assert_eq!(pending, WsAttachment::Pending);
    }

    #[test]
    fn reset_spectator_snapshot_clears_flag_and_queue() {
        // snapshot 送信中 (flag=true, queue 非空) の attachment を reset すると
        // flag=false / last_ply=0 / queue 空になる。room_id は保持する。
        let att = WsAttachment::Spectator {
            room_id: "room-xyz".to_owned(),
            snapshot_in_progress: true,
            last_ply_in_snapshot: 7,
            terminal_sent: Vec::new(),
            terminal_in_flight: None,
            terminal_aborted: false,
            pending_queue: vec![
                ("+5756FU,T2".to_owned(), Some(8)),
                ("##[CHAT] alice: hi".to_owned(), None),
            ],
        };
        assert_eq!(
            att.reset_spectator_snapshot(),
            WsAttachment::Spectator {
                room_id: "room-xyz".to_owned(),
                snapshot_in_progress: false,
                last_ply_in_snapshot: 0,
                pending_queue: Vec::new(),
                terminal_sent: Vec::new(),
                terminal_in_flight: None,
                terminal_aborted: false,
            }
        );
    }

    #[test]
    fn reset_spectator_snapshot_is_noop_for_non_spectator() {
        // Player / Pending は snapshot 状態を持たないので変更されない。
        let player = WsAttachment::player(Role::Black, "alice", "g");
        assert_eq!(player.clone().reset_spectator_snapshot(), player);
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
                ..
            } => {
                assert_eq!(room_id, "room-xyz");
                assert!(!snapshot_in_progress);
                assert_eq!(last_ply_in_snapshot, 0);
                assert!(pending_queue.is_empty());
            }
            other => panic!("expected Spectator, got {other:?}"),
        }
    }
}
