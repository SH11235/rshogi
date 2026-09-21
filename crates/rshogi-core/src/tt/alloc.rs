use std::alloc::Layout;
use std::ptr::NonNull;

#[cfg(not(windows))]
use std::alloc::handle_alloc_error;
#[cfg(not(windows))]
use std::alloc::{alloc, dealloc};
#[cfg(not(windows))]
use std::cmp::max;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE, LUID};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueA, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{
    GetLargePageMinimum, MEM_COMMIT, MEM_LARGE_PAGES, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
    VirtualAlloc, VirtualFree,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[derive(Clone, Copy, Debug)]
pub(super) enum AllocKind {
    /// Windows の MEM_LARGE_PAGES による確保に成功
    #[cfg(windows)]
    LargePages,
    /// Linux/Android で MADV_HUGEPAGE の要求に成功。実際の backing は OS が決める
    #[cfg(any(target_os = "linux", target_os = "android"))]
    HugePageHint,
    /// Large Pages 確保や hint 要求に失敗した場合のフォールバック、
    /// または macOS 等の未対応環境で使用
    Regular,
}

impl AllocKind {
    /// Windows の明示的な Large Pages 確保に成功した種別か。
    /// Linux/Android の hint 要求は実際の backing を保証しないため含めない。
    pub(super) fn is_explicit_large_pages(self) -> bool {
        match self {
            #[cfg(windows)]
            AllocKind::LargePages => true,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            AllocKind::HugePageHint => false,
            AllocKind::Regular => false,
        }
    }

    /// Linux/Android で huge-page hint の要求に成功した種別か。
    pub(super) fn is_huge_page_hint(self) -> bool {
        match self {
            #[cfg(windows)]
            AllocKind::LargePages => false,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            AllocKind::HugePageHint => true,
            AllocKind::Regular => false,
        }
    }
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
            debug_assert!(alignment.is_power_of_two(), "alignment must be power of two");
            if let Some(alloc) = try_alloc_large_pages(size) {
                return alloc;
            }
            alloc_windows(size, alignment)
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
    value.div_ceil(align) * align
}

#[cfg(windows)]
fn try_alloc_large_pages(size: usize) -> Option<Allocation> {
    unsafe {
        let large_page_size = GetLargePageMinimum() as usize;
        if large_page_size == 0 {
            return None;
        }

        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)
            == 0
        {
            return None;
        }

        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        let privilege_name = c"SeLockMemoryPrivilege";
        if LookupPrivilegeValueA(std::ptr::null(), privilege_name.as_ptr() as *const u8, &mut luid)
            == 0
        {
            CloseHandle(token);
            return None;
        }

        let tp = TOKEN_PRIVILEGES {
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
        if AdjustTokenPrivileges(token, 0, &tp, prev_len, &mut prev_tp, &mut prev_len) == 0
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

        AdjustTokenPrivileges(token, 0, &prev_tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        CloseHandle(token);

        let ptr = NonNull::new(ptr as *mut u8)?;
        Some(Allocation {
            ptr,
            kind: AllocKind::LargePages,
        })
    }
}

#[cfg(windows)]
fn alloc_windows(size: usize, alignment: usize) -> Allocation {
    unsafe {
        let ptr =
            VirtualAlloc(std::ptr::null_mut(), size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        let ptr = NonNull::new(ptr as *mut u8).unwrap_or_else(|| {
            let align = alignment.max(4096);
            std::alloc::handle_alloc_error(Layout::from_size_align(size, align).unwrap())
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
    let page_align = 2 * 1024 * 1024;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let page_align = 4096;

    let alignment = max(alignment, page_align);
    let layout = Layout::from_size_align(size, alignment)
        .expect("Invalid TT allocation layout")
        .pad_to_align();
    // SAFETY: layout は from_size_align が検証済み。TT は最小 2 cluster を確保するため size は 0 にならない。
    // 返った領域は Allocation が単独所有し、Drop で同じ layout を使って解放する。
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        handle_alloc_error(layout);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    let kind = {
        // SAFETY: ptr は直前に layout で確保した領域の先頭で、layout.size() はその全長。
        // MADV_HUGEPAGE は配置のヒントであり、領域の内容や有効性を変えない。
        let result = unsafe { libc::madvise(ptr as *mut _, layout.size(), libc::MADV_HUGEPAGE) };
        // madvise失敗は動作に影響しないが、パフォーマンスに影響する可能性があるため
        // デバッグビルドでは警告を出力
        #[cfg(debug_assertions)]
        if result != 0 {
            eprintln!("Warning: madvise MADV_HUGEPAGE failed");
        }
        if result == 0 {
            AllocKind::HugePageHint
        } else {
            AllocKind::Regular
        }
    };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let kind = AllocKind::Regular;

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

// SAFETY: 割当を単独所有し、所有権の移動後も同じレイアウトで解放する。共有の安全性は格納型側で保証する。
unsafe impl Send for Allocation {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_pages_report_neither_status() {
        assert!(!AllocKind::Regular.is_explicit_large_pages());
        assert!(!AllocKind::Regular.is_huge_page_hint());
    }

    #[cfg(windows)]
    #[test]
    fn explicit_large_pages_are_not_a_hint() {
        assert!(AllocKind::LargePages.is_explicit_large_pages());
        assert!(!AllocKind::LargePages.is_huge_page_hint());
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn huge_page_hint_is_not_reported_as_large_pages() {
        assert!(AllocKind::HugePageHint.is_huge_page_hint());
        assert!(!AllocKind::HugePageHint.is_explicit_large_pages());
    }

    #[test]
    fn allocation_kind_matches_the_platform() {
        let allocation = Allocation::allocate(1 << 20, 64);
        let kind = allocation.kind();
        assert!(!(kind.is_explicit_large_pages() && kind.is_huge_page_hint()));
        #[cfg(not(windows))]
        assert!(!kind.is_explicit_large_pages());
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        assert!(!kind.is_huge_page_hint());
    }
}
