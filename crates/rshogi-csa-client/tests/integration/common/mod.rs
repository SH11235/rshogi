//! mock USI engine (bash script) を使う integration test 共通のヘルパ。
//!
//! `cargo test` の並列実行下では、あるテストが script の write fd を開いている間に
//! 別テストの `UsiEngine::spawn` が fork すると、子プロセスが write fd を継承したまま
//! exec 前の窓に入り、その script の exec が `ETXTBSY` (Text file busy) で落ちる
//! (`O_CLOEXEC` は fork では閉じず exec 時にしか閉じない)。script 書き込みと fork を
//! [`FORK_WRITE_LOCK`] で直列化することで「write fd open 中の fork」を排除する。

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rshogi_csa_client::engine::{SpawnOptions, UsiEngine};

static SCRIPT_SEQ: AtomicU64 = AtomicU64::new(0);
/// 書き込みと fork の片方だけを直列化しても ETXTBSY レースが残るため、
/// [`write_mock_script`] と [`spawn_engine`] の両方が同じ lock を取る。
static FORK_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 与えた bash script を 0o755 の実行可能ファイルとして一時ディレクトリに書き出し、
/// path を返す。test ごとに unique な名前を付与する。
///
/// tmp path へ書き込み → close → chmod → atomic rename の順にすることで、自スレッドの
/// write fd が exec と重なる経路を塞ぐ。並行 test の fork に対しては
/// [`FORK_WRITE_LOCK`] が守る (module doc 参照)。
pub fn write_mock_script(name: &str, body: &str) -> PathBuf {
    use std::io::Write;
    let _guard = FORK_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir();
    let final_path =
        dir.join(format!("csa_client_mock_{}_{}_{}.sh", std::process::id(), name, seq));
    let tmp_path =
        dir.join(format!("csa_client_mock_{}_{}_{}.sh.tmp", std::process::id(), name, seq));
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .expect("open tmp script");
        f.write_all(body.as_bytes()).expect("write mock script");
        f.sync_all().expect("sync_all");
    }
    let mut perms = std::fs::metadata(&tmp_path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&tmp_path, perms).expect("chmod");
    std::fs::rename(&tmp_path, &final_path).expect("atomic rename");
    final_path
}

/// `UsiEngine::spawn` を [`FORK_WRITE_LOCK`] 下で行う (module doc 参照)。
///
/// lock は fork の瞬間だけでなく handshake 完了 (`usiok`/`readyok` 待ち) まで
/// 保持される。応答を意図的に遅延させる mock を追加すると、並行テストの script
/// 書き込みが startup_timeout まで巻き込まれてブロックされ得る点に注意。
pub fn spawn_engine(
    path: &Path,
    options: &HashMap<String, toml::Value>,
    opts: SpawnOptions,
) -> anyhow::Result<UsiEngine> {
    let _guard = FORK_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    UsiEngine::spawn(path, options, opts)
}
