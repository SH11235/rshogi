//! プレイヤ状態機械と最小限の League 実装。
//!
//! 状態遷移・マッチング・ログインのうち最小構成で必要な範囲を扱う。
//! 重複ログイン対応、x1 観戦系、Floodgate 固有ペアリングはこのモジュールに
//! 差分で乗せる形で拡張する想定。

use std::collections::HashMap;

use crate::error::StateError;
use crate::types::{Color, GameId, GameName, PlayerName};

/// 1 セッションを一意に識別する世代カウンタ。
///
/// LOGIN ごとに単調増加する番号を発行し、「League の現在の登録は自分のセッションか」
/// を判定するために使う。同名の旧セッションを `EvictOld` で追い出す際は、新 LOGIN
/// で世代が更新されるため、旧タスクは [`League::logout_if_generation`] で
/// 「自分の世代が現在のものと一致するか」を確認してから logout を実行する。
/// これにより、旧タスクの終了処理が後から走って新セッションを誤って logout して
/// しまう race を防ぐ。
///
/// 別途、旧 `run_waiter` の `select!` を起こして即終了させるための cancel 信号は、
/// TCP frontend 側で `Arc<tokio::sync::Notify>` をプレイヤ名で持つ別マップで管理する
/// （workers ビルド (tokio 非依存) でも League がコンパイルできるよう、League 自身は
/// 同期プリミティブを持たない設計）。
pub type SessionGeneration = u64;

/// プレイヤ状態機械の 6 状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerStatus {
    /// ログイン直後かつマッチ未開始の状態（接続確立済み・認証済み）。
    Connected,
    /// LOGIN 済みで同一 `game_name` の相手待ち。
    GameWaiting {
        /// マッチ対象とする game_name。
        game_name: GameName,
        /// 手番希望（None なら任意）。
        preferred_color: Option<Color>,
    },
    /// Game_Summary 送信後の AGREE 待ち。
    AgreeWaiting {
        /// 確定した対局 ID。
        game_id: GameId,
    },
    /// 片方 AGREE 済み、相手の AGREE 待ち。
    StartWaiting {
        /// 対局 ID。
        game_id: GameId,
    },
    /// 対局中。
    InGame {
        /// 対局 ID。
        game_id: GameId,
    },
    /// 終局（LOGOUT 直前）。
    Finished,
}

/// 直接マッチで成立した 1 対局のプレイヤ割り当て。
///
/// 呼び出し側で先手・後手の取り違えが起きないよう、色ごとに名前を明示した型で返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedPair {
    /// 先手に割り当てられたプレイヤ。
    pub black: PlayerName,
    /// 後手に割り当てられたプレイヤ。
    pub white: PlayerName,
}

/// `PairingLogic` に渡すプレイヤ 1 件分の情報。
///
/// `LeastDiffPairingStrategy` 等のレート差ベース戦略は `rate` と
/// `recent_opponents` も参照する。スケジューラ経路は `RateStorage` /
/// `FloodgateHistoryStorage` から事前取得して埋める想定。直接マッチ戦略では
/// 余分なフィールドは無視され、`Option::None` / 空 `Vec` で構築されても OK。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCandidate {
    /// プレイヤ名。
    pub name: PlayerName,
    /// 手番希望（None なら任意）。
    pub preferred_color: Option<Color>,
    /// 既知のレーティング。`None` の場合は戦略側で既定値（通常 1500）を当てる。
    /// レート差ベース戦略 (`LeastDiffPairingStrategy`) は本フィールドを使う。
    pub rate: Option<i32>,
    /// 直近の対戦相手（連戦回避ペナルティ計算で使う）。`Vec<String>` 内は
    /// 直近 N 試合の対戦相手の handle。スケジューラ経路で履歴ストレージから
    /// 事前取得して埋める。
    pub recent_opponents: Vec<String>,
}

