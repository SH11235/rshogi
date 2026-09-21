//! Windows の書込共有を拒否した、NNUE 重み用の読み取り専用領域。
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY,
    UnmapViewOfFile,
};

/// 他プロセスの読み取りだけを許可する共有フラグ。
const FILE_SHARE_READ: u32 = 0x0000_0001;
/// mapping 中の rename / delete を許可する共有フラグ。
///
/// rename / delete は directory entry を書き換えるだけで、section が保持する file の
/// 内容は変化しない。書込共有 (`FILE_SHARE_WRITE`) は与えないままなので、
/// 切り詰め (`SetEndOfFile`) を含む内容の変更は引き続き拒否される。
/// つまり「ロード時に一度検証した SHA-256 が最後まで有効」という不変条件は保たれる。
const FILE_SHARE_DELETE: u32 = 0x0000_0004;

pub(super) struct ReadOnlyMapping {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    // File を先に閉じず、最後の view 解放まで書込共有の拒否を維持する。
    file: File,
}

impl ReadOnlyMapping {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        // 書込共有は与えない。既存の書込用 handle/view がある場合も open は失敗する。
        // delete 共有は与え、稼働中でもモデル file の差し替え（新 file を書いて
        // rename で被せる）を妨げないようにする。
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .open(path)?;
        let metadata = file.metadata()?;
        let len = metadata.len();
        if !metadata.is_file() || len == 0 || len > isize::MAX as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid mapping length"));
        }
        // SAFETY: 有効なfile handleを保持し、書き込みと切り詰めを拒否している。
        // rename / delete は許可するが、どちらもこのsectionが保持する内容を変えない。
        // 読み取り専用sectionを作成し、返ったhandleはこの関数で1度だけ閉じる。
        let handle = unsafe {
            CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 有効なsectionの先頭から、検証したfile長の範囲内だけをmapする。
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, len as usize) };
        let error = io::Error::last_os_error();
        // SAFETY: 成功したviewはsectionを保持する。ローカルhandleは他へ渡していない。
        unsafe {
            CloseHandle(handle);
        }
        if view.Value.is_null() {
            return Err(error);
        }
        Ok(Self {
            view,
            len: len as usize,
            file,
        })
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: file長は固定、viewはselfの全寿命で有効かつread-only。
        // file handleを保持し、他の書き込み可能handle/viewとの共存を拒否する。
        unsafe { std::slice::from_raw_parts(self.view.Value.cast::<u8>(), self.len) }
    }
}

impl Drop for ReadOnlyMapping {
    fn drop(&mut self) {
        // fileはフィールドとしてこのDrop終了後に閉じる。viewを先に解放する。
        debug_assert!(!self.file.as_raw_handle().is_null());
        // SAFETY: このobjectがviewの唯一の所有者。借用は全て終了している。
        unsafe {
            UnmapViewOfFile(self.view);
        }
    }
}

