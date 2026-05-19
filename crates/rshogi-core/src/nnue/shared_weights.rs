//! NNUE 重みのプロセス間共有メモリ
//!
//! 多プロセス実行（自己対局 / SPRT / 評価パイプライン）では、各プロセスが NNUE の
//! FeatureTransformer 重み（~225MB）を heap に独立コピーするため、物理メモリ・L3 を
//! プロセス数ぶん圧迫し高並列で NPS が崩落する（concurrency 1→15 で実測 −32%）。
//!
//! 本モジュールは展開済みの重み配列を POSIX 共有メモリ（`shm_open` + `mmap`）に置き、
//! 同一 UID・同一 eval 内容を読む全プロセスが 1 物理コピーを参照できるようにする。
//! YaneuraOu commit `2a4cb3d`（`SystemWideSharedConstant<NnueNetworks>`）と同型の機構。
//!
//! # 設計の要点
//!
//! - 共有粒度は `AlignedBox` 単位（フラット重み配列）。重み構造体まるごとではないため
//!   POD 化不要で、全 NNUE アーキテクチャに同一機構が効く。
//! - 1 重み blob = 1 shm セグメント。命名は blob 全バイトの FNV-1a 128bit content hash。
//! - create-or-attach は `flock` 規律で直列化。creator は `flock` を `ftruncate` の前に取り
//!   `ready=1` 到達後まで保持する。これにより「生存 creator ⇔ flock 保持中」が成立し、
//!   attacher が `shm_unlink` するのは「flock 保持下で size==total かつ ready!=1」＝
//!   creator 死亡確定の 1 ケースのみ（生存セグメントを誤 unlink する経路が存在しない）。
//! - attach 時は shm 上の blob を local 展開済み blob と memcmp し、一致時のみ採用する
//!   （hash 衝突や別内容セグメントでも評価値の正当性を保証）。
//! - shm 確保・mmap・検証のいずれの失敗でも local heap をそのまま使う（既存挙動維持）。
//! - Linux 専用。それ以外のターゲットでは `try_share` は no-op。

use super::accumulator::AlignedBox;