/// `League::login` の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginResult {
    /// 認証成功。`x1` が true なら x1 拡張モードで受け付けた。
    Ok {
        /// x1 拡張モードで受け付けたかどうか。
        x1: bool,
        /// この LOGIN で発行されたセッション世代。`run_waiter` / `drive_game` に
        /// 渡して [`League::logout_if_generation`] と組み合わせて使う。
        generation: SessionGeneration,
    },
    /// 認証失敗（不正なパスワード／未登録プレイヤ）。
    Incorrect,
    /// 同名のプレイヤが既に接続中。
    ///
    /// 重複ログインの解決方針を導入するまでは「新接続拒否」を既定で返す。
    AlreadyLoggedIn,
}

/// League 内部で 1 プレイヤ分の状態を保持するレコード。
///
/// 状態機械 (`PlayerStatus`) と x1 拡張モードフラグを 1 か所に束ねて、ログイン時点で
/// 決まる「このクライアントは `%%` 系を許可されているか」を他 API から同期して
/// 読めるようにする。x1 フラグは LOGIN 受理時点で確定し、以降の状態遷移では変化
/// しない（別接続として再 LOGIN しない限り上書きされない）ため、`transition` では
/// 明示的に保持し続ける。
#[derive(Debug, Clone)]
struct PlayerRecord {
    status: PlayerStatus,
    x1: bool,
    /// 現在のセッションを識別する世代番号。LOGIN ごとに発行され、旧タスクが
    /// `Arc<tokio::sync::Notify>` 等で起こされて後片付けに入ったときに、自分の
    /// 世代と League の最新世代を比較する。同名で再 LOGIN 済みのときは世代が
    /// 一致しないため、旧タスクの logout が新セッションを巻き込まない。
    generation: SessionGeneration,
}

/// ログイン中のプレイヤと状態を保持する League。
///
/// 状態機械 (`PlayerStatus`)、x1 拡張フラグ、マッチ待ちプール候補を一元管理する。
/// マッチング戦略やレート永続化は別モジュール (`matching::pairing` や
/// `port::RateStorage`) に逃がしている。
#[derive(Debug, Default)]
pub struct League {
    players: HashMap<PlayerName, PlayerRecord>,
    /// 次に発行する session generation。LOGIN / evict 時に単調増加する。
    /// 0 はサーバ起動直後の初期値で「まだ何も発行していない」状態。
    next_generation: SessionGeneration,
}

impl League {
    /// 空の League を生成する。
    pub fn new() -> Self {
        Self::default()
    }

