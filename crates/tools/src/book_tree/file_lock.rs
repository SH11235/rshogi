//! 台帳と lock のファイルに取るファイル lock。
//!
//! Unix では std の lock (flock、勧告) をそのまま使う。Windows の lock (LockFileEx) は強制で、
//! std の lock はファイル全体に掛かるため、serve の実行中は他のプロセスが台帳を読めない。
//! Windows では EOF より先の 1 バイトだけを lock し、データの範囲は読めるままにする。
//! どちらも lock はファイルの実体に付くので、hard link やシンボリックリンクの別名でも排他になる。

#[cfg(not(windows))]
pub(super) fn try_lock(file: &std::fs::File) -> Result<(), std::fs::TryLockError> {
    file.try_lock()
}

#[cfg(not(windows))]
pub(super) fn try_lock_shared(file: &std::fs::File) -> Result<(), std::fs::TryLockError> {
    file.try_lock_shared()
}

#[cfg(not(windows))]
pub(super) fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    file.unlock()
}

#[cfg(windows)]
pub(super) use windows::{try_lock, try_lock_shared, unlock};

#[cfg(windows)]
mod windows {
    use std::fs::{File, TryLockError};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // 台帳がここまで伸びることはない。std の lock (0 から u64::MAX バイト) の範囲内なので、
    // std の lock を取る別のハンドルとも衝突する。
    const OFFSET: u64 = u64::MAX - 1;

    fn region() -> OVERLAPPED {
        let mut overlapped = OVERLAPPED::default();
        overlapped.Anonymous.Anonymous.Offset = OFFSET as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (OFFSET >> 32) as u32;
        overlapped
    }

    fn lock(file: &File, flags: u32) -> Result<(), TryLockError> {
        let mut overlapped = region();
        // SAFETY: ハンドルは `file` が生きている間有効で、std は同期 I/O で開くので
        // `overlapped` は呼び出しの間だけ使われる (完了を待つ非同期 I/O にならない)。
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                flags | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            Err(TryLockError::WouldBlock)
        } else {
            Err(TryLockError::Error(error))
        }
    }

    pub(in super::super) fn try_lock(file: &File) -> Result<(), TryLockError> {
        lock(file, LOCKFILE_EXCLUSIVE_LOCK)
    }

    pub(in super::super) fn try_lock_shared(file: &File) -> Result<(), TryLockError> {
        lock(file, 0)
    }

    pub(in super::super) fn unlock(file: &File) -> std::io::Result<()> {
        let mut overlapped = region();
        // SAFETY: `lock` と同じ。解除する範囲は `lock` で取った範囲と同じでなければならない。
        let ok = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
        if ok != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{File, TryLockError};

    #[test]
    fn locked_file_stays_readable_and_excludes_hard_link_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        std::fs::write(&path, b"line\n").unwrap();
        let held = File::options().read(true).write(true).open(&path).unwrap();
        super::try_lock(&held).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"line\n");
        let alias = dir.path().join("alias.jsonl");
        std::fs::hard_link(&path, &alias).unwrap();
        let other = File::open(&alias).unwrap();
        assert!(matches!(super::try_lock_shared(&other), Err(TryLockError::WouldBlock)));
        super::unlock(&held).unwrap();
        super::try_lock_shared(&other).unwrap();
        super::unlock(&other).unwrap();
    }
}