// SAFETY: viewは不変で、fileはSend+Sync。所有権の移動でaddressは変化しない。
unsafe impl Send for ReadOnlyMapping {}
// SAFETY: viewはread-onlyで書込共有を拒否しており、複数threadから読み取りだけを行う。
unsafe impl Sync for ReadOnlyMapping {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::accumulator::WeightBox;
    use std::io::Write;
    use std::sync::Arc;
    use windows_sys::Win32::System::Memory::{FILE_MAP_WRITE, PAGE_READWRITE};

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "rshogi-map-{}-{unique}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let mut file = OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
            let mut data = vec![0; 64];
            for value in -128i16..128 {
                data.extend_from_slice(&value.to_le_bytes());
            }
            file.write_all(&data).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).unwrap();
        }
    }

    #[test]
    fn mapped_rows_keep_owner_alive_and_copy_on_write_to_heap() -> io::Result<()> {
        let fixture = Fixture::new();
        let owner = Arc::new(ReadOnlyMapping::open(&fixture.0)?);
        let first = WeightBox::<i16>::from_mapped(owner.clone(), 64, 32)?;
        let second = WeightBox::<i16>::from_mapped(owner.clone(), 128, 32)?;
        assert_eq!(first[0], -128);
        assert_eq!(second[0], -96);
        let second = std::thread::spawn(move || {
            assert_eq!(second[31], -65);
            second
        })
        .join()
        .unwrap();
        assert!((first.as_ptr() as usize).is_multiple_of(64));
        drop(owner);
        // make_mut は mapping を私有ヒープへ複製してから可変スライスを返す。
        let mut copied =
            WeightBox::<i16>::from_mapped(Arc::new(ReadOnlyMapping::open(&fixture.0)?), 64, 32)?;
        copied.make_mut()[0] = 1234;
        assert_eq!(copied[0], 1234);
        assert_eq!(first[0], -128);
        assert!(OpenOptions::new().write(true).open(&fixture.0).is_err());
        drop(first);
        assert!(OpenOptions::new().write(true).open(&fixture.0).is_err());
        // copied は複製後に mapping を手放しているので、残る保持者は second だけ。
        drop(second);
        assert!(OpenOptions::new().write(true).open(&fixture.0).is_ok());
        assert_eq!(copied[0], 1234);
        Ok(())
    }

    #[test]
    fn mapped_psqt_and_threat_spans_preserve_bits_and_ownership() {
        let fixture = Fixture::new();
        let values = [i32::MIN, -1, 0, i32::MAX];
        {
            let mut file = OpenOptions::new().write(true).open(&fixture.0).unwrap();
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(64)).unwrap();
            for value in values {
                file.write_all(&value.to_le_bytes()).unwrap();
            }
        }
        let owner = Arc::new(ReadOnlyMapping::open(&fixture.0).unwrap());
        let psqt = WeightBox::<i32>::from_mapped(owner.clone(), 64, 4).unwrap();
        let threat = WeightBox::<i8>::from_mapped(owner.clone(), 64, 16).unwrap();
        assert_eq!(&*psqt, &values);
        let expected: Vec<i8> =
            values.iter().flat_map(|v| v.to_le_bytes()).map(|b| b as i8).collect();
        assert_eq!(&*threat, &expected);
        let mut copied = WeightBox::<i32>::from_mapped(owner.clone(), 64, 4).unwrap();
        copied.make_mut()[0] = 123;
        assert_eq!(copied[0], 123);
        assert_eq!(psqt[0], i32::MIN);
        drop(owner);
        drop(psqt);
        assert!(OpenOptions::new().write(true).open(&fixture.0).is_err());
        drop(threat);
        assert!(OpenOptions::new().write(true).open(&fixture.0).is_ok());
    }

    #[test]
    fn mapped_typed_spans_validate_element_width_and_exact_end() {
        let fixture = Fixture::new();
        let owner = Arc::new(ReadOnlyMapping::open(&fixture.0).unwrap());
        // fixtureは576B。最後の64Bで各型の要素幅を検査する。
        assert!(WeightBox::<i8>::from_mapped(owner.clone(), 512, 64).is_ok());
        assert!(WeightBox::<i16>::from_mapped(owner.clone(), 512, 32).is_ok());
        assert!(WeightBox::<i32>::from_mapped(owner.clone(), 512, 16).is_ok());
        assert!(WeightBox::<i8>::from_mapped(owner.clone(), 512, 65).is_err());
        assert!(WeightBox::<i16>::from_mapped(owner.clone(), 512, 33).is_err());
        assert!(WeightBox::<i32>::from_mapped(owner.clone(), 512, 17).is_err());
        assert!(WeightBox::<i32>::from_mapped(owner.clone(), 64, usize::MAX / 4 + 1).is_err());
        assert!(WeightBox::<i8>::from_mapped(owner.clone(), 64, usize::MAX).is_err());
        assert!(WeightBox::<i32>::from_mapped(owner, 65, 1).is_err());
    }

    #[test]
    fn mapped_rows_reject_invalid_spans() {
        let fixture = Fixture::new();
        let owner = Arc::new(ReadOnlyMapping::open(&fixture.0).unwrap());
        for (offset, count) in [
            (1, 1),
            (64, 0),
            (512, 33),
            (usize::MAX, 1),
            (64, usize::MAX),
        ] {
            assert!(WeightBox::<i16>::from_mapped(owner.clone(), offset, count).is_err());
        }
        drop(owner);
        File::create(&fixture.0).unwrap();
        assert!(ReadOnlyMapping::open(&fixture.0).is_err());
    }

    /// `WeightBox` は `DerefMut` を持たないため、可変参照は `make_mut` 経由でしか取れない。
    /// `make_mut` は mapping を私有ヒープへ複製するので、mapping 元 file のバイト列も、
    /// 同じ span を借用する他の box も変化しない。
    #[test]
    fn make_mut_copies_on_write_and_leaves_the_mapped_file_untouched() -> io::Result<()> {
        let fixture = Fixture::new();
        let before = std::fs::read(&fixture.0)?;
        let owner = Arc::new(ReadOnlyMapping::open(&fixture.0)?);
        let peer = WeightBox::<i16>::from_mapped(owner.clone(), 64, 32)?;
        let mut row = WeightBox::<i16>::from_mapped(owner, 64, 32)?;
        assert_eq!(row[0], -128);
        row.make_mut()[0] = 1;
        assert_eq!(row[0], 1);
        assert_eq!(peer[0], -128);
        drop(peer);
        drop(row);
        assert_eq!(std::fs::read(&fixture.0)?, before);
        Ok(())
    }

    /// 稼働中のモデル差し替え手順（新 file を書いて rename で被せる）が通り、
    /// かつ mapping 済みのバイト列が変化しないこと。書込 open は引き続き拒否される。
    #[test]
    fn mapping_allows_replacing_the_file_by_rename_without_changing_mapped_bytes() -> io::Result<()>
    {
        let fixture = Fixture::new();
        let owner = Arc::new(ReadOnlyMapping::open(&fixture.0)?);
        let mapped = WeightBox::<i16>::from_mapped(owner.clone(), 64, 32)?;
        let before = mapped.to_vec();
        // 書込共有は与えていないので、内容の書き換えは依然として拒否される。
        assert!(OpenOptions::new().write(true).open(&fixture.0).is_err());

        let replacement = fixture.0.with_extension("replacement");
        std::fs::write(&replacement, vec![0x5au8; 576])?;
        std::fs::rename(&replacement, &fixture.0)?;

        assert_eq!(&*mapped, before.as_slice());
        assert_eq!(owner.bytes().len(), 576);
        assert_ne!(std::fs::read(&fixture.0)?, owner.bytes());
        Ok(())
    }

    #[test]
    fn preexisting_writable_view_prevents_readonly_open() {
        let fixture = Fixture::new();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(3)
            .open(&fixture.0)
            .unwrap();
        // SAFETY: test owns a nonempty file with read/write access, and keeps it open until mapping.
        let handle = unsafe {
            CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                0,
                std::ptr::null(),
            )
        };
        assert!(!handle.is_null());
        // SAFETY: valid section handle, map the existing nonempty file read/write.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, 0) };
        // SAFETY: successful view retains section; test uniquely owns this handle.
        unsafe {
            CloseHandle(handle);
        }
        assert!(!view.Value.is_null());
        drop(file);
        let result = ReadOnlyMapping::open(&fixture.0);
        // SAFETY: test owns this view, and has never created references into it.
        unsafe {
            UnmapViewOfFile(view);
        }
        assert!(result.is_err());
    }
}