    /// プレイヤのログインを試みる。
    ///
    /// パスワードの検証は呼び出し側（[`crate::port::RateStorage`] + PasswordHasher 相当）に任せる。
    /// 本関数は純粋に状態機械として、登録成功時 [`PlayerStatus::Connected`] に遷移させ、
    /// `x1` 拡張モードの許可フラグも併せて記録する。
    pub fn login(&mut self, name: &PlayerName, x1: bool) -> LoginResult {
        if self.players.contains_key(name) {
            return LoginResult::AlreadyLoggedIn;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.players.insert(
            name.clone(),
            PlayerRecord {
                status: PlayerStatus::Connected,
                x1,
                generation,
            },
        );
        LoginResult::Ok { x1, generation }
    }

    /// 現在の同名セッションを追い出して、旧セッションの世代番号を返す。
    ///
    /// `EvictOld` ポリシーで使う原子的な追い出し API。`League` のロックを保持した
    /// 状態で「追い出し → 新 LOGIN」を 1 つの臨界区にまとめるために使う。返値の
    /// 世代は呼び出し側で「旧 cancel notify を引くキー」として参照できる。
    /// 既存セッションが無ければ `None`。
    pub fn evict_session(&mut self, name: &PlayerName) -> Option<SessionGeneration> {
        self.players.remove(name).map(|r| r.generation)
    }

    /// 指定世代のセッションが現在の登録と一致するときだけ logout する。
    ///
    /// 旧 `run_waiter` が `WaiterOutcome::Aborted` 等の終了経路で後始末を走らせた
    /// ときに、既に新 LOGIN が同名で着席している場合に新セッションまで巻き込んで
    /// logout してしまう競合を防ぐ。世代が一致して logout した場合に `true`、
    /// それ以外（別世代に置換済 or 未ログイン）は `false` を返す。
    pub fn logout_if_generation(
        &mut self,
        name: &PlayerName,
        generation: SessionGeneration,
    ) -> bool {
        match self.players.get(name) {
            Some(rec) if rec.generation == generation => {
                self.players.remove(name);
                true
            }
            _ => false,
        }
    }

    /// 指定プレイヤが x1 拡張モードで LOGIN しているかを返す。
    ///
    /// LOGIN 時に `x1` フラグ付きだったプレイヤのみ `%%` 系コマンドを受理できる。
    /// 未ログインのプレイヤは `false` を返す。
    pub fn is_x1(&self, name: &PlayerName) -> bool {
        self.players.get(name).map(|r| r.x1).unwrap_or(false)
    }

    /// プレイヤの状態を遷移させる。
    ///
    /// 最小限の不変条件のみを守る:
    /// - 未ログインのプレイヤに対する遷移は [`StateError::InvalidForState`] で拒否する。
    /// - `Finished` 状態は終端として扱い、他状態への再遷移を拒否する（LOGOUT で除去される想定）。
    ///
    /// 完全な遷移表（`Connected → GameWaiting`、`AgreeWaiting ↔ StartWaiting` 等）は
    /// 対局ルームハンドラ側と突き合わせて詳細化する。
    pub fn transition(
        &mut self,
        name: &PlayerName,
        new_status: PlayerStatus,
    ) -> Result<(), StateError> {
        let Some(slot) = self.players.get_mut(name) else {
            return Err(StateError::InvalidForState {
                current: "<not-logged-in>".to_owned(),
            });
        };
        if matches!(slot.status, PlayerStatus::Finished) {
            return Err(StateError::InvalidForState {
                current: "Finished".to_owned(),
            });
        }
        slot.status = new_status;
        Ok(())
    }

    /// プレイヤの現在状態を参照する。
    pub fn status(&self, name: &PlayerName) -> Option<&PlayerStatus> {
        self.players.get(name).map(|r| &r.status)
    }

    /// 全プレイヤ一覧。`%%WHO` 応答生成等で使う。
    pub fn who(&self) -> Vec<(PlayerName, PlayerStatus)> {
        self.players.iter().map(|(n, r)| (n.clone(), r.status.clone())).collect()
    }

    /// LOGOUT 時にプレイヤをリーグから除去する。
    pub fn logout(&mut self, name: &PlayerName) {
        self.players.remove(name);
    }

    /// マッチ成立を確定する：両対局者を [`PlayerStatus::AgreeWaiting`] に遷移させる。
    ///
    /// `pick_direct_match` や [`crate::matching::pairing::PairingLogic`] が返した
    /// `MatchedPair` を受け、双方の状態を一括して `AgreeWaiting` に進める。
    /// 片方でも未ログイン／既に対局中なら `StateError::InvalidForState` を返し、
    /// 両者の状態を一切変更しない（all-or-nothing 不変条件）。
    ///
    /// - 双方を「合意待ち」へ遷移する。
    /// - 状態機械の単一エントリ。
    pub fn confirm_match(
        &mut self,
        matched: &MatchedPair,
        game_id: GameId,
    ) -> Result<(), StateError> {
        // 1. 両者の現状を確認（GameWaiting で、かつ同一 `game_name` であること）。
        let mut shared_game_name: Option<&GameName> = None;
        for name in [&matched.black, &matched.white] {
            match self.players.get(name).map(|r| &r.status) {
                Some(PlayerStatus::GameWaiting { game_name, .. }) => {
                    if let Some(prev) = shared_game_name {
                        if prev != game_name {
                            // 同一 `game_name` の組でなければ成立させない。
                            return Err(StateError::InvalidForState {
                                current: format!(
                                    "game_name mismatch: {} vs {}",
                                    prev.as_str(),
                                    game_name.as_str()
                                ),
                            });
                        }
                    } else {
                        shared_game_name = Some(game_name);
                    }
                }
                Some(other) => {
                    return Err(StateError::InvalidForState {
                        current: format!("{other:?}"),
                    });
                }
                None => {
                    return Err(StateError::InvalidForState {
                        current: "<not-logged-in>".to_owned(),
                    });
                }
            }
        }
        // 2. 両者を AgreeWaiting に遷移（x1 フラグは維持する）。
        for name in [&matched.black, &matched.white] {
            if let Some(rec) = self.players.get_mut(name) {
                rec.status = PlayerStatus::AgreeWaiting {
                    game_id: game_id.clone(),
                };
            }
        }
        Ok(())
    }

    /// 終局時の後処理：両対局者を [`PlayerStatus::Finished`] に遷移させる。
    ///
    /// `GameRoom` が `HandleOutcome::GameEnded` を返したとき、`League` に対して
    /// 両者の状態を Finished に確定するために呼ぶ。LOGOUT は別途 [`Self::logout`] で
    /// プレイヤエントリ自体を除去する想定（資源解放）。
    pub fn end_game(&mut self, players: &MatchedPair) -> Result<(), StateError> {
        // confirm_match と同様 all-or-nothing：両者揃っているのを確認してから書き換える。
        for name in [&players.black, &players.white] {
            if !self.players.contains_key(name) {
                return Err(StateError::InvalidForState {
                    current: "<not-logged-in>".to_owned(),
                });
            }
        }
        for name in [&players.black, &players.white] {
            if let Some(rec) = self.players.get_mut(name) {
                rec.status = PlayerStatus::Finished;
            }
        }
        Ok(())
    }

    /// 指定の `game_name` で待機中のプレイヤ候補を抽出する（戦略へ渡す材料）。
    ///
    /// 副作用なし。`PairingLogic` 実装が `&League` を受けて呼ぶことを想定している。
    pub fn waiting_candidates(&self, game_name: &GameName) -> Vec<PairingCandidate> {
        let mut v: Vec<_> = self
            .players
            .iter()
            .filter_map(|(n, r)| match &r.status {
                PlayerStatus::GameWaiting {
                    game_name: g,
                    preferred_color,
                } if g == game_name => Some(PairingCandidate {
                    name: n.clone(),
                    preferred_color: *preferred_color,
                    // League は rate / 履歴を保持しない。レート差ベース戦略を使う
                    // 経路では呼び出し側が `RateStorage` / `FloodgateHistoryStorage`
                    // から事前取得して埋める。
                    rate: None,
                    recent_opponents: Vec::new(),
                }),
                _ => None,
            })
            .collect();
        // 決定論性のため名前でソート。
        v.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        v
    }

    /// 同一 `game_name` のマッチ待ちから、相補的な手番（`Black` と `White`）の組を 1 組だけ抽出する。
    ///
    /// - 同一 `game_name` で両者が待機しているときだけ成立。
    /// - 候補が 2 名未満ならマッチを成立させない。
    /// - 手番希望が重複する（同色同士／一方が未指定など）組はマッチを成立させない。
    ///
    /// `PairingLogic` 経由の戦略チェーン版は [`crate::matching::pairing`] を参照。
    pub fn pick_direct_match(&self, game_name: &GameName) -> Option<MatchedPair> {
        // game_name にマッチしているプレイヤ一覧を (name, preferred_color) で取り出す。
        let mut candidates: Vec<(&PlayerName, Option<Color>)> = self
            .players
            .iter()
            .filter_map(|(n, r)| match &r.status {
                PlayerStatus::GameWaiting {
                    game_name: g,
                    preferred_color,
                } if g == game_name => Some((n, *preferred_color)),
                _ => None,
            })
            .collect();

        if candidates.len() < 2 {
            return None;
        }
        // 決定論的な並びにするためプレイヤ名でソート。
        candidates.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));

