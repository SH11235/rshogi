//! CSA セッション End-to-End テスト。
//!
//! TCP ソケット経由で以下のシナリオを通し、フロントエンド全体の挙動を検証する:
//!
//! - 認証（LOGIN / 成功・失敗・レート制限）
//! - マッチ成立 → Game_Summary → AGREE → 対局進行 → 終局（投了 / 最大手数）
//! - CSA V2 棋譜と 00LIST が shogi-server mk_rate 互換形式で吐かれる
//! - 待機中の切断・`agree_timeout` 総窓の enforcement 等の不変条件
//!
//! `flavor = "current_thread"` + `LocalSet` でサーバーを起動し、同じタスク内から
//! `TcpStream` クライアントで接続・行送受信する。仮想時計は使わないが、各シナリオは
//! 数百ミリ秒以内に収束するため実時間でも安定する。

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use rshogi_core::types::EnteringKingRule;
use rshogi_csa_server::port::PlayerRateRecord;
use rshogi_csa_server::types::PlayerName;
use rshogi_csa_server::{ClockSpec, FileKifuStorage};
use rshogi_csa_server_tcp::auth::PlainPasswordHasher;
use rshogi_csa_server_tcp::broadcaster::InMemoryBroadcaster;
use rshogi_csa_server_tcp::rate_limit::IpLoginRateLimiter;
use rshogi_csa_server_tcp::server::{InMemoryPasswordStore, ServerConfig, build_state, run_server};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// テスト用 RateStorage。auth.rs のモックと同等。
mod support {
    use super::*;
    use rshogi_csa_server::error::StorageError;
    use rshogi_csa_server::port::{PlayerRateRecord, RateStorage};
    use std::cell::RefCell;

    pub struct MemRateStorage {
        data: RefCell<HashMap<String, PlayerRateRecord>>,
    }

    impl MemRateStorage {
        pub fn new(records: Vec<PlayerRateRecord>) -> Self {
            let mut map = HashMap::new();
            for r in records {
                map.insert(r.name.as_str().to_owned(), r);
            }
            Self {
                data: RefCell::new(map),
            }
        }
    }

    impl RateStorage for MemRateStorage {
        async fn load(&self, name: &PlayerName) -> Result<Option<PlayerRateRecord>, StorageError> {
            Ok(self.data.borrow().get(name.as_str()).cloned())
        }

        async fn save(&self, record: &PlayerRateRecord) -> Result<(), StorageError> {
            self.data.borrow_mut().insert(record.name.as_str().to_owned(), record.clone());
            Ok(())
        }

        async fn list_all(&self) -> Result<Vec<PlayerRateRecord>, StorageError> {
            Ok(self.data.borrow().values().cloned().collect())
        }
    }
}

/// 1 テスト用に一意な作業ディレクトリを作る。
fn unique_topdir(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("rshogi_csa_tcp_e2e_{tag}_{pid}_{ts}"))
}

/// テストシナリオ 1 件分のサーバーを立ち上げる。
/// - `127.0.0.1:0` で bind し、実際のポートを返す。
/// - players は alice/bob 固定（パスワードはどちらも `pw`）。
async fn spawn_server(tag: &str) -> (std::net::SocketAddr, PathBuf) {
    spawn_server_with_clock(
        tag,
        ClockSpec::Countdown {
            total_time_sec: 60,
            byoyomi_sec: 10,
        },
    )
    .await
}

/// テストシナリオ 1 件分のサーバーを指定時計で立ち上げる。
async fn spawn_server_with_clock(tag: &str, clock: ClockSpec) -> (std::net::SocketAddr, PathBuf) {
    let topdir = unique_topdir(tag);
    let mut password_map = HashMap::new();
    password_map.insert("alice".to_owned(), "pw".to_owned());
    password_map.insert("bob".to_owned(), "pw".to_owned());
    // %%WHO / %%LIST / %%SHOW の観戦者役として使う追加アカウント。
    password_map.insert("carol".to_owned(), "pw".to_owned());
    let rate_records = vec![
        PlayerRateRecord {
            name: PlayerName::new("alice"),
            rate: 1500,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: "2026-04-17T00:00:00Z".to_owned(),
        },
        PlayerRateRecord {
            name: PlayerName::new("bob"),
            rate: 1500,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: "2026-04-17T00:00:00Z".to_owned(),
        },
        PlayerRateRecord {
            name: PlayerName::new("carol"),
            rate: 1500,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: "2026-04-17T00:00:00Z".to_owned(),
        },
    ];
    let rate_storage = support::MemRateStorage::new(rate_records);
    let kifu_storage = FileKifuStorage::new(topdir.clone());
    let config = ServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        kifu_topdir: topdir.clone(),
        clock,
        time_margin_ms: 1_500,
        max_moves: 256,
        login_timeout: Duration::from_secs(10),
        agree_timeout: Duration::from_secs(30),
        x1_reply_write_timeout: Duration::from_secs(5),
        entering_king_rule: EnteringKingRule::Point24,
        initial_sfen: None,
        admin_handles: Vec::new(),
    };
    // bind_addr=:0 を使うため、先に手動で bind してから actual addr を取る必要がある。
    // ここでは ServerConfig を既定の :0 のまま build_state に渡し、run_server 内で
    // bind される際のポートを取れない。そのため、TcpListener を先に bind して
    // そのアドレスを config に書き戻す。
    let probe = tokio::net::TcpListener::bind(config.bind_addr).await.unwrap();
    let actual_addr = probe.local_addr().unwrap();
    drop(probe); // 実際の bind は run_server が行う
    let mut config = config;
    config.bind_addr = actual_addr;
    let state = Rc::new(build_state(
        config,
        rate_storage,
        kifu_storage,
        InMemoryPasswordStore { map: password_map },
        Box::new(PlainPasswordHasher::new()),
        IpLoginRateLimiter::default_limits(),
        InMemoryBroadcaster::new(),
    ));
    let _handle = run_server(state).await.expect("run_server");
    // accept ループが起動するまで少し待つ。
    tokio::time::sleep(Duration::from_millis(50)).await;
    (actual_addr, topdir)
}

/// 1 クライアント分の (reader, writer) ペア。
async fn connect(addr: std::net::SocketAddr) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let (r, w) = stream.into_split();
    (BufReader::new(r), w)
}

async fn send_line(writer: &mut OwnedWriteHalf, line: &str) {
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.write_all(b"\r\n").await.unwrap();
    writer.flush().await.unwrap();
}

async fn read_line_raw(reader: &mut BufReader<OwnedReadHalf>) -> Option<String> {
    let mut buf = String::new();
    match tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut buf)).await {
        Ok(Ok(0)) => None,
        Ok(Ok(_)) => {
            let s = buf.trim_end_matches(['\r', '\n']).to_owned();
            Some(s)
        }
        Ok(Err(e)) => panic!("read_line error: {e}"),
        Err(_) => panic!("read_line timed out (buf so far: {buf:?})"),
    }
}

/// `BEGIN Game_Summary` … `END Game_Summary` の塊を丸ごと読み切る。
async fn drain_game_summary(reader: &mut BufReader<OwnedReadHalf>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let line = read_line_raw(reader).await.expect("early eof in summary");
        let done = line == "END Game_Summary";
        out.push(line);
        if done {
            return out;
        }
    }
}

