use std::alloc::{handle_alloc_error, Layout};
use std::ptr::NonNull;

#[cfg(not(windows))]
use std::alloc::{alloc, dealloc};
#[cfg(not(windows))]
use std::cmp::max;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_SUCCESS};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueA, OpenProcessToken, LUID, LUID_AND_ATTRIBUTES,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{
    GetLargePageMinimum, VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_LARGE_PAGES, MEM_RELEASE,
    MEM_RESERVE, PAGE_READWRITE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::GetCurrentProcess;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AllocKind {
    LargePages,
    /// Windows で Large Pages 確保失敗時のフォールバック、
    /// または macOS 等の Large Pages 未対応環境で使用
    #[allow(dead_code)]
    Regular,
}

pub(super) struct Allocation {
    ptr: NonNull<u8>,
    kind: AllocKind,
    #[cfg(not(windows))]
    layout: Layout,
}

impl Allocation {
    pub(super) fn allocate(size: usize, alignment: usize) -> Self {
        #[cfg(windows)]
        {
            if let Some(alloc) = try_alloc_large_pages(size) {
                return alloc;
            }
            return alloc_windows(size);
        }

        #[cfg(not(windows))]
        {
            alloc_unix(size, alignment)
        }
    }

    pub(super) fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    pub(super) fn kind(&self) -> AllocKind {
        self.kind
    }
}

#[cfg(windows)]
fn align_up(value: usize, align: usize) -> usize {
    // 呼び出し元のTTサイズは実用上64-bit環境でオーバーフローしないが、
    // 防御的にdebug_assertでチェック（リリースビルドでは無効化）
    debug_assert!(
        value.checked_add(align - 1).is_some(),
        "align_up overflow: value={value}, align={align}"
    );
    (value + align - 1) / align * align
}

#[cfg(windows)]
fn try_alloc_large_pages(size: usize) -> Option<Allocation> {
    unsafe {
        let large_page_size = GetLargePageMinimum() as usize;
        if large_page_size == 0 {
            return None;
        }

        let mut token = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)
            == 0
        {
            return None;
        }

        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        if LookupPrivilegeValueA(
            std::ptr::null(),
            b"SeLockMemoryPrivilege\0".as_ptr() as *const i8,
            &mut luid,
        ) == 0
        {
            CloseHandle(token);
            return None;
        }

        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut prev_tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 0,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: LUID {
                    LowPart: 0,
                    HighPart: 0,
                },
                Attributes: 0,
            }],
        };
        let mut prev_len = std::mem::size_of::<TOKEN_PRIVILEGES>() as u32;

        // AdjustTokenPrivileges が非ゼロを返しても ERROR_SUCCESS でない場合は
        // 部分的な失敗（ERROR_NOT_ALL_ASSIGNED等）を意味するためチェック
        if AdjustTokenPrivileges(token, 0, &mut tp, prev_len, &mut prev_tp, &mut prev_len) == 0
            || GetLastError() != ERROR_SUCCESS
        {
            CloseHandle(token);
            return None;
        }

        let alloc_size = align_up(size, large_page_size);
        let ptr = VirtualAlloc(
            std::ptr::null_mut(),
            alloc_size,
            MEM_RESERVE | MEM_COMMIT | MEM_LARGE_PAGES,
            PAGE_READWRITE,
        );

        AdjustTokenPrivileges(
            token,
            0,
            &mut prev_tp,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        CloseHandle(token);

        let ptr = NonNull::new(ptr as *mut u8)?;
        Some(Allocation {
            ptr,
            kind: AllocKind::LargePages,
        })
    }
}

#[cfg(windows)]
fn alloc_windows(size: usize) -> Allocation {
    unsafe {
        let ptr =
            VirtualAlloc(std::ptr::null_mut(), size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        let ptr = NonNull::new(ptr as *mut u8).unwrap_or_else(|| {
            std::alloc::handle_alloc_error(Layout::from_size_align(size, 4096).unwrap())
        });
        Allocation {
            ptr,
            kind: AllocKind::Regular,
        }
    }
}

#[cfg(not(windows))]
fn alloc_unix(size: usize, alignment: usize) -> Allocation {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let (page_align, kind) = (2 * 1024 * 1024, AllocKind::LargePages);
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let (page_align, kind) = (4096, AllocKind::Regular);

    let alignment = max(alignment, page_align);
    let layout = Layout::from_size_align(size, alignment)
        .expect("Invalid TT allocation layout")
        .pad_to_align();
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        handle_alloc_error(layout);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        let result = libc::madvise(ptr as *mut _, layout.size(), libc::MADV_HUGEPAGE);
        // madvise失敗は動作に影響しないが、パフォーマンスに影響する可能性があるため
        // デバッグビルドでは警告を出力
        #[cfg(debug_assertions)]
        if result != 0 {
            eprintln!("Warning: madvise MADV_HUGEPAGE failed");
        }
        #[cfg(not(debug_assertions))]
        let _ = result;
    }

    Allocation {
        ptr: NonNull::new(ptr).expect("TT allocation returned null"),
        kind,
        layout,
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            #[cfg(windows)]
            {
                let ok = VirtualFree(self.ptr.as_ptr() as *mut _, 0, MEM_RELEASE);
                if ok == 0 {
                    // リソースリークの可能性があるため、リリースビルドでも警告を出力
                    eprintln!("Warning: VirtualFree failed with error {}", GetLastError());
                    debug_assert!(false, "VirtualFree failed");
                }
            }
            #[cfg(not(windows))]
            {
                dealloc(self.ptr.as_ptr(), self.layout);
            }
        }
    }
}

// SAFETY: Allocation owns raw memory for the TT and is protected by higher-level synchronization.
unsafe impl Send for Allocation {}
unsafe impl Sync for Allocation {}