        // 相補的手番（Black×White または White×Black）のみを採用する。
        // 手番希望が未指定のプレイヤは現状の直接マッチでは対象外。
        for i in 0..candidates.len() {
            for j in (i + 1)..candidates.len() {
                let (ni, ci) = candidates[i];
                let (nj, cj) = candidates[j];
                match (ci, cj) {
                    (Some(Color::Black), Some(Color::White)) => {
                        return Some(MatchedPair {
                            black: ni.clone(),
                            white: nj.clone(),
                        });
                    }
                    (Some(Color::White), Some(Color::Black)) => {
                        return Some(MatchedPair {
                            black: nj.clone(),
                            white: ni.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> PlayerName {
        PlayerName::new(s)
    }

    #[test]
    fn login_and_duplicate() {
        let mut l = League::new();
        assert!(matches!(l.login(&name("alice"), false), LoginResult::Ok { x1: false, .. }));
        assert_eq!(l.login(&name("alice"), false), LoginResult::AlreadyLoggedIn);
        assert!(matches!(l.login(&name("bob"), true), LoginResult::Ok { x1: true, .. }));
    }

    #[test]
    fn login_issues_monotonically_increasing_generations() {
        // EvictOld の世代比較が成立する前提として、世代は LOGIN ごとに単調増加する。
        let mut l = League::new();
        let LoginResult::Ok { generation: g0, .. } = l.login(&name("alice"), false) else {
            panic!("login must succeed");
        };
        l.evict_session(&name("alice"));
        let LoginResult::Ok { generation: g1, .. } = l.login(&name("alice"), false) else {
            panic!("re-login must succeed");
        };
        assert!(g1 > g0, "generation must increase after evict + re-login");
    }

    #[test]
    fn evict_session_returns_old_generation_and_clears_entry() {
        let mut l = League::new();
        let LoginResult::Ok {
            generation: old, ..
        } = l.login(&name("alice"), false)
        else {
            panic!("login must succeed");
        };
        let returned = l.evict_session(&name("alice")).expect("must return old generation");
        assert_eq!(old, returned);
        assert!(l.status(&name("alice")).is_none());
        // 未ログインプレイヤを evict しても None。
        assert!(l.evict_session(&name("ghost")).is_none());
    }

    #[test]
    fn logout_if_generation_protects_new_session_after_evict() {
        // 旧 run_waiter が終了経路で logout を走らせたときに、既に新 LOGIN が
        // 同名で着席していれば新セッションを巻き込まずに no-op になることを固定する。
        let mut l = League::new();
        let LoginResult::Ok {
            generation: old, ..
        } = l.login(&name("alice"), false)
        else {
            panic!("login must succeed");
        };
        l.evict_session(&name("alice"));
        let LoginResult::Ok {
            generation: new_, ..
        } = l.login(&name("alice"), false)
        else {
            panic!("re-login must succeed");
        };
        // 旧世代での logout は no-op。新セッションは残る。
        assert!(!l.logout_if_generation(&name("alice"), old));
        assert!(l.status(&name("alice")).is_some());
        // 新世代なら logout 成立。
        assert!(l.logout_if_generation(&name("alice"), new_));
        assert!(l.status(&name("alice")).is_none());
        // 未ログイン状態への呼び出しも no-op。
        assert!(!l.logout_if_generation(&name("alice"), new_));
    }

    #[test]
    fn is_x1_reflects_login_flag() {
        let mut l = League::new();
        assert!(!l.is_x1(&name("ghost")));
        l.login(&name("alice"), false);
        l.login(&name("bob"), true);
        assert!(!l.is_x1(&name("alice")));
        assert!(l.is_x1(&name("bob")));
    }

    #[test]
    fn x1_flag_survives_state_transitions_and_match_confirmation() {
        // x1 は LOGIN 時に確定する属性で、状態機械の遷移では消えない。
        let mut l = League::new();
        l.login(&name("alice"), true);
        l.login(&name("bob"), true);
        for n in ["alice", "bob"] {
            l.transition(
                &name(n),
                PlayerStatus::GameWaiting {
                    game_name: GameName::new("g1"),
                    preferred_color: if n == "alice" {
                        Some(Color::Black)
                    } else {
                        Some(Color::White)
                    },
                },
            )
            .unwrap();
        }
        let pair = MatchedPair {
            black: name("alice"),
            white: name("bob"),
        };
        l.confirm_match(&pair, GameId::new("g1")).unwrap();
        assert!(l.is_x1(&name("alice")));
        assert!(l.is_x1(&name("bob")));
        l.end_game(&pair).unwrap();
        assert!(l.is_x1(&name("alice")));
        assert!(l.is_x1(&name("bob")));
        l.logout(&name("alice"));
        assert!(!l.is_x1(&name("alice")));
    }

    #[test]
    fn transition_requires_login() {
        let mut l = League::new();
        let err = l
            .transition(
                &name("ghost"),
                PlayerStatus::InGame {
                    game_id: GameId::new("g1"),
                },
            )
            .unwrap_err();
        assert!(matches!(err, StateError::InvalidForState { .. }));
    }

    #[test]
    fn transition_rejects_from_finished_terminal() {
        let mut l = League::new();
        l.login(&name("alice"), false);
        l.transition(&name("alice"), PlayerStatus::Finished).unwrap();
        let err = l
            .transition(
                &name("alice"),
                PlayerStatus::InGame {
                    game_id: GameId::new("g1"),
                },
            )
            .unwrap_err();
        assert!(matches!(err, StateError::InvalidForState { .. }));
    }

    #[test]
    fn transition_updates_state() {
        let mut l = League::new();
        l.login(&name("alice"), false);
        l.transition(
            &name("alice"),
            PlayerStatus::GameWaiting {
                game_name: GameName::new("floodgate-600-10"),
                preferred_color: Some(Color::Black),
            },
        )
        .unwrap();
        match l.status(&name("alice")) {
            Some(PlayerStatus::GameWaiting { .. }) => (),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn pick_direct_match_none_if_fewer_than_two() {
        let mut l = League::new();
        l.login(&name("alice"), false);
        l.transition(
            &name("alice"),
            PlayerStatus::GameWaiting {
                game_name: GameName::new("g1"),
                preferred_color: Some(Color::Black),
            },
        )
        .unwrap();
        assert_eq!(l.pick_direct_match(&GameName::new("g1")), None);
    }

    #[test]
    fn pick_direct_match_rejects_same_color_pair() {
        let mut l = League::new();
        for n in ["alice", "bob"] {
            l.login(&name(n), false);
            l.transition(
                &name(n),
                PlayerStatus::GameWaiting {
                    game_name: GameName::new("g1"),
                    preferred_color: Some(Color::Black),
                },
            )
            .unwrap();
        }
        // 同色希望どうしはマッチを成立させない。
        assert_eq!(l.pick_direct_match(&GameName::new("g1")), None);
    }

    #[test]
    fn pick_direct_match_rejects_when_preferred_color_absent() {
        let mut l = League::new();
        for (n, c) in [("alice", None), ("bob", Some(Color::Black))] {
            l.login(&name(n), false);
            l.transition(
                &name(n),
                PlayerStatus::GameWaiting {
                    game_name: GameName::new("g1"),
                    preferred_color: c,
                },
            )
            .unwrap();
        }
        // 手番希望未指定のプレイヤが混ざる組は直接マッチでは成立させない。
        assert_eq!(l.pick_direct_match(&GameName::new("g1")), None);
    }

    #[test]
    fn pick_direct_match_prefers_complementary_colors() {
        let mut l = League::new();
        for (n, c) in [
            ("alice", Some(Color::Black)),
            ("bob", Some(Color::Black)),
            ("carol", Some(Color::White)),
        ] {
            l.login(&name(n), false);
            l.transition(
                &name(n),
                PlayerStatus::GameWaiting {
                    game_name: GameName::new("g1"),
                    preferred_color: c,
                },
            )
            .unwrap();
        }
        // alice(Black) と carol(White) が相補的で選ばれる。
        let pair = l.pick_direct_match(&GameName::new("g1")).unwrap();
        assert_eq!(pair.black.as_str(), "alice");
        assert_eq!(pair.white.as_str(), "carol");
    }

    fn make_pair(b: &str, w: &str) -> MatchedPair {
        MatchedPair {
            black: name(b),
            white: name(w),
        }
    }

    fn login_and_wait(l: &mut League, who: &str, color: Color) {
        l.login(&name(who), false);
        l.transition(
            &name(who),
            PlayerStatus::GameWaiting {
                game_name: GameName::new("g1"),
                preferred_color: Some(color),
            },
        )
        .unwrap();
    }

    #[test]
    fn confirm_match_transitions_both_to_agree_waiting() {
        let mut l = League::new();
        login_and_wait(&mut l, "alice", Color::Black);
        login_and_wait(&mut l, "bob", Color::White);
        let pair = make_pair("alice", "bob");
        l.confirm_match(&pair, GameId::new("g1-001")).unwrap();
        for n in ["alice", "bob"] {
            match l.status(&name(n)) {
                Some(PlayerStatus::AgreeWaiting { game_id }) => {
                    assert_eq!(game_id.as_str(), "g1-001");
                }
                other => panic!("unexpected status for {n}: {other:?}"),
            }
        }
    }

    #[test]
    fn confirm_match_rejects_when_one_side_not_waiting() {
        let mut l = League::new();
        // alice は GameWaiting だが bob は Connected のまま。
        login_and_wait(&mut l, "alice", Color::Black);
        l.login(&name("bob"), false);
        let pair = make_pair("alice", "bob");
        let err = l.confirm_match(&pair, GameId::new("g1")).unwrap_err();
        assert!(matches!(err, StateError::InvalidForState { .. }));
        // 失敗時は alice の状態も変えていない（all-or-nothing）。
        assert!(matches!(l.status(&name("alice")), Some(PlayerStatus::GameWaiting { .. })));
    }

    #[test]
    fn confirm_match_rejects_when_player_missing() {
        let mut l = League::new();
        login_and_wait(&mut l, "alice", Color::Black);
        let pair = make_pair("alice", "ghost");
        let err = l.confirm_match(&pair, GameId::new("g1")).unwrap_err();
        assert!(matches!(err, StateError::InvalidForState { .. }));
        assert!(matches!(l.status(&name("alice")), Some(PlayerStatus::GameWaiting { .. })));
    }

    #[test]
    fn end_game_marks_both_finished() {
        let mut l = League::new();
        login_and_wait(&mut l, "alice", Color::Black);
        login_and_wait(&mut l, "bob", Color::White);
        let pair = make_pair("alice", "bob");
        l.confirm_match(&pair, GameId::new("g1")).unwrap();
        l.end_game(&pair).unwrap();
        for n in ["alice", "bob"] {
            assert!(matches!(l.status(&name(n)), Some(PlayerStatus::Finished)));
        }
    }

    #[test]
    fn end_game_rejects_unknown_player() {
        let mut l = League::new();
        login_and_wait(&mut l, "alice", Color::Black);
        let pair = make_pair("alice", "ghost");
        let err = l.end_game(&pair).unwrap_err();
        assert!(matches!(err, StateError::InvalidForState { .. }));
    }

    #[test]
    fn end_game_does_not_partially_update_when_other_player_missing() {
        // 片方が既に logout 済み（players から消えた）場合、もう片方も書き換えないこと。
        let mut l = League::new();
        login_and_wait(&mut l, "alice", Color::Black);
        login_and_wait(&mut l, "bob", Color::White);
        let pair = make_pair("alice", "bob");
        l.confirm_match(&pair, GameId::new("g1")).unwrap();
        l.logout(&name("bob"));
        // alice の状態は AgreeWaiting のまま、bob は欠落 → end_game は失敗。
        let err = l.end_game(&pair).unwrap_err();
        assert!(matches!(err, StateError::InvalidForState { .. }));
        // alice は Finished に進んでいない（部分書き換えなし）。
        assert!(matches!(l.status(&name("alice")), Some(PlayerStatus::AgreeWaiting { .. })));
    }

    #[test]
    fn confirm_match_rejects_pair_with_different_game_names() {
        // 異なる game_name で待機している 2 人を誤って渡しても、
        // confirm_match が拒否すること。
        let mut l = League::new();
        l.login(&name("alice"), false);
        l.transition(
            &name("alice"),
            PlayerStatus::GameWaiting {
                game_name: GameName::new("g1"),
                preferred_color: Some(Color::Black),
            },
        )
        .unwrap();
        l.login(&name("bob"), false);
        l.transition(
            &name("bob"),
            PlayerStatus::GameWaiting {
                game_name: GameName::new("g2"),
                preferred_color: Some(Color::White),
            },
        )
        .unwrap();
        let pair = make_pair("alice", "bob");
        let err = l.confirm_match(&pair, GameId::new("xx")).unwrap_err();
        assert!(matches!(err, StateError::InvalidForState { .. }));
        // 両者の状態は変わっていない。
        assert!(matches!(l.status(&name("alice")), Some(PlayerStatus::GameWaiting { .. })));
        assert!(matches!(l.status(&name("bob")), Some(PlayerStatus::GameWaiting { .. })));
    }

    #[test]
    fn disconnect_during_play_routes_through_end_game() {
        // 対局中の切断 → 異常終了 → 両者を Finished へ。
        let mut l = League::new();
        login_and_wait(&mut l, "alice", Color::Black);
        login_and_wait(&mut l, "bob", Color::White);
        let pair = make_pair("alice", "bob");
        l.confirm_match(&pair, GameId::new("g1")).unwrap();
        // 双方 AGREE 完了の代わりに、両者を直接 InGame に進めて対局中状態を作る。
        for n in ["alice", "bob"] {
            l.transition(
                &name(n),
                PlayerStatus::InGame {
                    game_id: GameId::new("g1"),
                },
            )
            .unwrap();
        }
        // この時点で alice が切断したと仮定。GameRoom 側は force_abnormal を呼ぶ。
        // League はそれと同期して end_game を呼ぶ責務。
        l.end_game(&pair).unwrap();
        for n in ["alice", "bob"] {
            assert!(matches!(l.status(&name(n)), Some(PlayerStatus::Finished)));
        }
        // LOGOUT までで完全にエントリ削除。
        l.logout(&name("alice"));
        l.logout(&name("bob"));
        assert!(l.status(&name("alice")).is_none());
        assert!(l.status(&name("bob")).is_none());
    }

    #[test]
    fn waiting_candidates_returns_only_matching_game_name() {
        let mut l = League::new();
        login_and_wait(&mut l, "alice", Color::Black);
        l.login(&name("carol"), false);
        l.transition(
            &name("carol"),
            PlayerStatus::GameWaiting {
                game_name: GameName::new("other"),
                preferred_color: Some(Color::White),
            },
        )
        .unwrap();
        let candidates = l.waiting_candidates(&GameName::new("g1"));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name.as_str(), "alice");
    }
}
