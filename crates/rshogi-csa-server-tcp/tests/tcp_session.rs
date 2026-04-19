//! CSA セッション End-to-End テスト（Requirement 15.1）。
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
use rshogi_csa_server::FileKifuStorage;
use rshogi_csa_server::port::PlayerRateRecord;
use rshogi_csa_server::types::PlayerName;
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
        total_time_sec: 60,
        byoyomi_sec: 10,
        time_margin_ms: 1_500,
        max_moves: 256,
        login_timeout: Duration::from_secs(10),
        agree_timeout: Duration::from_secs(30),
        entering_king_rule: EnteringKingRule::Point24,
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
        total_time_sec: 60,
        byoyomi_sec: 10,
        time_margin_ms: 1_500,
        max_moves: 256,
        login_timeout: Duration::from_secs(10),
        agree_timeout,
        entering_king_rule: EnteringKingRule::Point24,
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
