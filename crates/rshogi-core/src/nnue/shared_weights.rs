//! NNUE 重みのプロセス間共有メモリ
//!
//! 多プロセス実行（自己対局 / SPRT / 評価パイプライン）では、各プロセスが NNUE の
//! FeatureTransformer 重み（~225MB）を heap に独立コピーするため、物理メモリ・L3 を
//! プロセス数ぶん圧迫し高並列で NPS が崩落する（concurrency 1→15 で実測 −32%）。
//!
//! 本モジュールは展開済みの重み配列を POSIX 共有メモリ（`shm_open` + `mmap`）に置き、
//! 同一 UID・同一 eval 内容を読む全プロセスが 1 物理コピーを参照できるようにする。
//!
//! # 設計の要点
//!
//! - 共有粒度は `AlignedBox` 単位（フラット重み配列）。重み構造体まるごとではないため
//!   POD 化不要で、全 NNUE アーキテクチャに同一機構が効く。
//! - 1 重み blob = 1 shm セグメント。命名は blob 全バイトの FNV-1a 128bit content hash。
//! - create-or-attach は `flock` 規律で直列化。creator は `flock` を `fallocate` の前に取り
//!   `ready=1` 到達後まで保持する。これにより「生存 creator ⇔ flock 保持中」が成立し、
//!   attacher が `shm_unlink` するのは「flock 保持下で size==total かつ ready!=1」＝
//!   creator 死亡確定の 1 ケースのみ（生存セグメントを誤 unlink する経路が存在しない）。
//! - attach 時は shm 上の blob を local 展開済み blob と memcmp し、一致時のみ採用する
//!   （hash 衝突や別内容セグメントでも評価値の正当性を保証）。
//! - shm 確保・mmap・検証のいずれの失敗でも local heap をそのまま使う（既存挙動維持）。
//! - セグメントの名前は、使う側が居なくなったら消す。使っている間は fd を開いたまま
//!   先頭 1 バイトに fcntl の OFD 読み取り lock を持つ（flock とは独立）。手放すときと、
//!   各プロセスで最初に共有を試すときの掃除で、flock を排他で取り、OFD 書き込み lock が
//!   取れた（= 他に使う側が居ない）名前を unlink する。プロセスが落ちても lock は OS が
//!   外すので、残った名前は次に起動したプロセスが消す。名前を消しても既存のマッピングは
//!   有効なままなので、消し過ぎても失うのは共有だけ（評価値は変わらない）。
//! - Linux 専用。それ以外のターゲットでは `try_share` は no-op。

use super::accumulator::AlignedBox;

/// 共有したマッピングを `munmap` した後に呼ぶ。最後の利用者ならセグメント名を消す。
#[cfg(target_os = "linux")]
pub(crate) fn release(map_base: *mut libc::c_void) {
    linux::release(map_base);
}

/// キルスイッチ環境変数。`0` / `off` / `false` で共有を無効化する。
/// 参照は Linux 実装のみのため、他ターゲットでは dead_code になる（cfg で除外）。
#[cfg(target_os = "linux")]
const ENV_KILL_SWITCH: &str = "RSHOGI_NNUE_SHARED_WEIGHTS";