/// キルスイッチ環境変数。`0` / `off` / `false` で共有を無効化する。
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

    /// create-or-attach の成功結果。
    struct ObtainOk {
        /// mmap が返したマッピング先頭
        base: *mut libc::c_void,
        /// 診断用ロール文字列
        role: &'static str,
    }

    /// attach 試行の結果。
    enum AttachResult {
        /// attach 成功（memcmp 一致済み）
        Ok(*mut libc::c_void),
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
                return create_segment(create_fd, cname, spec);
            }
            if last_errno() != Some(libc::EEXIST) {
                // 権限不足・ENOSPC など → local fallback。
                return None;
            }
            // 既存セグメントへ attach。
            match attach_segment(cname, spec) {
                AttachResult::Ok(base) => {
                    return Some(ObtainOk {
                        base,
                        role: "attached",
                    });
                }
                AttachResult::Fallback => return None,
                AttachResult::RetryOuter => continue,
            }
        }
        None
    }

    /// creator 経路: セグメントを作成し重みを書き込む。
    fn create_segment(fd: libc::c_int, cname: &CString, spec: &BlobSpec) -> Option<ObtainOk> {
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

        // flock を ftruncate の前に取得し、ready=1 後まで保持する（flock 規律）。
        // SAFETY: fd は有効。
        if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            cleanup();
            return None;
        }
        // SAFETY: fd 有効、total は i64 範囲内（呼び出し元で検証済み）。
        if unsafe { libc::ftruncate(fd, spec.total as libc::off_t) } != 0 {
            cleanup();
            return None;
        }
        // SAFETY: fd 有効、total バイトが ftruncate 済み。
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
            // 全書込の後に ready を Release ストア（attacher の Acquire と対）。
            (*hdr).ready.store(1, Ordering::Release);
        }

        // 書込後は read-only 化（事故書込を SIGSEGV で顕在化。失敗は致命でない）。
        // SAFETY: base / total は上の mmap で得た領域そのもの。
        unsafe {
            libc::mprotect(base, spec.total, libc::PROT_READ);
        }

        // 書き込んだ shm blob が local blob とバイト一致することを確認する。
        // `copy_nonoverlapping` 直後のため通常一致するが、rev4 設計の「共有採用は
        // バイト一致確認後」という不変条件を creator 経路でも満たすための検証。
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

        // SAFETY: fd は有効。
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
            libc::close(fd);
        }
        Some(ObtainOk {
            base,
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

            if st.st_size as usize != spec.total {
                // size != total。内訳:
                // - `st_size == 0`: creator が `shm_open` 後・`ftruncate` 前に死亡した
                //   未完成セグメント（残骸）の可能性。
                // - `0 < st_size < total`: `ftruncate` は size を 0→total へ atomic に
                //   変えるため flock 規律下では通常発生しない。別フォーマット／別実装が
                //   同名で作ったセグメント等、将来の互換性のための保険として同経路で扱う。
                // lock を持ったまま待たず解放し、内側ループで shm_open からやり直す。
                // SAFETY: fd 有効。
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                    libc::close(fd);
                }
                if attempt + 1 < SIZE_WAIT_ATTEMPTS {
                    std::thread::sleep(SIZE_WAIT_SLEEP);
                    continue;
                }
                // 内側ループ上限まで size 不一致が続いた → unlink せず local fallback。
                //
                // 採用方針 A（rev4 設計で採用）: `st_size != total` は
                // 「生存 creator が `shm_open`→`ftruncate` の数命令窓に居る」可能性を
                // 排除できない唯一の曖昧ケースのため unlink しない。これにより
                // 「生存 creator・健全セグメントを誤 unlink する経路が一つも無い」が
                // 例外なく成立する。
                //
                // 代償: creator が上記 μs 窓で SIGKILL されると size-0 残骸が残り、
                // 同一 eval の共有が手動 cleanup（`rm /dev/shm/rshogi-nnue-*`）まで
                // 無効化される。ただし現実的に最も起きやすい「memcpy 中の OOM kill」は
                // size==total・ready!=1 になり下の `ready` 経路で自動 unlink+retry 回復
                // するため、本経路で残骸化するのは μs 窓 kill のみ＝極めて稀。
                return AttachResult::Fallback;
            }

            // size == total → mmap（read-only）。
            // SAFETY: fd 有効、total バイトが確保済み（fstat 確認済）。
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

            return finish_attach(fd, cname, base, spec);
        }
        AttachResult::Fallback
    }

    /// attach: mmap 後のヘッダ検証 + memcmp。
    fn finish_attach(
        fd: libc::c_int,
        cname: &CString,
        base: *mut libc::c_void,
        spec: &BlobSpec,
    ) -> AttachResult {
        // まず ready を Acquire ロードする（creator の Release ストアと対）。
        // SAFETY: base は total バイト（>= HEADER_SIZE）の有効な読取可能マッピング先頭で、
        // ページアラインのため `*const ShmHeader` として整列している。
        let hdr = base as *const ShmHeader;
        let ready = unsafe { (*hdr).ready.load(Ordering::Acquire) };

        if ready != 1 {
            // flock 保持下で size==total かつ ready!=1 ＝ creator が ftruncate 後・
            // ready=1 前に死亡したことが確定（生存 creator は flock 保持中のはず）。
            // 唯一の正当な unlink ケース。
            // SAFETY: base は有効な ShmHeader。
            let creator_pid = unsafe { (*hdr).creator_pid };
            log::info!("nnue shared weights: stale segment (creator_pid={creator_pid}) unlinked");
            // SAFETY: base/total は上の mmap 領域。fd は有効。
            unsafe {
                libc::munmap(base, spec.total);
                libc::shm_unlink(cname.as_ptr());
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
            return AttachResult::RetryOuter;
        }

        // ready==1 を確認したので非 atomic ヘッダフィールドを読んで検証する。
        // SAFETY: base は有効な ShmHeader。creator は ready=1 の前に全フィールドを
        // 書き込んでおり、Release/Acquire と flock 同期で happens-before が成立。
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

        // 採用。
        // SAFETY: fd 有効。
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
            libc::close(fd);
        }
        AttachResult::Ok(base)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fnv1a_128_empty_is_offset_basis() {
            assert_eq!(fnv1a_128(&[]), FNV128_OFFSET);
        }

        #[test]
        fn fnv1a_128_is_deterministic() {
            let data = b"rshogi nnue shared weights";
            assert_eq!(fnv1a_128(data), fnv1a_128(data));
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