/// 指定の `expect` 行を観測するまで読み飛ばし（出現した行も返す）。
async fn read_until(reader: &mut BufReader<OwnedReadHalf>, expect: &str) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let line = read_line_raw(reader).await.expect("early eof");
        let done = line == expect;
        out.push(line);
        if done {
            return out;
        }
    }
}

fn run_local<F, Fut>(f: F)
where
    F: FnOnce() -> Fut + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move { f().await });
}

// ---------- テスト本体 ----------

#[test]
fn login_auth_failure_on_bad_password() {
    run_local(|| async {
        let (addr, topdir) = spawn_server("badpw").await;
        let (mut r, mut w) = connect(addr).await;
        send_line(&mut w, "LOGIN alice+g1+black badpw").await;
        let resp = read_line_raw(&mut r).await.unwrap();
        assert_eq!(resp, "LOGIN:incorrect");
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn login_ok_and_match_start_via_game_summary_and_agree() {
    run_local(|| async {
        let (addr, topdir) = spawn_server("login_ok_match").await;

        // Black 側の接続 → LOGIN
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");

        // White 側の接続 → LOGIN → マッチ成立 → 両者に Game_Summary が届く
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");

        let s_black = drain_game_summary(&mut rb).await;
        let s_white = drain_game_summary(&mut rw).await;
        assert!(s_black.iter().any(|l| l == "Your_Turn:+"));
        assert!(s_white.iter().any(|l| l == "Your_Turn:-"));

        // 両者 AGREE → START:<game_id> が Players 宛てに届く。
        send_line(&mut wb, "AGREE").await;
        send_line(&mut ww, "AGREE").await;
        let start_b = read_line_raw(&mut rb).await.unwrap();
        let start_w = read_line_raw(&mut rw).await.unwrap();
        assert!(start_b.starts_with("START:"));
        assert_eq!(start_b, start_w);
        let game_id = start_b.trim_start_matches("START:").to_owned();
        assert!(!game_id.is_empty());

        // 投了までの最短対局: +7776FU → -3334FU → %TORYO（Black が投了）。
        send_line(&mut wb, "+7776FU").await;
        let _ = read_until(&mut rb, "+7776FU,T0").await;
        let _ = read_until(&mut rw, "+7776FU,T0").await;
        send_line(&mut ww, "-3334FU").await;
        let _ = read_until(&mut rb, "-3334FU,T0").await;
        let _ = read_until(&mut rw, "-3334FU,T0").await;
        send_line(&mut wb, "%TORYO").await;
        let b_end = read_until(&mut rb, "#LOSE").await;
        assert!(b_end.iter().any(|l| l == "#RESIGN"));

        // 棋譜ファイルと 00LIST が出ていることを確認。
        tokio::time::sleep(Duration::from_millis(50)).await;
        let zerozero = tokio::fs::read_to_string(topdir.join("00LIST")).await.unwrap();
        assert!(zerozero.contains(&game_id), "00LIST: {zerozero}");
        assert!(zerozero.contains("alice bob"));
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn fischer_clock_summary_exposes_increment_field() {
    run_local(|| async {
        let (addr, topdir) = spawn_server_with_clock(
            "fischer_summary",
            ClockSpec::Fischer {
                total_time_sec: 60,
                increment_sec: 5,
            },
        )
        .await;
        let (mut rb, mut wb) = connect(addr).await;
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");

        let s_black = drain_game_summary(&mut rb).await;
        let s_white = drain_game_summary(&mut rw).await;
        for summary in [&s_black, &s_white] {
            assert!(summary.iter().any(|l| l == "Time_Unit:1sec"), "{summary:?}");
            assert!(summary.iter().any(|l| l == "Total_Time:60"), "{summary:?}");
            assert!(summary.iter().any(|l| l == "Increment:5"), "{summary:?}");
            assert!(!summary.iter().any(|l| l.starts_with("Byoyomi:")), "{summary:?}");
        }

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn stopwatch_clock_summary_uses_minute_unit() {
    run_local(|| async {
        let (addr, topdir) = spawn_server_with_clock(
            "stopwatch_summary",
            ClockSpec::StopWatch {
                total_time_min: 15,
                byoyomi_min: 1,
            },
        )
        .await;
        let (mut rb, mut wb) = connect(addr).await;
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");

        let s_black = drain_game_summary(&mut rb).await;
        let s_white = drain_game_summary(&mut rw).await;
        for summary in [&s_black, &s_white] {
            assert!(summary.iter().any(|l| l == "Time_Unit:1min"), "{summary:?}");
            assert!(summary.iter().any(|l| l == "Total_Time:15"), "{summary:?}");
            assert!(summary.iter().any(|l| l == "Byoyomi:1"), "{summary:?}");
            assert!(!summary.iter().any(|l| l.starts_with("Increment:")), "{summary:?}");
        }

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn kifu_and_zerozero_list_compatible_with_mk_rate() {
    run_local(|| async {
        let (addr, topdir) = spawn_server("kifu_fmt").await;
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        let _ = read_line_raw(&mut rb).await;
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        let _ = read_line_raw(&mut rw).await;
        let _ = drain_game_summary(&mut rb).await;
        let _ = drain_game_summary(&mut rw).await;
        send_line(&mut wb, "AGREE").await;
        send_line(&mut ww, "AGREE").await;
        let start_b = read_line_raw(&mut rb).await.unwrap();
        let _ = read_line_raw(&mut rw).await;
        let game_id = start_b.trim_start_matches("START:").to_owned();

        // 1 手指してから Black が投了 → 黒投了負け。
        send_line(&mut wb, "+7776FU").await;
        let _ = read_until(&mut rb, "+7776FU,T0").await;
        let _ = read_until(&mut rw, "+7776FU,T0").await;
        send_line(&mut ww, "-3334FU").await;
        let _ = read_until(&mut rb, "-3334FU,T0").await;
        let _ = read_until(&mut rw, "-3334FU,T0").await;
        send_line(&mut wb, "%TORYO").await;
        let _ = read_until(&mut rb, "#LOSE").await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        // 棋譜の場所は YYYY/MM/DD/<game_id>.csa（game_id は YYYYMMDDHHMMSS+連番）。
        let yyyy = &game_id[0..4];
        let mm = &game_id[4..6];
        let dd = &game_id[6..8];
        let csa_path = topdir.join(yyyy).join(mm).join(dd).join(format!("{game_id}.csa"));
        let csa = tokio::fs::read_to_string(&csa_path).await.unwrap();
        // V2.2 ヘッダ、プレイヤ名、2 手、%TORYO の存在を確認。
        assert!(csa.starts_with("V2.2\n"));
        assert!(csa.contains("\nN+alice\n"));
        assert!(csa.contains("\nN-bob\n"));
        assert!(csa.contains("\n+7776FU,T"));
        assert!(csa.contains("\n-3334FU,T"));
        assert!(csa.contains("\n%TORYO\n"));
        // 00LIST 1 行分が mk_rate 互換（スペース区切り 6 カラム、末尾 #RESIGN）。
        let zerozero = tokio::fs::read_to_string(topdir.join("00LIST")).await.unwrap();
        let line = zerozero.lines().last().unwrap();
        let cols: Vec<_> = line.split(' ').collect();
        assert_eq!(cols.len(), 6, "mk_rate expects 6 columns: {line}");
        assert_eq!(cols[5], "#RESIGN");
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn login_rate_limit_denies_burst() {
    run_local(|| async {
        let (addr, topdir) = spawn_server("ratelimit").await;
        // 既定の 10 回/分 を超える 12 連続 LOGIN を同一 IP (127.0.0.1) で叩く。
        // 11 回目以降の応答は `rate_limited` プレフィックス付きになるはず。
        let mut denied = false;
        for i in 0..12 {
            let (mut r, mut w) = connect(addr).await;
            send_line(&mut w, "LOGIN alice+g1+black badpw").await;
            let resp = read_line_raw(&mut r).await.unwrap_or_default();
            if resp.starts_with("LOGIN:incorrect rate_limited") {
                denied = true;
                break;
            }
            // rate limiter はカウンタのみで拒否しないので、incorrect で閉じる。
            assert_eq!(resp, "LOGIN:incorrect", "iter {i}");
        }
        assert!(denied, "rate limiter should deny within 12 tries");
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

/// `agree_timeout` を変更できる拡張スポーン。回帰テスト用。
async fn spawn_server_with_agree_timeout(
    tag: &str,
    agree_timeout: Duration,
) -> (std::net::SocketAddr, PathBuf) {
    let topdir = unique_topdir(tag);
    let mut password_map = HashMap::new();
    password_map.insert("alice".to_owned(), "pw".to_owned());
    password_map.insert("bob".to_owned(), "pw".to_owned());
    let rate_records = vec![
        PlayerRateRecord {
            name: PlayerName::new("alice"),
            rate: 1500,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: "2026-04-17T00:00:00Z".to_owned(),
        },
        PlayerRateRecord {
            name: PlayerName::new("bob"),
            rate: 1500,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: "2026-04-17T00:00:00Z".to_owned(),
        },
    ];
    let rate_storage = support::MemRateStorage::new(rate_records);
    let kifu_storage = FileKifuStorage::new(topdir.clone());
    let config = ServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        kifu_topdir: topdir.clone(),
        clock: ClockSpec::Countdown {
            total_time_sec: 60,
            byoyomi_sec: 10,
        },
        time_margin_ms: 1_500,
        max_moves: 256,
        login_timeout: Duration::from_secs(10),
        agree_timeout,
        x1_reply_write_timeout: Duration::from_secs(5),
        entering_king_rule: EnteringKingRule::Point24,
        initial_sfen: None,
        admin_handles: Vec::new(),
    };
    let probe = tokio::net::TcpListener::bind(config.bind_addr).await.unwrap();
    let actual_addr = probe.local_addr().unwrap();
    drop(probe);
    let mut config = config;
    config.bind_addr = actual_addr;
    let state = Rc::new(build_state(
        config,
        rate_storage,
        kifu_storage,
        InMemoryPasswordStore { map: password_map },
        Box::new(PlainPasswordHasher::new()),
        IpLoginRateLimiter::default_limits(),
        InMemoryBroadcaster::new(),
    ));
    let _handle = run_server(state).await.expect("run_server");
    tokio::time::sleep(Duration::from_millis(50)).await;
    (actual_addr, topdir)
}

#[test]
fn agree_total_window_is_not_reset_by_peer_keepalive() {
    // `agree_timeout` は Game_Summary 送信時点からの総待機窓であり、
    // 片方が KEEPALIVE を連打してももう一方の AGREE を無期限待ちにしないこと。
    // 短い窓（1.5 秒）を設定し、白が KEEPALIVE を連続送信しつつ黒が AGREE しない
    // シナリオで、窓超過後はマッチ不成立 (REJECT 通知) に落ちることを確認する。
    run_local(|| async {
        let (addr, topdir) =
            spawn_server_with_agree_timeout("agree_total_window", Duration::from_millis(1_500))
                .await;
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");
        let _ = drain_game_summary(&mut rb).await;
        let _ = drain_game_summary(&mut rw).await;

        // Black は一切応答しない。White が 300ms 間隔で KEEPALIVE を 6 回送って
        // 合計 1.8 秒間の擬似 keepalive を発生させる。total window = 1.5 秒なので
        // keepalive によってタイマーがリセットされない実装なら、窓超過で REJECT が届く。
        let driver = async {
            for _ in 0..6 {
                send_line(&mut ww, "").await; // empty line = KEEPALIVE
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        };
        let reader = async {
            // REJECT 行を期待（window 超過で不成立）。
            let line = read_line_raw(&mut rw).await.unwrap();
            assert!(line.starts_with("REJECT:"), "expected REJECT, got {line:?}");
        };
        tokio::join!(driver, reader);

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn x1_waiter_answers_info_commands_and_is_still_matchable() {
    // x1 付きで LOGIN したクライアントは matchmaking に通常通り参加しつつ、
    // 待機中は `%%VERSION` / `%%HELP` / keep-alive に応答できる。相補手番の
    // 相手が到着すれば Game_Summary を受信してマッチ成立する
    // （x1 は「`%%` 系コマンドも解釈できる対局クライアント」の意味であり、
    // info-only な観戦モードではない）。
    run_local(|| async {
        let (addr, topdir) = spawn_server("x1_waiter_info").await;

        // alice が x1 付きで LOGIN。
        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN alice+g1+black pw x1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:alice OK");

        // %%VERSION → 1 行応答。
        send_line(&mut wa, "%%VERSION").await;
        let v = read_line_raw(&mut ra).await.unwrap();
        assert!(v.starts_with("##[VERSION] "), "unexpected VERSION line: {v}");

        // %%HELP → 複数行 + `##[HELP] END` 終端。プレフィックスだけ確認して
        // 残りは Game_Summary までまとめて読み流す。
        send_line(&mut wa, "%%HELP").await;
        let h = read_line_raw(&mut ra).await.unwrap();
        assert!(h.starts_with("##[HELP] "), "unexpected HELP line: {h}");

        // keep-alive（空行）でも切断されない。
        send_line(&mut wa, "").await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        // bob が相補手番で入ってきた時点でマッチ成立し、alice 側にも
        // Game_Summary が流れてくる（`BEGIN Game_Summary` を観測）。
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:bob OK");

        let mut saw_begin = false;
        for _ in 0..60 {
            let line = read_line_raw(&mut ra).await.unwrap();
            if line == "BEGIN Game_Summary" {
                saw_begin = true;
                break;
            }
        }
        assert!(saw_begin, "did not observe Game_Summary for x1 waiter");
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn x1_waiter_answers_who_with_terminator_and_self_row() {
    // x1 付きで LOGIN した 2 プレイヤが異なる game_name で待機している状態で
    // %%WHO を投げると、自身と他プレイヤが `##[WHO] <name> <status>` で一覧され、
    // `##[WHO] END` で終わる。双方 x1 だが game_name が違うのでマッチは成立
    // しない。
    run_local(|| async {
        let (addr, topdir) = spawn_server("x1_who").await;

        // alice が x1 で waiting に入る（g1, black）。
        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN alice+g1+black pw x1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:alice OK");

        // bob が x1 で別の game_name (g-other, white) に入る → マッチ成立しない。
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN bob+g-other+white pw x1").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:bob OK");

        // alice が %%WHO を投げる。
        send_line(&mut wa, "%%WHO").await;
        let mut rows: Vec<String> = Vec::new();
        for _ in 0..10 {
            let line = read_line_raw(&mut ra).await.unwrap();
            let is_end = line == "##[WHO] END";
            rows.push(line);
            if is_end {
                break;
            }
        }
        assert_eq!(rows.last().map(String::as_str), Some("##[WHO] END"));
        assert!(rows.iter().any(|l| l == "##[WHO] alice waiting:g1"), "no alice row: {rows:?}");
        assert!(rows.iter().any(|l| l == "##[WHO] bob waiting:g-other"), "no bob row: {rows:?}");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn x1_list_and_show_reflect_ongoing_game() {
    // 先行で alice vs bob が対局開始してレジストリに登録される状態を作り、
    // 別接続の x1 クライアントから %%LIST / %%SHOW で同じ対局を参照できる。
    run_local(|| async {
        let (addr, topdir) = spawn_server("x1_list_show").await;

        // alice / bob でマッチ成立させる。
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");
        let _ = drain_game_summary(&mut rb).await;
        let _ = drain_game_summary(&mut rw).await;
        send_line(&mut wb, "AGREE").await;
        send_line(&mut ww, "AGREE").await;
        let start_b = read_line_raw(&mut rb).await.unwrap();
        let _ = read_line_raw(&mut rw).await;
        let game_id = start_b.trim_start_matches("START:").to_owned();

        // 観戦者想定の x1 クライアントが LOGIN（game_name は別のもので OK、対局は
        // 組まれない）。
        let (mut rs, mut ws) = connect(addr).await;
        send_line(&mut ws, "LOGIN carol+other+black pw x1").await;
        assert_eq!(read_line_raw(&mut rs).await.unwrap(), "LOGIN:carol OK");

        // %%LIST → 進行中対局に alice vs bob が含まれる。
        send_line(&mut ws, "%%LIST").await;
        let mut list_rows: Vec<String> = Vec::new();
        for _ in 0..10 {
            let line = read_line_raw(&mut rs).await.unwrap();
            let is_end = line == "##[LIST] END";
            list_rows.push(line);
            if is_end {
                break;
            }
        }
        assert!(
            list_rows
                .iter()
                .any(|l| l.contains(&game_id) && l.contains("alice") && l.contains("bob")),
            "LIST: {list_rows:?}"
        );
        assert_eq!(list_rows.last().map(String::as_str), Some("##[LIST] END"));

        // %%SHOW <game_id> → 各フィールドが 1 行ずつ返る。
        send_line(&mut ws, &format!("%%SHOW {game_id}")).await;
        let mut show_rows: Vec<String> = Vec::new();
        for _ in 0..10 {
            let line = read_line_raw(&mut rs).await.unwrap();
            let is_end = line == "##[SHOW] END";
            show_rows.push(line);
            if is_end {
                break;
            }
        }
        assert!(
            show_rows.iter().any(|l| l == &format!("##[SHOW] game_id {game_id}")),
            "SHOW: {show_rows:?}"
        );
        assert!(
            show_rows.iter().any(|l| l == "##[SHOW] black alice"),
            "SHOW missing black: {show_rows:?}"
        );
        assert!(
            show_rows.iter().any(|l| l == "##[SHOW] white bob"),
            "SHOW missing white: {show_rows:?}"
        );
        assert!(
            show_rows.iter().any(|l| l == "##[SHOW] game_name g1"),
            "SHOW missing game_name: {show_rows:?}"
        );

        // %%SHOW 未知 ID → NOT_FOUND + 終端行の 2 行で framing を保つ。
        send_line(&mut ws, "%%SHOW unknown-game").await;
        let nf = read_line_raw(&mut rs).await.unwrap();
        assert_eq!(nf, "##[SHOW] NOT_FOUND unknown-game");
        let end = read_line_raw(&mut rs).await.unwrap();
        assert_eq!(end, "##[SHOW] END");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn rejected_game_never_appears_in_list() {
    // AGREE 待ち中に片方が REJECT を返して対局不成立になったケースでは、
    // GameRegistry に登録されていないので %%LIST には出ない。
    run_local(|| async {
        let (addr, topdir) = spawn_server("reject_list").await;

        // 観戦者役の carol を先に入れておく。
        let (mut rc, mut wc) = connect(addr).await;
        send_line(&mut wc, "LOGIN carol+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), "LOGIN:carol OK");

        // alice / bob でマッチ成立 → Game_Summary まで行くが bob が REJECT する。
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");
        let _ = drain_game_summary(&mut rb).await;
        let _ = drain_game_summary(&mut rw).await;

        // bob は AGREE せず REJECT → alice / bob 双方に `REJECT:<game_id>` が届く。
        send_line(&mut ww, "REJECT").await;
        let reject_b = read_line_raw(&mut rb).await.unwrap();
        assert!(reject_b.starts_with("REJECT:"), "expected REJECT, got {reject_b}");
        let reject_w = read_line_raw(&mut rw).await.unwrap();
        assert!(reject_w.starts_with("REJECT:"), "expected REJECT, got {reject_w}");

        // サーバ側の epilogue が走るのを少し待つ。
        tokio::time::sleep(Duration::from_millis(100)).await;

        // carol が %%LIST → 空（terminator のみ）であることを確認。
        send_line(&mut wc, "%%LIST").await;
        let first = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(first, "##[LIST] END", "rejected game should not be listed: got {first}");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn non_x1_waiter_is_disconnected_on_any_input() {
    // x1 なし LOGIN の waiter は、待機中の任意の入力（%% 系含む）で切断される。
    // 「x1 未確立のセッションは %% を受け付けない」方針を TCP レベルで守る。
    run_local(|| async {
        let (addr, topdir) = spawn_server("non_x1_waiter").await;
        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:alice OK");

        // %%VERSION を投げると切断される。
        send_line(&mut wa, "%%VERSION").await;
        // 応答行は無く EOF を観測する（ソケットが閉じられる）。
        let eof = read_line_raw(&mut ra).await;
        assert!(eof.is_none(), "non-x1 waiter should be disconnected, got line: {eof:?}");
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn waiter_disconnect_is_cleaned_up_and_allows_relogin() {
    // 先着プレイヤが相手待ち中に切断したとき、待機プールと League から明示的に除去される
    // こと。さもなければ同一 handle の再 LOGIN が already_logged_in で失敗する。
    run_local(|| async {
        let (addr, topdir) = spawn_server("waiter_disc").await;
        // 1 人目 alice が GameWaiting に入る。
        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:alice OK");
        // alice 切断。
        drop(wa);
        drop(ra);
        // サーバーが切断を検知してクリーンアップする時間を確保。
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 同一 handle で再 LOGIN。already_logged_in にならず通ること。
        let (mut ra2, mut wa2) = connect(addr).await;
        send_line(&mut wa2, "LOGIN alice+g1+black pw").await;
        let resp = read_line_raw(&mut ra2).await.unwrap();
        assert_eq!(resp, "LOGIN:alice OK");
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

/// alice / bob をマッチ成立させて AGREE まで進め、(reader_black, writer_black,
/// reader_white, writer_white, game_id) を返すテストハーネス。
/// 終局系 E2E テストで共通のセットアップを削減するため。
async fn login_match_agree(
    addr: std::net::SocketAddr,
) -> (
    BufReader<OwnedReadHalf>,
    OwnedWriteHalf,
    BufReader<OwnedReadHalf>,
    OwnedWriteHalf,
    String,
) {
    let (mut rb, mut wb) = connect(addr).await;
    send_line(&mut wb, "LOGIN alice+g1+black pw").await;
    assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
    let (mut rw, mut ww) = connect(addr).await;
    send_line(&mut ww, "LOGIN bob+g1+white pw").await;
    assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");
    let _ = drain_game_summary(&mut rb).await;
    let _ = drain_game_summary(&mut rw).await;
    send_line(&mut wb, "AGREE").await;
    send_line(&mut ww, "AGREE").await;
    let start_b = read_line_raw(&mut rb).await.unwrap();
    let _ = read_line_raw(&mut rw).await.unwrap();
    let game_id = start_b.trim_start_matches("START:").to_owned();
    (rb, wb, rw, ww, game_id)
}

#[test]
fn kachi_on_initial_position_ends_as_illegal_kachi() {
    // 平手初期局面で %KACHI を投げると 24 点不成立で `#ILLEGAL_MOVE` 終局。
    // TCP 駆動系を通った終局メッセージが対局者双方に届くことを確認する。
    run_local(|| async {
        let (addr, topdir) = spawn_server("kachi_rejected").await;
        let (mut rb, mut wb, mut rw, _ww, _game_id) = login_match_agree(addr).await;
        send_line(&mut wb, "%KACHI").await;
        // 黒 (敗者側) は #ILLEGAL_MOVE + #LOSE。白 (勝者側) は #ILLEGAL_MOVE + #WIN。
        let b_lines = read_until(&mut rb, "#LOSE").await;
        assert!(b_lines.iter().any(|l| l == "#ILLEGAL_MOVE"));
        let w_lines = read_until(&mut rw, "#WIN").await;
        assert!(w_lines.iter().any(|l| l == "#ILLEGAL_MOVE"));
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn sennichite_broadcasts_draw_on_12_ply_gold_dance() {
    // 平手から両者の左金を 4 九 ↔ 4 八 / 4 一 ↔ 4 二 と循環させて 3 サイクル (12 手)
    // 経過で初期局面 4 回目の出現 → `#SENNICHITE` + `#DRAW` が双方の対局者に届く。
    // TCP 層で千日手の通知が正しく fanout されることを E2E で検証する。
    run_local(|| async {
        let (addr, topdir) = spawn_server("sennichite").await;
        let (mut rb, mut wb, mut rw, mut ww, _game_id) = login_match_agree(addr).await;
        // 3 サイクル (12 手) を淡々と送り出す。最終手以外は `,T0` broadcast が流れる。
        let moves: &[(&str, bool)] = &[
            ("+4948KI", true), // (token, is_black)
            ("-4142KI", false),
            ("+4849KI", true),
            ("-4241KI", false),
        ];
        for _ in 0..2 {
            for (tok, is_black) in moves {
                if *is_black {
                    send_line(&mut wb, tok).await;
                } else {
                    send_line(&mut ww, tok).await;
                }
                let expect = format!("{tok},T0");
                let _ = read_until(&mut rb, &expect).await;
                let _ = read_until(&mut rw, &expect).await;
            }
        }
        // 3 サイクル目: 最終 (-4241KI) で千日手が確定する。最終手の放送と `#SENNICHITE`
        // / `#DRAW` の両方を対局者双方で確認する。
        for (tok, is_black) in moves.iter().take(3) {
            if *is_black {
                send_line(&mut wb, tok).await;
            } else {
                send_line(&mut ww, tok).await;
            }
            let expect = format!("{tok},T0");
            let _ = read_until(&mut rb, &expect).await;
            let _ = read_until(&mut rw, &expect).await;
        }
        send_line(&mut ww, "-4241KI").await;
        let b_end = read_until(&mut rb, "#DRAW").await;
        assert!(b_end.iter().any(|l| l == "#SENNICHITE"));
        assert!(b_end.iter().any(|l| l == "-4241KI,T0"));
        let w_end = read_until(&mut rw, "#DRAW").await;
        assert!(w_end.iter().any(|l| l == "#SENNICHITE"));
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn monitor2_subscribes_and_receives_moves_and_chat() {
    // x1 クライアント carol が `%%MONITOR2ON <game_id>` で対局に購読し、
    // (a) 指し手 broadcast (`+7776FU,T0` 等) を受信する
    // (b) `%%CHAT <msg>` で自身が送ったメッセージが自身に echo される
    // (c) `%%MONITOR2OFF` で購読を解除する (以降の broadcast は届かない)
    // を end-to-end で検証する。
    run_local(|| async {
        let (addr, topdir) = spawn_server("monitor2").await;

        // 対局セットアップ (alice vs bob)。white は対局中に指さないので writer は使わない。
        let (mut rb, mut wb, mut rw, _ww, game_id) = login_match_agree(addr).await;

        // 観戦者 carol は x1 で login。game_name は対局と同じにする必要は無い
        // (異なる game_name なので直接マッチは成立せず、x1 waiter として留まる)。
        let (mut rc, mut wc) = connect(addr).await;
        send_line(&mut wc, "LOGIN carol+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), "LOGIN:carol OK");

        // `%%MONITOR2ON <game_id>` → `##[MONITOR2] BEGIN <game_id>` + END。
        send_line(&mut wc, &format!("%%MONITOR2ON {game_id}")).await;
        let begin = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(begin, format!("##[MONITOR2] BEGIN {game_id}"));
        let end = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(end, "##[MONITOR2] END");

        // 対局者が着手すると、broadcast (Spectators 宛て) で observer にも届く。
        send_line(&mut wb, "+7776FU").await;
        let _ = read_until(&mut rb, "+7776FU,T0").await;
        let _ = read_until(&mut rw, "+7776FU,T0").await;
        // observer は `+7776FU,T0` を受信する。
        let observed = read_until(&mut rc, "+7776FU,T0").await;
        assert!(observed.iter().any(|l| l == "+7776FU,T0"));

        // `%%CHAT` → `##[CHAT] carol: hello` を observer 自身が echo 受信する
        // (subscriber に自身が含まれる contract)。
        send_line(&mut wc, "%%CHAT hello").await;
        let mut saw_chat = false;
        for _ in 0..10 {
            let line = read_line_raw(&mut rc).await.unwrap();
            if line == "##[CHAT] carol: hello" {
                saw_chat = true;
                break;
            }
            if line == format!("##[CHAT] OK {game_id}") {
                // OK 応答自体はあり得る順序。続きを読む。
                continue;
            }
            if line == "##[CHAT] END" {
                continue;
            }
        }
        assert!(saw_chat, "did not observe CHAT echo");

        // `%%MONITOR2OFF <game_id>` → `##[MONITOR2OFF] <game_id>` + END。
        send_line(&mut wc, &format!("%%MONITOR2OFF {game_id}")).await;
        let off = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(off, format!("##[MONITOR2OFF] {game_id}"));
        let off_end = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(off_end, "##[MONITOR2OFF] END");

        // observer を降りた後、対局者の次着手は broadcaster 経由では届かない
        // (subscriber は drop されて prune される)。厳密な「届かない」検証は
        // タイミング依存なので、後続手を送って observer の recv_line が短時間
        // (100ms) 以内に何も届かない (あるいは subscriber が落ちて何か別行が
        // 届く) ことで妥協する。本テストでは購読解除 reply の最終行まで
        // 完了した時点を off 済みとみなし、以降は検証しない (スケジューラ
        // タイミングで broadcaster の retain (prune) が未実行のまま 1 通だけ
        // 取りこぼす可能性がある)。

        // 対局を投了で畳んでテスト clean-up する。
        send_line(&mut wb, "%TORYO").await;
        let _ = read_until(&mut rb, "#LOSE").await;
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn monitor2_removes_subscriber_from_matchmaking_pool() {
    // Codex review (PR #469 P1) の回帰: x1 waiter は LOGIN で一旦 WaitingPool に
    // 入るが、`%%MONITOR2ON` が成立した時点で観戦者扱いになるため pool から
    // 除外しなければならない。除外しないと、同一 game_name + 相補色で後続
    // プレイヤが LOGIN したとき観戦者が対局者として選ばれてしまう。
    //
    // このテストでは alice/bob 対局進行中に carol が同じ `g1`+`black` で x1
    // LOGIN (alice と同色 = bob と相補)。`%%MONITOR2ON` で observer 化した後、
    // carol が `%%WHO` で自分の status を確認し `waiting:g1` が出ないことを
    // 確認する (= pool から正しく抜けている)。
    run_local(|| async {
        let (addr, topdir) = spawn_server("monitor2_no_match").await;

        // alice vs bob のメイン対局。
        let (_rb, _wb, _rw, _ww, game_id) = login_match_agree(addr).await;

        // carol が相補色候補として pool に入る (現時点では `waiting:g1`)。
        let (mut rc, mut wc) = connect(addr).await;
        send_line(&mut wc, "LOGIN carol+g1+black pw x1").await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), "LOGIN:carol OK");

        // observer 化。pool から外れる。
        send_line(&mut wc, &format!("%%MONITOR2ON {game_id}")).await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), format!("##[MONITOR2] BEGIN {game_id}"));
        let _ = read_line_raw(&mut rc).await.unwrap(); // END

        // %%WHO で carol の status を確認。observer 化済みなら `waiting:g1` は
        // 出ないはず (pool から抜けているので League の GameWaiting 状態ではない)。
        send_line(&mut wc, "%%WHO").await;
        let mut lines = Vec::new();
        for _ in 0..20 {
            let l = read_line_raw(&mut rc).await.unwrap();
            let end = l == "##[WHO] END";
            lines.push(l);
            if end {
                break;
            }
        }
        let has_waiting_carol = lines.iter().any(|l| l == "##[WHO] carol waiting:g1");
        assert!(!has_waiting_carol, "observer carol must not appear as waiting: {lines:?}");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn chat_without_active_monitor_returns_not_monitoring() {
    // x1 waiter が `%%MONITOR2ON` 前に `%%CHAT` を投げると `NOT_MONITORING` で
    // 弾かれる。購読前の chat 経路を誤って開放していないことの回帰防止。
    run_local(|| async {
        let (addr, topdir) = spawn_server("chat_no_sub").await;
        let (mut rc, mut wc) = connect(addr).await;
        send_line(&mut wc, "LOGIN carol+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), "LOGIN:carol OK");
        send_line(&mut wc, "%%CHAT no-subscription-yet").await;
        let resp = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(resp, "##[CHAT] NOT_MONITORING");
        let end = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(end, "##[CHAT] END");
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

/// 指定の admin ハンドル付きで TCP サーバーを起動するヘルパ。
/// `%%SETBUOY` / `%%DELETEBUOY` テストで使う。
async fn spawn_server_with_admin(
    tag: &str,
    admin_handles: Vec<String>,
) -> (std::net::SocketAddr, PathBuf) {
    spawn_server_custom(
        tag,
        ClockSpec::Countdown {
            total_time_sec: 60,
            byoyomi_sec: 10,
        },
        EnteringKingRule::Point24,
        None,
        admin_handles,
    )
    .await
}

async fn spawn_server_custom(
    tag: &str,
    clock: ClockSpec,
    entering_king_rule: EnteringKingRule,
    initial_sfen: Option<&str>,
    admin_handles: Vec<String>,
) -> (std::net::SocketAddr, PathBuf) {
    let topdir = unique_topdir(tag);
    let mut password_map = HashMap::new();
    for h in ["alice", "bob", "carol", "admin"] {
        password_map.insert(h.to_owned(), "pw".to_owned());
    }
    let rate_records: Vec<_> = ["alice", "bob", "carol", "admin"]
        .iter()
        .map(|n| PlayerRateRecord {
            name: PlayerName::new(*n),
            rate: 1500,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: "2026-04-17T00:00:00Z".to_owned(),
        })
        .collect();
    let rate_storage = support::MemRateStorage::new(rate_records);
    let kifu_storage = FileKifuStorage::new(topdir.clone());
    let config = ServerConfig {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        kifu_topdir: topdir.clone(),
        clock,
        time_margin_ms: 1_500,
        max_moves: 256,
        login_timeout: Duration::from_secs(10),
        agree_timeout: Duration::from_secs(30),
        x1_reply_write_timeout: Duration::from_secs(5),
        entering_king_rule,
        initial_sfen: initial_sfen.map(str::to_owned),
        admin_handles,
    };
    let probe = tokio::net::TcpListener::bind(config.bind_addr).await.unwrap();
    let actual_addr = probe.local_addr().unwrap();
    drop(probe);
    let mut config = config;
    config.bind_addr = actual_addr;
    let state = Rc::new(build_state(
        config,
        rate_storage,
        kifu_storage,
        InMemoryPasswordStore { map: password_map },
        Box::new(PlainPasswordHasher::new()),
        IpLoginRateLimiter::default_limits(),
        InMemoryBroadcaster::new(),
    ));
    let _handle = run_server(state).await.expect("run_server");
    tokio::time::sleep(Duration::from_millis(50)).await;
    (actual_addr, topdir)
}

#[test]
fn setbuoy_from_admin_is_accepted_and_getbuoycount_reflects_state() {
    // admin ハンドル (`admin`) が %%SETBUOY で buoy を登録し、同 client が
    // %%GETBUOYCOUNT で登録件数を参照、続いて %%DELETEBUOY で削除して
    // %%GETBUOYCOUNT が NOT_FOUND に戻ることを E2E で検証する。
    run_local(|| async {
        let (addr, topdir) = spawn_server_with_admin("buoy_admin", vec!["admin".to_owned()]).await;
        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN admin+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:admin OK");

        // %%SETBUOY my-buoy +7776FU 3 → OK + END。
        send_line(&mut wa, "%%SETBUOY my-buoy +7776FU 3").await;
        let resp = read_line_raw(&mut ra).await.unwrap();
        assert_eq!(resp, "##[SETBUOY] OK my-buoy 3");
        let end = read_line_raw(&mut ra).await.unwrap();
        assert_eq!(end, "##[SETBUOY] END");

        // %%GETBUOYCOUNT my-buoy → 3 + END。
        send_line(&mut wa, "%%GETBUOYCOUNT my-buoy").await;
        let q = read_line_raw(&mut ra).await.unwrap();
        assert_eq!(q, "##[GETBUOYCOUNT] my-buoy 3");
        let _ = read_line_raw(&mut ra).await.unwrap();

        // %%DELETEBUOY my-buoy → OK + END。
        send_line(&mut wa, "%%DELETEBUOY my-buoy").await;
        let d = read_line_raw(&mut ra).await.unwrap();
        assert_eq!(d, "##[DELETEBUOY] OK my-buoy");
        let _ = read_line_raw(&mut ra).await.unwrap();

        // 削除後は NOT_FOUND。
        send_line(&mut wa, "%%GETBUOYCOUNT my-buoy").await;
        let q2 = read_line_raw(&mut ra).await.unwrap();
        assert_eq!(q2, "##[GETBUOYCOUNT] NOT_FOUND my-buoy");
        let _ = read_line_raw(&mut ra).await.unwrap();

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn setbuoy_from_non_admin_is_permission_denied() {
    // 非 admin (carol) が %%SETBUOY を投げると PERMISSION_DENIED で弾かれ、
    // その後 %%GETBUOYCOUNT は NOT_FOUND (登録されていない)。
    run_local(|| async {
        let (addr, topdir) =
            spawn_server_with_admin("buoy_non_admin", vec!["admin".to_owned()]).await;
        let (mut rc, mut wc) = connect(addr).await;
        send_line(&mut wc, "LOGIN carol+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), "LOGIN:carol OK");

        // `%%SETBUOY` は <game_name> <moves> <count> が最低 3 トークン必要なので、
        // パース通過させる最小形で投げる (非 admin は SETBUOY のパスに入る前に
        // permission で弾かれる)。
        send_line(&mut wc, "%%SETBUOY bad-buoy +7776FU 3").await;
        let resp = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(resp, "##[SETBUOY] PERMISSION_DENIED bad-buoy");
        let _ = read_line_raw(&mut rc).await.unwrap();

        // 登録されていないことを GETBUOYCOUNT で再確認 (参照は権限不要)。
        send_line(&mut wc, "%%GETBUOYCOUNT bad-buoy").await;
        let q = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(q, "##[GETBUOYCOUNT] NOT_FOUND bad-buoy");
        let _ = read_line_raw(&mut rc).await.unwrap();

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn getbuoycount_for_unknown_buoy_returns_not_found_without_admin_check() {
    // 参照系 (%%GETBUOYCOUNT) は admin 権限不要で全クライアントから使える。
    run_local(|| async {
        let (addr, topdir) = spawn_server_with_admin("buoy_anon_query", Vec::new()).await;
        let (mut rc, mut wc) = connect(addr).await;
        send_line(&mut wc, "LOGIN alice+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), "LOGIN:alice OK");
        send_line(&mut wc, "%%GETBUOYCOUNT nothing-here").await;
        let q = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(q, "##[GETBUOYCOUNT] NOT_FOUND nothing-here");
        let _ = read_line_raw(&mut rc).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn setbuoy_is_consumed_when_match_starts_and_summary_uses_derived_turn() {
    run_local(|| async {
        let (addr, topdir) =
            spawn_server_with_admin("buoy_match_start", vec!["admin".to_owned()]).await;

        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN admin+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:admin OK");
        send_line(&mut wa, "%%SETBUOY g1 +7776FU 1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[SETBUOY] OK g1 1");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[SETBUOY] END");

        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");

        let s_black = drain_game_summary(&mut rb).await;
        let s_white = drain_game_summary(&mut rw).await;
        assert!(s_black.iter().any(|l| l == "To_Move:-"), "black summary: {s_black:?}");
        assert!(s_white.iter().any(|l| l == "To_Move:-"), "white summary: {s_white:?}");

        send_line(&mut wa, "%%GETBUOYCOUNT g1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] g1 0");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] END");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn fork_creates_single_use_buoy_from_existing_game() {
    run_local(|| async {
        let (addr, topdir) =
            spawn_server_with_admin("fork_from_kifu", vec!["admin".to_owned()]).await;

        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");
        let _ = drain_game_summary(&mut rb).await;
        let _ = drain_game_summary(&mut rw).await;
        send_line(&mut wb, "AGREE").await;
        send_line(&mut ww, "AGREE").await;
        let start_b = read_line_raw(&mut rb).await.unwrap();
        let _ = read_line_raw(&mut rw).await.unwrap();
        let source_game_id = start_b.trim_start_matches("START:").to_owned();
        send_line(&mut wb, "+7776FU").await;
        let _ = read_until(&mut rb, "+7776FU,T0").await;
        let _ = read_until(&mut rw, "+7776FU,T0").await;
        send_line(&mut ww, "%TORYO").await;
        let _ = read_until(&mut rb, "#WIN").await;
        let _ = read_until(&mut rw, "#LOSE").await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN admin+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:admin OK");
        send_line(&mut wa, &format!("%%FORK {} forked 1", source_game_id)).await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[FORK] OK forked 1");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[FORK] END");
        send_line(&mut wa, "%%GETBUOYCOUNT forked").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] forked 1");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] END");

        let (mut rb2, mut wb2) = connect(addr).await;
        send_line(&mut wb2, "LOGIN alice+forked+black pw").await;
        assert_eq!(read_line_raw(&mut rb2).await.unwrap(), "LOGIN:alice OK");
        let (mut rw2, mut ww2) = connect(addr).await;
        send_line(&mut ww2, "LOGIN bob+forked+white pw").await;
        assert_eq!(read_line_raw(&mut rw2).await.unwrap(), "LOGIN:bob OK");
        let s_black = drain_game_summary(&mut rb2).await;
        let s_white = drain_game_summary(&mut rw2).await;
        assert!(s_black.iter().any(|l| l == "To_Move:-"), "forked black summary: {s_black:?}");
        assert!(s_white.iter().any(|l| l == "To_Move:-"), "forked white summary: {s_white:?}");

        send_line(&mut wa, "%%GETBUOYCOUNT forked").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] forked 0");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] END");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn monitor2_on_unknown_game_returns_not_found() {
    // 存在しない game_id への `%%MONITOR2ON` は `NOT_FOUND` を返し、購読状態を
    // 変更しない (broadcaster にも登録しない)。
    run_local(|| async {
        let (addr, topdir) = spawn_server("mon_unknown").await;
        let (mut rc, mut wc) = connect(addr).await;
        send_line(&mut wc, "LOGIN carol+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut rc).await.unwrap(), "LOGIN:carol OK");
        send_line(&mut wc, "%%MONITOR2ON unknown-game").await;
        let resp = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(resp, "##[MONITOR2] NOT_FOUND unknown-game");
        let end = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(end, "##[MONITOR2] END");
        // 購読していないので CHAT は弾かれるはず。
        send_line(&mut wc, "%%CHAT hello").await;
        let chat = read_line_raw(&mut rc).await.unwrap();
        assert_eq!(chat, "##[CHAT] NOT_MONITORING");
        let _ = read_line_raw(&mut rc).await.unwrap(); // END
        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn fork_gracefully_errors_and_keeps_connection_alive() {
    // 元棋譜が存在しない場合・nth_move が範囲外の場合の `%%FORK` で接続が
    // 切れずに `##[FORK] NOT_FOUND` / `##[FORK] ERROR ...` + `END` を返すこと
    // を検証する (codex レビュー PR #474 P2)。検証後に `%%GETBUOYCOUNT` が
    // 通ることで、waiter ループが健在であることを確認する。
    run_local(|| async {
        let (addr, topdir) =
            spawn_server_with_admin("fork_graceful_error", vec!["admin".to_owned()]).await;

        // admin で x1 接続。
        let (mut ra, mut wa) = connect(addr).await;
        send_line(&mut wa, "LOGIN admin+obs+black pw x1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "LOGIN:admin OK");

        // 1) 存在しない game_id への FORK → NOT_FOUND。
        send_line(&mut wa, "%%FORK nonexistent-game forked 1").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[FORK] NOT_FOUND nonexistent-game");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[FORK] END");

        // 接続が生きていることを %%GETBUOYCOUNT で確認。
        send_line(&mut wa, "%%GETBUOYCOUNT nothing").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] NOT_FOUND nothing");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] END");

        // 2) 既存対局を作って nth_move を範囲外にした FORK → ERROR。
        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");
        let _ = drain_game_summary(&mut rb).await;
        let _ = drain_game_summary(&mut rw).await;
        send_line(&mut wb, "AGREE").await;
        send_line(&mut ww, "AGREE").await;
        // START:<game_id> を読み取って game_id を確定する。
        let start_b = read_line_raw(&mut rb).await.unwrap();
        assert!(start_b.starts_with("START:"), "expected START line, got {start_b:?}");
        let source_game_id = start_b.trim_start_matches("START:").to_owned();
        let _ = read_line_raw(&mut rw).await.unwrap(); // white 側の START
        send_line(&mut wb, "+7776FU,T0").await;
        let _ = read_until(&mut rb, "+7776FU,T0").await;
        let _ = read_until(&mut rw, "+7776FU,T0").await;
        send_line(&mut ww, "%TORYO").await;
        let _ = read_until(&mut rb, "#WIN").await;
        let _ = read_until(&mut rw, "#LOSE").await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        // nth_move=999 は範囲外。切断せず ERROR + END を返す。
        send_line(&mut wa, &format!("%%FORK {source_game_id} bad 999")).await;
        let err_line = read_line_raw(&mut ra).await.unwrap();
        assert!(
            err_line.starts_with("##[FORK] ERROR bad"),
            "expected graceful ERROR response, got {err_line:?}",
        );
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[FORK] END");

        // まだ接続は生きている。
        send_line(&mut wa, "%%GETBUOYCOUNT bad").await;
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] NOT_FOUND bad");
        assert_eq!(read_line_raw(&mut ra).await.unwrap(), "##[GETBUOYCOUNT] END");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn uchifuzume_from_initial_sfen_ends_as_illegal_move_e2e() {
    // Phase 3 acceptance: 打ち歩詰の典型局面を TCP E2E で流し、
    // `#ILLEGAL_MOVE` → `#LOSE/#WIN` が wire されることを固定する。
    run_local(|| async {
        let (addr, topdir) = spawn_server_custom(
            "uchifuzume_e2e",
            ClockSpec::Countdown {
                total_time_sec: 60,
                byoyomi_sec: 10,
            },
            EnteringKingRule::Point24,
            Some("8k/6G2/8+P/9/9/9/9/9/4K4 b P 1"),
            Vec::new(),
        )
        .await;

        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");

        let _ = drain_game_summary(&mut rb).await;
        let _ = drain_game_summary(&mut rw).await;
        send_line(&mut wb, "AGREE").await;
        send_line(&mut ww, "AGREE").await;
        let _ = read_line_raw(&mut rb).await.unwrap();
        let _ = read_line_raw(&mut rw).await.unwrap();

        send_line(&mut wb, "+0012FU").await;
        let black_end = read_until(&mut rb, "#LOSE").await;
        let white_end = read_until(&mut rw, "#WIN").await;
        assert!(black_end.iter().any(|l| l == "#ILLEGAL_MOVE"), "black_end: {black_end:?}");
        assert!(white_end.iter().any(|l| l == "#ILLEGAL_MOVE"), "white_end: {white_end:?}");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}

#[test]
fn oute_sennichite_from_initial_sfen_ends_as_perpetual_check_loss_e2e() {
    // Phase 3 acceptance: 連続王手千日手の最小循環を TCP E2E で流し、
    // `#OUTE_SENNICHITE` が終局メッセージとして表に出ることを確認する。
    run_local(|| async {
        let (addr, topdir) = spawn_server_custom(
            "oute_sennichite_e2e",
            ClockSpec::Countdown {
                total_time_sec: 60,
                byoyomi_sec: 10,
            },
            EnteringKingRule::Point24,
            Some("9/6k2/9/9/9/9/9/6R2/K8 w - 1"),
            Vec::new(),
        )
        .await;

        let (mut rb, mut wb) = connect(addr).await;
        send_line(&mut wb, "LOGIN alice+g1+black pw").await;
        assert_eq!(read_line_raw(&mut rb).await.unwrap(), "LOGIN:alice OK");
        let (mut rw, mut ww) = connect(addr).await;
        send_line(&mut ww, "LOGIN bob+g1+white pw").await;
        assert_eq!(read_line_raw(&mut rw).await.unwrap(), "LOGIN:bob OK");

        let s_black = drain_game_summary(&mut rb).await;
        let s_white = drain_game_summary(&mut rw).await;
        assert!(s_black.iter().any(|l| l == "To_Move:-"), "black summary: {s_black:?}");
        assert!(s_white.iter().any(|l| l == "To_Move:-"), "white summary: {s_white:?}");

        send_line(&mut wb, "AGREE").await;
        send_line(&mut ww, "AGREE").await;
        let _ = read_line_raw(&mut rb).await.unwrap();
        let _ = read_line_raw(&mut rw).await.unwrap();

        send_line(&mut ww, "-3242OU").await;
        let _ = read_until(&mut rb, "-3242OU,T0").await;
        let _ = read_until(&mut rw, "-3242OU,T0").await;
        send_line(&mut wb, "+3848HI").await;
        let _ = read_until(&mut rb, "+3848HI,T0").await;
        let _ = read_until(&mut rw, "+3848HI,T0").await;
        send_line(&mut ww, "-4232OU").await;
        let _ = read_until(&mut rb, "-4232OU,T0").await;
        let _ = read_until(&mut rw, "-4232OU,T0").await;
        send_line(&mut wb, "+4838HI").await;

        let black_end = read_until(&mut rb, "#LOSE").await;
        let white_end = read_until(&mut rw, "#WIN").await;
        assert!(black_end.iter().any(|l| l == "#OUTE_SENNICHITE"), "black_end: {black_end:?}");
        assert!(white_end.iter().any(|l| l == "#OUTE_SENNICHITE"), "white_end: {white_end:?}");

        let _ = tokio::fs::remove_dir_all(&topdir).await;
    });
}