/// 重み blob を共有メモリへ移行する。
///
/// `b` は heap-backed の重み `AlignedBox`。共有に成功した場合のみ shm-backed の box に
/// 差し替える（中身は元の heap blob とバイト単位で同一）。失敗時は何もしない（heap 維持）。
/// `label` は診断ログ用のラベル（例 `"FT weights"`）。
///
/// ネットワーク構築が完全に終わった後（重みへの全書込が済んだ後）に呼ぶこと。
pub(crate) fn try_share<T: Copy>(b: &mut AlignedBox<T>, label: &str) {
    #[cfg(target_os = "linux")]
    linux::try_share(b, label);
    #[cfg(not(target_os = "linux"))]
    {
        // Linux 以外では共有メモリ機構を持たない。heap のまま使う。
        let _ = (b, label);
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{AlignedBox, ENV_KILL_SWITCH};
    use std::ffi::CString;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// shm セグメント先頭のヘッダ。blob データは `HEADER_SIZE` バイト目から。
    #[repr(C)]
    struct ShmHeader {
        /// マジック定数（`SHM_MAGIC`）
        magic: u64,
        /// フォーマットバージョン（`FORMAT_VERSION`）
        format_version: u32,
        /// 1 要素のバイト数（`size_of::<T>()`）
        elem_size: u32,
        /// blob の要素数
        blob_len_elems: u64,
        /// content hash 下位 64bit
        identity_hash_lo: u64,
        /// content hash 上位 64bit
        identity_hash_hi: u64,
        /// 作成プロセスの PID（診断用）
        creator_pid: i32,
        /// 完成フラグ（0=未完成 / 1=完成）。最後に Release ストアされる。
        ready: AtomicU32,
    }

    /// マジック定数（ASCII "RSNNUW01" 相当）
    const SHM_MAGIC: u64 = 0x5253_4e4e_5557_3031;
    /// フォーマットバージョン
    const FORMAT_VERSION: u32 = 1;
    /// ヘッダ領域のサイズ。blob を 64 バイト境界に置くため `size_of::<ShmHeader>()` 以上の
    /// 64 の倍数とする。mmap base はページアラインのため blob は絶対 64 アラインになる。
    const HEADER_SIZE: usize = 64;

    const _: () = assert!(
        std::mem::size_of::<ShmHeader>() <= HEADER_SIZE,
        "ShmHeader must fit within HEADER_SIZE"
    );

    /// 外側 retry（unlink を伴う create-or-attach 再試行）の上限回数
    const MAX_OUTER_RETRY: u32 = 3;
    /// size 待ち内側ループの試行回数
    const SIZE_WAIT_ATTEMPTS: u32 = 10;
    /// size 待ち内側ループの 1 回あたり sleep
    const SIZE_WAIT_SLEEP: Duration = Duration::from_millis(20);

    /// FNV-1a 128bit のオフセット基底
    const FNV128_OFFSET: u128 = 0x6c62272e07bb0142_62b821756295c58d;
    /// FNV-1a 128bit の素数
    const FNV128_PRIME: u128 = 0x0000000001000000_000000000000013B;

    /// 共有対象 blob の記述子。create-or-attach の各段に渡す。
    struct BlobSpec<'a> {
        /// セグメント全長（`HEADER_SIZE` + blob バイト数）
        total: usize,
        /// 1 要素のバイト数
        elem_size: u32,
        /// blob の要素数
        len: usize,
        /// blob 全バイトの content hash
        id: u128,
        /// local 展開済み blob（creator は書込元、attacher は memcmp 対象）
        blob: &'a [u8],
    }

    /// バイト列の FNV-1a 128bit ハッシュ。
    ///
    /// `std::hash::DefaultHasher` はアルゴリズム・出力の永続安定性が保証されないため、
    /// content-address 用には固定アルゴリズムを自前実装する。
    fn fnv1a_128(bytes: &[u8]) -> u128 {
        let mut h = FNV128_OFFSET;
        for &b in bytes {
            h ^= b as u128;
            h = h.wrapping_mul(FNV128_PRIME);
        }
        h
    }

    /// content hash と要素数から shm セグメント名を作る。
    fn segment_name(id: u128, blob_len_elems: usize) -> String {
        format!("/rshogi-nnue-v1-{id:032x}-{blob_len_elems}")
    }

    /// キルスイッチ環境変数が共有を無効化しているか。
    fn env_disabled() -> bool {
        match std::env::var(ENV_KILL_SWITCH) {
            Ok(v) => {
                let v = v.trim();
                v == "0" || v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false")
            }
            Err(_) => false,
        }
    }

    /// 直前の libc 呼び出しの errno を返す。
    fn last_errno() -> Option<i32> {
        std::io::Error::last_os_error().raw_os_error()
    }

    /// 診断ログ 1 行。
    fn report(label: &str, name: &str, shared: bool, detail: &str) {
        let mode = if shared { "shared" } else { "local" };
        log::info!("nnue shared weights [{label}]: {mode} ({detail}) {name}");
    }

    /// 共有メモリの名前が並ぶ dir（glibc の `shm_open` の置き場）
    const SHM_DIR: &str = "/dev/shm";
    /// 重みのセグメント名の接頭辞（`segment_name` の先頭の `/` を除いたもの）
    const SEGMENT_PREFIX: &str = "rshogi-nnue-v1-";

    /// 使っている印の OFD lock を取る。`kind` は `F_RDLCK`（使っている）か `F_WRLCK`
    /// （他に使う側が居ないことの確認）。待たずに結果を返す。
    fn ofd_lock(fd: libc::c_int, kind: libc::c_int) -> bool {
        // SAFETY: flock 構造体は全フィールド 0 が有効な値。
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = kind as libc::c_short;
        lock.l_whence = libc::SEEK_SET as libc::c_short;
        lock.l_start = 0;
        lock.l_len = 1;
        // SAFETY: fd は呼び出し元が開いた有効な fd、lock は有効な flock 構造体。
        unsafe { libc::fcntl(fd, libc::F_OFD_SETLK, &lock) == 0 }
    }

    /// `fd` のセグメントに名前が残っていて、他に使う側が居なければ名前を消す。
    ///
    /// 名前を消すのは flock を排他で持つ間だけ（作る側・繋ぐ側・掃除の全部がそうする）。
    /// なので flock を取った後に `st_nlink > 0` なら、名前はまだ `fd` の inode を指している。
    /// 作りかけ（長さ 0）は消さない（作る側が `shm_open` から flock を取るまでの間がある）。
    fn unlink_if_unused(fd: libc::c_int, cname: &CString) {
        // SAFETY: fd は有効。他が flock を持つ間（作っている・繋いでいる）は待つ。
        if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            return;
        }
        // SAFETY: st は zeroed な libc::stat。fd 有効。
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let linked = unsafe { libc::fstat(fd, &mut st) } == 0 && st.st_nlink > 0;
        if linked && st.st_size > 0 && ofd_lock(fd, libc::F_WRLCK) {
            // SAFETY: cname は有効な C 文字列。上の理由で名前は fd の inode を指す。
            unsafe { libc::shm_unlink(cname.as_ptr()) };
        }
        // SAFETY: fd 有効。
        unsafe { libc::flock(fd, libc::LOCK_UN) };
    }

    /// `prefix` で始まる名前のうち、誰も使っていないものを消す（落ちたプロセスの残り）。
    fn sweep_unused(prefix: &str) {
        let Ok(entries) = std::fs::read_dir(SHM_DIR) else {
            return;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.starts_with(prefix) {
                continue;
            }
            let Ok(cname) = CString::new(format!("/{name}")) else {
                continue;
            };
            // SAFETY: cname は有効な C 文字列。他の UID の名前は開けずに飛ばす。
            let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0o600) };
            if fd < 0 {
                continue;
            }
            // 掃除は起動を待たせないよう、作っている・繋いでいる最中の名前は飛ばす。
            // SAFETY: fd は有効。
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                unlink_if_unused(fd, &cname);
            }
            // SAFETY: fd は有効。close で flock と OFD lock も外れる。
            unsafe { libc::close(fd) };
        }
    }

    /// 共有したマッピングの先頭番地と、そのセグメントを使っている印（OFD 読み取り lock を
    /// 持つ fd）と名前。`AlignedBox` の大きさを変えないよう、box の外に置く。重みの読み込み・
    /// 手放しの時にしか触らない。
    static IN_USE: std::sync::Mutex<Vec<(usize, libc::c_int, CString)>> =
        std::sync::Mutex::new(Vec::new());

    /// `base` のマッピングを `munmap` した後に呼ぶ。最後の利用者なら名前を消し、fd を閉じる。
    pub(crate) fn release(base: *mut libc::c_void) {
        let entry = {
            let mut in_use = IN_USE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            in_use
                .iter()
                .position(|(at, _, _)| *at == base as usize)
                .map(|index| in_use.swap_remove(index))
        };
        if let Some((_, fd, name)) = entry {
            unlink_if_unused(fd, &name);
            // SAFETY: fd は IN_USE から取り出した、この値だけが持つ有効な fd。
            unsafe { libc::close(fd) };
        }
    }

    /// create-or-attach の成功結果。
    struct ObtainOk {
        /// mmap が返したマッピング先頭
        base: *mut libc::c_void,
        /// 使っている印の OFD 読み取り lock を持つ fd
        fd: libc::c_int,
        /// 診断用ロール文字列
        role: &'static str,
    }

    /// attach 試行の結果。
    enum AttachResult {
        /// attach 成功（memcmp 一致済み）。`fd` は使っている印の lock を持つ。
        Ok {
            base: *mut libc::c_void,
            fd: libc::c_int,
        },
        /// 共有を諦め local heap を使う
        Fallback,
        /// stale を unlink した／誰かが unlink したので外側 retry する
        RetryOuter,
    }

    /// 重み blob を共有メモリへ移行する（Linux 実装）。
    pub(super) fn try_share<T: Copy>(b: &mut AlignedBox<T>, label: &str) {
        if env_disabled() {
            report(label, "", false, "disabled by env");
            return;
        }

        let len = b.len();
        let elem_size = std::mem::size_of::<T>();
        if len == 0 || elem_size == 0 {
            return; // 空 blob は共有不要
        }
        // 共有領域上の blob 先頭は `base`（ページアライン）+ `HEADER_SIZE`。`HEADER_SIZE`
        // は 64 のため blob 先頭は 64 バイトアラインだが、それ以上は保証できない。
        // `align_of::<T>() > HEADER_SIZE` の型では `from_shared` 後の `Deref` が未アライン
        // スライスを生む恐れがあるため、その場合は共有せず local heap を使う。
        if std::mem::align_of::<T>() > HEADER_SIZE {
            report(label, "", false, "alignment exceeds header");
            return;
        }

        // blob のバイト長を checked 計算（汎用 helper のためオーバーフローを防ぐ）。
        let blob_bytes = match len.checked_mul(elem_size) {
            Some(n) => n,
            None => {
                report(label, "", false, "size overflow");
                return;
            }
        };

        // blob を &[u8] として視る。
        // SAFETY: `b` は `len` 個の `T` を保持する有効なスライス。本 helper は crate 内から
        // 整数プリミティブ（`i8`/`i16`/`i32`）の `AlignedBox` に対してのみ呼ばれる。これらは
        // padding を持たず全バイトが初期化済みで安定したバイト表現を持つため、`[T]` を
        // `blob_bytes` バイトの `[u8]` として読むのは健全。
        let blob: &[u8] =
            unsafe { std::slice::from_raw_parts(b.as_ptr() as *const u8, blob_bytes) };

        let id = fnv1a_128(blob);

        // セグメント全長も checked 計算。
        let total = match HEADER_SIZE.checked_add(blob_bytes) {
            Some(t) if t <= i64::MAX as usize => t,
            _ => {
                report(label, "", false, "size overflow");
                return;
            }
        };

        let name = segment_name(id, len);
        if name.len() >= libc::NAME_MAX as usize {
            report(label, &name, false, "name too long");
            return;
        }
        let cname = match CString::new(name.clone()) {
            Ok(c) => c,
            Err(_) => {
                report(label, &name, false, "invalid name");
                return;
            }
        };

        // 前に落ちたプロセスが残した名前を、プロセスごとに 1 度だけ消す。
        static SWEEP: std::sync::Once = std::sync::Once::new();
        SWEEP.call_once(|| sweep_unused(SEGMENT_PREFIX));

        let spec = BlobSpec {
            total,
            elem_size: elem_size as u32,
            len,
            id,
            blob,
        };

        match obtain_segment(&cname, &spec) {
            Some(ok) => {
                // blob の借用はここで終了（NLL）。以降 `*b` を mut で書ける。
                // SAFETY:
                // - `data_ptr` = base + HEADER_SIZE。base はページアライン、HEADER_SIZE=64
                //   は `align_of::<T>()` の倍数（上のガードで `align_of::<T>() <= 64` を保証）
                //   なので `data_ptr` は `T` のアライン要件を満たす。
                // - shm セグメントは [base, base+total) で、blob は
                //   [base+HEADER_SIZE, base+total) に `len` 要素ぶん格納されている。
                // - `ok.base` / `total` は mmap で得たマッピングそのもの。
                // - この `AlignedBox` が唯一の所有者（1 box : 1 mapping）。
                // - 共有領域はロード後 read-only。`AlignedBox` の `Shared` backing は
                //   `DerefMut` が panic するため可変参照を発行せず、協調プロセスも以後
                //   書き込まない（creator の `mprotect(PROT_READ)` は事故検出用の追加防御で、
                //   その成否に read-only 性は依存しない）。
                let data_ptr = unsafe { (ok.base as *mut u8).add(HEADER_SIZE) as *mut T };
                IN_USE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((
                    ok.base as usize,
                    ok.fd,
                    cname,
                ));
                let shared = unsafe { AlignedBox::from_shared(data_ptr, len, ok.base, total) };
                *b = shared;
                report(label, &name, true, ok.role);
            }
            None => {
                report(label, &name, false, "fallback");
            }
        }
    }

    /// create-or-attach プロトコル本体。
    fn obtain_segment(cname: &CString, spec: &BlobSpec) -> Option<ObtainOk> {
        for _ in 0..MAX_OUTER_RETRY {
            // SAFETY: cname は有効な C 文字列。shm_open は fd または -1 を返す。
            let create_fd = unsafe {
                libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600)
            };
            if create_fd >= 0 {
                return create_segment(create_fd, cname, spec, reserve_segment);
            }
            if last_errno() != Some(libc::EEXIST) {
                // 権限不足・ENOSPC など → local fallback。
                return None;
            }
            // 既存セグメントへ attach。
            match attach_segment(cname, spec) {
                AttachResult::Ok { base, fd } => {
                    return Some(ObtainOk {
                        base,
                        fd,
                        role: "attached",
                    });
                }
                AttachResult::Fallback => return None,
                AttachResult::RetryOuter => continue,
            }
        }
        None
    }

    fn reserve_segment(fd: libc::c_int, total: libc::off_t) -> std::io::Result<()> {
        // SAFETY: fd は creator が開いた書込可能な shm、total は検証済みの正の長さ。
        if unsafe { libc::fallocate(fd, 0, 0, total) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// creator 経路: セグメントを作成し重みを書き込む。
    fn create_segment(
        fd: libc::c_int,
        cname: &CString,
        spec: &BlobSpec,
        reserve: impl FnOnce(libc::c_int, libc::off_t) -> std::io::Result<()>,
    ) -> Option<ObtainOk> {
        // 失敗時クリーンアップ: unlink → flock 解放 → close。ready=1 到達前の失敗で
        // 未完成セグメントを残さない。mmap 後は失敗経路が無いため munmap は不要。
        // `flock(LOCK_UN)` は未取得時も無害。
        let cleanup = || {
            // SAFETY: fd は shm_open で得た有効な fd。
            unsafe {
                libc::shm_unlink(cname.as_ptr());
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
        };

        // flock を fallocate の前に取得し、ready=1 後まで保持する。
        // SAFETY: fd は有効。
        if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            cleanup();
            return None;
        }
        let Ok(total) = libc::off_t::try_from(spec.total) else {
            cleanup();
            return None;
        };
        // 長さを設定するだけでは、tmpfs の空き不足が mmap への書込時に SIGBUS になる。
        if let Err(error) = reserve(fd, total) {
            log::debug!("nnue shared weights: fallocate failed: {error}");
            cleanup();
            return None;
        }
        // SAFETY: fd は有効で、fallocate が total バイト分を確保済み。
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                spec.total,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            cleanup();
            return None;
        }

        // 共有マッピングに対する THP のヒント。
        // SAFETY: base / total は上の mmap で得た領域そのもの。
        unsafe {
            libc::madvise(base, spec.total, libc::MADV_HUGEPAGE);
        }

        // ヘッダと blob を書き込む。
        // SAFETY: base は total バイトの有効な書込可能マッピング先頭。ページアラインの
        // ため `*mut ShmHeader` として整列している。shm は zero-fill 済みのため
        // `ShmHeader` の全フィールドは有効なビットパターン。blob コピー先
        // [base+HEADER_SIZE, base+total) は blob.len() バイトを収める。
        unsafe {
            let hdr = base as *mut ShmHeader;
            (*hdr).magic = SHM_MAGIC;
            (*hdr).format_version = FORMAT_VERSION;
            (*hdr).elem_size = spec.elem_size;
            (*hdr).blob_len_elems = spec.len as u64;
            (*hdr).identity_hash_lo = spec.id as u64;
            (*hdr).identity_hash_hi = (spec.id >> 64) as u64;
            (*hdr).creator_pid = libc::getpid();
            let dst = (base as *mut u8).add(HEADER_SIZE);
            std::ptr::copy_nonoverlapping(spec.blob.as_ptr(), dst, spec.blob.len());
            // 全書込を完了してから ready を公開する。
            (*hdr).ready.store(1, Ordering::Release);
        }

        // 書込後は read-only 化（事故書込を SIGSEGV で顕在化。失敗は致命でない）。
        // SAFETY: base / total は上の mmap で得た領域そのもの。
        unsafe {
            libc::mprotect(base, spec.total, libc::PROT_READ);
        }

        // creator 経路でも、共有領域を採用する前に元データとバイト単位で照合する。
        // SAFETY: [base+HEADER_SIZE, base+total) は blob.len() バイトの有効領域。
        let shm_blob = unsafe {
            std::slice::from_raw_parts((base as *const u8).add(HEADER_SIZE), spec.blob.len())
        };
        if shm_blob != spec.blob {
            // 想定外の不一致 → セグメントを破棄して local fallback。
            // SAFETY: base/total は mmap 領域、cname は有効な C 文字列、fd は有効。
            unsafe {
                libc::munmap(base, spec.total);
                libc::shm_unlink(cname.as_ptr());
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
            return None;
        }

        // 使っている印は flock を放す前に取る（放した直後の掃除に消されないように）。
        // 取れない kernel でも共有は続ける（名前が早く消えて共有が減るだけ）。
        if !ofd_lock(fd, libc::F_RDLCK) {
            log::debug!(
                "nnue shared weights: use lock failed: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: fd は有効。fd は印として開いたまま返す。
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
        Some(ObtainOk {
            base,
            fd,
            role: "created",
        })
    }

    /// attacher 経路: 既存セグメントへ attach し、検証・memcmp する。
    fn attach_segment(cname: &CString, spec: &BlobSpec) -> AttachResult {
        for attempt in 0..SIZE_WAIT_ATTEMPTS {
            // SAFETY: cname は有効な C 文字列。
            let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0o600) };
            if fd < 0 {
                if last_errno() == Some(libc::ENOENT) {
                    // 誰かが unlink 済み → 外側 retry（creator になり得る）。
                    return AttachResult::RetryOuter;
                }
                return AttachResult::Fallback;
            }
            // SAFETY: fd 有効。creator が setup 中ならブロックする。
            if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
                // SAFETY: fd 有効。
                unsafe { libc::close(fd) };
                return AttachResult::Fallback;
            }

            // fstat で size を確認。
            // SAFETY: st は zeroed な libc::stat。fd 有効。
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut st) } != 0 {
                // SAFETY: fd 有効。
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                    libc::close(fd);
                }
                return AttachResult::Fallback;
            }

            if st.st_nlink == 0 {
                // 待機中に unlink された fd で、同名の新しいセグメントを消さない。
                // SAFETY: fd は有効。
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                    libc::close(fd);
                }
                return AttachResult::RetryOuter;
            }

            if st.st_size as usize != spec.total {
                // creator が shm_open 直後で flock をまだ取得していない可能性がある。
                // creator が確保を進められるよう、flock を解放してから再試行する。
                // SAFETY: fd 有効。
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                    libc::close(fd);
                }
                if attempt + 1 < SIZE_WAIT_ATTEMPTS {
                    std::thread::sleep(SIZE_WAIT_SLEEP);
                    continue;
                }
                // 再試行後も creator の終了を確定できないため、名前を残して local へ戻る。
                return AttachResult::Fallback;
            }

            // 未完成セグメントには hole があり、mmap での読み込みも
            // 空き容量不足で SIGBUS になり得る。flock 保持下で ready を pread する。
            let mut ready = [0; size_of::<u32>()];
            // SAFETY: fd は有効で、ready は指定長の書込可能なバッファ。
            let read = unsafe {
                libc::pread(
                    fd,
                    ready.as_mut_ptr().cast(),
                    ready.len(),
                    std::mem::offset_of!(ShmHeader, ready) as libc::off_t,
                )
            };
            if read != ready.len() as libc::ssize_t {
                // SAFETY: fd は有効。
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                    libc::close(fd);
                }
                return AttachResult::Fallback;
            }
            if u32::from_ne_bytes(ready) != 1 {
                log::info!("nnue shared weights: stale segment unlinked");
                // SAFETY: fd は有効。flock 保持下では creator は終了済み。
                unsafe {
                    libc::shm_unlink(cname.as_ptr());
                    libc::flock(fd, libc::LOCK_UN);
                    libc::close(fd);
                }
                return AttachResult::RetryOuter;
            }

            // SAFETY: fd は有効で、ready の公開前に全ページへの書込が済んでいる。
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    spec.total,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            if base == libc::MAP_FAILED {
                // SAFETY: fd 有効。
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                    libc::close(fd);
                }
                return AttachResult::Fallback;
            }

            return finish_attach(fd, base, spec);
        }
        AttachResult::Fallback
    }

    /// attach: mmap 後のヘッダ検証 + memcmp。
    fn finish_attach(fd: libc::c_int, base: *mut libc::c_void, spec: &BlobSpec) -> AttachResult {
        let hdr = base as *const ShmHeader;
        // SAFETY: base はページ境界に整列したヘッダ。ready の確認と flock により
        // creator が全フィールドを書き終えたことが保証される。
        let (magic, fmt, esz, blen, hlo, hhi) = unsafe {
            (
                (*hdr).magic,
                (*hdr).format_version,
                (*hdr).elem_size,
                (*hdr).blob_len_elems,
                (*hdr).identity_hash_lo,
                (*hdr).identity_hash_hi,
            )
        };
        let id_ok = hlo == spec.id as u64 && hhi == (spec.id >> 64) as u64;
        let header_ok = magic == SHM_MAGIC
            && fmt == FORMAT_VERSION
            && esz == spec.elem_size
            && blen == spec.len as u64
            && id_ok;
        if !header_ok {
            // 健全（ready==1）だが別内容のセグメント（hash 衝突 / squat）。
            // 健全セグメントは決して unlink しない → local fallback。
            // SAFETY: base/total は上の mmap 領域。fd は有効。
            unsafe {
                libc::munmap(base, spec.total);
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
            return AttachResult::Fallback;
        }

        // shm 上の blob と local 展開済み blob をバイト単位で比較。
        // SAFETY: [base+HEADER_SIZE, base+total) は blob.len() バイトを収める有効領域。
        let shm_blob = unsafe {
            std::slice::from_raw_parts((base as *const u8).add(HEADER_SIZE), spec.blob.len())
        };
        if shm_blob != spec.blob {
            // 内容不一致 → 健全セグメントは unlink せず local fallback。
            // SAFETY: base/total は上の mmap 領域。fd は有効。
            unsafe {
                libc::munmap(base, spec.total);
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
            return AttachResult::Fallback;
        }

        // 採用。使っている印は flock を放す前に取る。
        if !ofd_lock(fd, libc::F_RDLCK) {
            log::debug!(
                "nnue shared weights: use lock failed: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: fd 有効。fd は印として開いたまま返す。
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
        AttachResult::Ok { base, fd }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fnv1a_128_empty_is_offset_basis() {
            assert_eq!(fnv1a_128(&[]), FNV128_OFFSET);
        }

        #[test]
        fn fnv1a_128_distinguishes_inputs() {
            assert_ne!(fnv1a_128(b"abc"), fnv1a_128(b"abd"));
            assert_ne!(fnv1a_128(b"abc"), fnv1a_128(b"ab"));
        }

        #[test]
        fn segment_name_is_valid_shm_name() {
            let name = segment_name(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef, 1536 * 73305);
            assert!(name.starts_with('/'));
            assert!(!name[1..].contains('/'));
            assert!(name.len() < libc::NAME_MAX as usize);
        }

        #[test]
        fn header_fits_in_header_size() {
            assert!(std::mem::size_of::<ShmHeader>() <= HEADER_SIZE);
        }

        #[test]
        fn reservation_failure_discards_unready_segment() {
            let name =
                CString::new(format!("/rshogi-nnue-test-reserve-{}", std::process::id())).unwrap();
            let spec = BlobSpec {
                total: HEADER_SIZE + 1,
                elem_size: 1,
                len: 1,
                id: 0,
                blob: &[42],
            };
            // SAFETY: name は有効な C 文字列。
            let fd = unsafe {
                libc::shm_open(name.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600)
            };
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            assert!(
                create_segment(fd, &name, &spec, |_, size| {
                    assert_eq!(size as usize, spec.total);
                    Err(std::io::Error::from_raw_os_error(libc::ENOSPC))
                })
                .is_none()
            );
            // SAFETY: name は有効な C 文字列。open が成功した場合は fd を閉じる。
            let reopened = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
            let error = last_errno();
            if reopened >= 0 {
                // SAFETY: reopened は上で開いた有効な fd。
                unsafe { libc::close(reopened) };
            }
            assert_eq!(reopened, -1);
            assert_eq!(error, Some(libc::ENOENT));
        }

        fn segment_exists(name: &CString) -> bool {
            // SAFETY: name は有効な C 文字列。開けたら閉じる。
            let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
            if fd >= 0 {
                // SAFETY: fd は上で開いた有効な fd。
                unsafe { libc::close(fd) };
            }
            fd >= 0
        }

        /// 長さ `len` のセグメントを作って fd を返す（掃除の試験用）。
        fn make_test_segment(name: &CString, len: libc::off_t) -> libc::c_int {
            // SAFETY: name は有効な C 文字列。前回の取りこぼしを消してから作る。
            let fd = unsafe {
                libc::shm_unlink(name.as_ptr());
                libc::shm_open(name.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600)
            };
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            // SAFETY: fd は上で作った書込可能な shm。
            assert_eq!(unsafe { libc::ftruncate(fd, len) }, 0);
            fd
        }

        #[test]
        fn last_user_release_unlinks_segment() {
            let n = 2048usize;
            let pattern: Vec<i32> = (0..n as i32).map(|i| i.wrapping_mul(7919) ^ 0x3c3c).collect();
            let make_box = || {
                let mut b: AlignedBox<i32> = AlignedBox::new_zeroed(n);
                b.copy_from_slice(&pattern);
                b
            };
            // SAFETY: pattern は n 個の i32。i32 を u8 列として読むのは健全。
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(pattern.as_ptr() as *const u8, n * size_of::<i32>())
            };
            let cseg = CString::new(segment_name(fnv1a_128(bytes), n)).unwrap();
            // SAFETY: cseg は有効な C 文字列。ENOENT は無視される。
            unsafe { libc::shm_unlink(cseg.as_ptr()) };

            let mut a = make_box();
            try_share(&mut a, "test-release-a");
            let mut b = make_box();
            try_share(&mut b, "test-release-b");
            if !segment_exists(&cseg) {
                return; // /dev/shm が使えない環境では local fallback になる
            }
            drop(a);
            assert!(segment_exists(&cseg), "他に使う側が居る間は名前を残す");
            assert_eq!(&b[..], pattern.as_slice());
            drop(b);
            assert!(!segment_exists(&cseg), "最後の利用者が手放したら名前を消す");
        }

        #[test]
        fn sweep_removes_only_unused_segments() {
            let prefix = format!("rshogi-nnue-test-sweep-{}-", std::process::id());
            let unused = CString::new(format!("/{prefix}unused")).unwrap();
            let used = CString::new(format!("/{prefix}used")).unwrap();
            let empty = CString::new(format!("/{prefix}empty")).unwrap();
            let unused_fd = make_test_segment(&unused, 4096);
            // SAFETY: unused_fd は有効。持ち主が落ちた後と同じく、印を持たずに閉じる。
            unsafe { libc::close(unused_fd) };
            let used_fd = make_test_segment(&used, 4096);
            assert!(ofd_lock(used_fd, libc::F_RDLCK));
            let empty_fd = make_test_segment(&empty, 0);
            // SAFETY: empty_fd は有効。
            unsafe { libc::close(empty_fd) };

            sweep_unused(&prefix);

            assert!(!segment_exists(&unused), "誰も使っていない名前は消す");
            assert!(segment_exists(&used), "使っている印のある名前は残す");
            assert!(segment_exists(&empty), "作りかけ（長さ 0）は残す");
            // SAFETY: used_fd は有効。各 name は有効な C 文字列。
            unsafe {
                libc::close(used_fd);
                libc::shm_unlink(used.as_ptr());
                libc::shm_unlink(empty.as_ptr());
            }
        }

        /// creator → attacher の往復で重み内容がバイト単位で保存されることを確認する。
        /// `/dev/shm` が使えない環境では local fallback になるが、その場合も内容は不変。
        #[test]
        fn try_share_round_trip_preserves_bytes() {
            // 実モデルと衝突しない一意なテストパターン。
            let n = 4096usize;
            let pattern: Vec<i16> =
                (0..n).map(|i| (i as i16).wrapping_mul(31) ^ 0x5aa5u16 as i16).collect();

            let make_box = || {
                let mut b: AlignedBox<i16> = AlignedBox::new_zeroed(n);
                b.copy_from_slice(&pattern);
                b
            };

            // 後始末用にセグメント名を計算し、前回 run の取りこぼしを掃除する。
            let bytes: &[u8] =
                // SAFETY: pattern は n 個の i16。i16 を u8 列として読むのは健全。
                unsafe {
                    std::slice::from_raw_parts(
                        pattern.as_ptr() as *const u8,
                        n * std::mem::size_of::<i16>(),
                    )
                };
            let cseg = std::ffi::CString::new(segment_name(fnv1a_128(bytes), n)).unwrap();
            // SAFETY: cseg は有効な C 文字列。ENOENT は無視される。
            unsafe { libc::shm_unlink(cseg.as_ptr()) };

            // creator 経路: 共有後も内容がバイト一致すること。
            let mut a = make_box();
            try_share(&mut a, "test-create");
            assert_eq!(&a[..], pattern.as_slice(), "creator: 共有後も内容がバイト一致");

            // attacher 経路: 同一内容 → 同名セグメントへ attach、memcmp 一致で採用。
            let mut b = make_box();
            try_share(&mut b, "test-attach");
            assert_eq!(&b[..], pattern.as_slice(), "attacher: 共有後も内容がバイト一致");

            // 共有 box の Clone は private heap コピーになり、内容も一致する。
            let cloned = a.clone();
            assert_eq!(&cloned[..], pattern.as_slice(), "clone: 内容がバイト一致");

            // 後始末: マッピングを drop してからセグメント名を消す。
            drop(a);
            drop(b);
            drop(cloned);
            // SAFETY: cseg は有効な C 文字列。ENOENT は無視される。
            unsafe { libc::shm_unlink(cseg.as_ptr()) };
        }
    }
}
