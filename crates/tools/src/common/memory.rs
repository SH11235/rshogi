//! システムメモリ情報のクロスプラットフォーム取得。

use sysinfo::{MemoryRefreshKind, RefreshKind, System};

fn effective_available_memory(host_available: u64, cgroup_available: Option<u64>) -> Option<u64> {
    (host_available > 0)
        .then(|| cgroup_available.map_or(host_available, |available| host_available.min(available)))
}

/// OS が報告する利用可能メモリをバイト単位で返す。
///
/// Linux、Windows、macOS を含む `sysinfo` 対応環境で利用できる。
/// Linux の cgroup 内では、ホストの利用可能量と cgroup の残量の小さい方を返す。
/// OS から情報を取得できない場合は `None` を返す。
pub fn available_memory_bytes() -> Option<u64> {
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let available = system.available_memory();
    effective_available_memory(available, system.cgroup_limits().map(|limits| limits.free_memory))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_available_memory_limits_host_memory() {
        assert_eq!(effective_available_memory(1_000, Some(400)), Some(400));
        assert_eq!(effective_available_memory(1_000, Some(2_000)), Some(1_000));
        assert_eq!(effective_available_memory(1_000, Some(0)), Some(0));
        assert_eq!(effective_available_memory(0, None), None);
    }
}
