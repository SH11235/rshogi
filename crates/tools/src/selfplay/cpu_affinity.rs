//! engine の CPU affinity (`--cpu-affinity numa`)。spsa と tournament で共有する。
//!
//! worker (slot) ごとに Threads 個の論理 CPU を一つの NUMA node 内から割り当て、その worker の
//! engine は全てその CPU 群に固定する。

#[cfg(any(target_os = "linux", test))]
use std::collections::BTreeSet;
#[cfg(any(target_os = "linux", test))]
use std::path::Path;

#[cfg(any(target_os = "linux", test))]
use anyhow::Context;
use anyhow::{Result, bail};

/// `--cpu-affinity` の値。
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum CpuAffinityMode {
    /// CPU affinity を設定しない。
    Off,
    /// 各 worker の engine を一つの Linux NUMA node 内の論理 CPU 群へ固定する。
    Numa,
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpu_list(value: &str) -> Result<Vec<usize>> {
    let mut cpus = BTreeSet::new();
    for part in value.trim().split(',').filter(|part| !part.is_empty()) {
        if let Some((start, end)) = part.split_once('-') {
            let start = start
                .parse::<usize>()
                .with_context(|| format!("invalid CPU range start: {part}"))?;
            let end =
                end.parse::<usize>().with_context(|| format!("invalid CPU range end: {part}"))?;
            if start > end {
                bail!("invalid descending CPU range: {part}");
            }
            cpus.extend(start..=end);
        } else {
            cpus.insert(part.parse::<usize>().with_context(|| format!("invalid CPU: {part}"))?);
        }
    }
    if cpus.is_empty() {
        bail!("CPU list is empty");
    }
    Ok(cpus.into_iter().collect())
}

#[cfg(any(target_os = "linux", test))]
fn plan_numa_cpu_affinities(
    node_cpus: &[Vec<usize>],
    threads: usize,
    worker_count: usize,
) -> Result<Vec<Vec<usize>>> {
    if threads == 0 {
        bail!("Threads must be >= 1 for CPU affinity");
    }
    let groups_by_node: Vec<Vec<Vec<usize>>> = node_cpus
        .iter()
        .map(|cpus| cpus.chunks_exact(threads).map(<[usize]>::to_vec).collect())
        .collect();
    let available_groups: usize = groups_by_node.iter().map(Vec::len).sum();
    if available_groups < worker_count {
        bail!(
            "NUMA-local CPU groups are insufficient: need {worker_count} groups × {threads} CPUs, \
             but only {available_groups} complete groups are available"
        );
    }

    // node 0 を使い切ってから node 1 へ進むと、worker 数が少ない場合に片側 socket へ
    // 偏る。各 node から 1 group ずつ round-robin で取り、負荷とメモリ帯域を均等化する。
    let mut result = Vec::with_capacity(worker_count);
    let mut group_index = 0;
    while result.len() < worker_count {
        for groups in &groups_by_node {
            if let Some(group) = groups.get(group_index) {
                result.push(group.clone());
                if result.len() == worker_count {
                    break;
                }
            }
        }
        group_index += 1;
    }
    Ok(result)
}

/// worker `worker_count` 個ぶんの CPU 群を返す。各群は一つの NUMA node 内の `threads` 個の論理 CPU で、
/// 許可された CPU (cpuset) だけから取り、node ごとに 1 群ずつ round-robin で割り当てる。
/// 完全な群が足りない、NUMA topology が見えない、または Linux 以外ではエラーを返す。
#[cfg(target_os = "linux")]
pub fn discover_numa_cpu_affinities(
    threads: usize,
    worker_count: usize,
) -> Result<Vec<Vec<usize>>> {
    let status = std::fs::read_to_string("/proc/self/status")
        .context("failed to read /proc/self/status for CPU affinity")?;
    let allowed_text = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .context("Cpus_allowed_list is missing from /proc/self/status")?;
    let node_cpus = read_numa_node_cpus(Path::new("/sys/devices/system/node"), allowed_text)?;
    plan_numa_cpu_affinities(&node_cpus, threads, worker_count)
}

#[cfg(any(target_os = "linux", test))]
fn read_numa_node_cpus(node_root: &Path, allowed_text: &str) -> Result<Vec<Vec<usize>>> {
    let allowed: BTreeSet<_> = parse_cpu_list(allowed_text)?.into_iter().collect();
    let mut node_dirs: Vec<_> = std::fs::read_dir(node_root)
        .with_context(|| format!("failed to read {}", node_root.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to enumerate {}", node_root.display()))?
        .into_iter()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.strip_prefix("node").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
        .collect();
    node_dirs.sort_by_key(|entry| entry.file_name());

    let mut node_cpus = Vec::new();
    for entry in node_dirs {
        let path = entry.path().join("cpulist");
        let cpu_list = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        // メモリのみの NUMA node は CPU 割り当ての候補にしない。
        if cpu_list.trim().is_empty() {
            continue;
        }
        let cpus: Vec<_> = parse_cpu_list(&cpu_list)?
            .into_iter()
            .filter(|cpu| allowed.contains(cpu))
            .collect();
        if !cpus.is_empty() {
            node_cpus.push(cpus);
        }
    }
    if node_cpus.is_empty() {
        bail!(
            "no NUMA node with allowed CPUs found in {}; \
             expose NUMA topology or explicitly use --cpu-affinity off",
            node_root.display()
        );
    }
    Ok(node_cpus)
}

/// worker `worker_count` 個ぶんの CPU 群を返す。各群は一つの NUMA node 内の `threads` 個の論理 CPU で、
/// 許可された CPU (cpuset) だけから取り、node ごとに 1 群ずつ round-robin で割り当てる。
/// 完全な群が足りない、NUMA topology が見えない、または Linux 以外ではエラーを返す。
#[cfg(not(target_os = "linux"))]
pub fn discover_numa_cpu_affinities(
    _threads: usize,
    _worker_count: usize,
) -> Result<Vec<Vec<usize>>> {
    bail!("--cpu-affinity numa is supported only on Linux")
}

#[cfg(test)]
mod tests {
    #[test]
    fn cpu_list_parser_handles_ranges_and_deduplicates() {
        assert_eq!(super::parse_cpu_list("0-3,2,8,10-11").unwrap(), vec![0, 1, 2, 3, 8, 10, 11]);
        assert!(super::parse_cpu_list("3-1").is_err());
        assert!(super::parse_cpu_list("").is_err());
    }

    #[test]
    fn numa_discovery_respects_cpuset_and_skips_memory_only_nodes() {
        let dir = tempfile::tempdir().unwrap();
        for (node, cpus) in [("node0", "0-3\n"), ("node1", "4-7\n"), ("node2", "\n")] {
            let path = dir.path().join(node);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("cpulist"), cpus).unwrap();
        }
        let nodes = super::read_numa_node_cpus(dir.path(), "1-2,5-6").unwrap();
        assert_eq!(nodes, vec![vec![1, 2], vec![5, 6]]);
        assert_eq!(super::plan_numa_cpu_affinities(&nodes, 2, 2).unwrap(), nodes);
    }

    #[test]
    fn numa_discovery_rejects_hidden_or_missing_topology() {
        let dir = tempfile::tempdir().unwrap();
        // node ディレクトリが見えない場合、許可 CPU を単一 node とみなせない。
        let error = super::read_numa_node_cpus(dir.path(), "0-3").unwrap_err().to_string();
        assert!(error.contains("no NUMA node"), "{error}");
        assert!(super::read_numa_node_cpus(&dir.path().join("missing"), "0-3").is_err());
    }

    #[test]
    fn numa_discovery_rejects_unreadable_or_invalid_cpu_lists() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("node0");
        std::fs::create_dir(&node).unwrap();
        assert!(super::read_numa_node_cpus(dir.path(), "0-3").is_err());
        std::fs::write(node.join("cpulist"), "invalid\n").unwrap();
        assert!(super::read_numa_node_cpus(dir.path(), "0-3").is_err());
    }

    #[test]
    fn numa_discovery_rejects_topology_without_allowed_cpus() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("node0");
        std::fs::create_dir(&node).unwrap();
        for cpus in ["\n", "4-7\n"] {
            std::fs::write(node.join("cpulist"), cpus).unwrap();
            let error = super::read_numa_node_cpus(dir.path(), "0-3").unwrap_err().to_string();
            assert!(error.contains("no NUMA node"), "{error}");
        }
    }

    #[test]
    fn numa_affinity_groups_never_cross_nodes_and_balance_round_robin() {
        let sets = super::plan_numa_cpu_affinities(
            &[
                vec![0, 1, 2, 3, 8, 9, 10, 11],
                vec![4, 5, 6, 7, 12, 13, 14, 15],
            ],
            4,
            4,
        )
        .unwrap();
        assert_eq!(
            sets,
            vec![
                vec![0, 1, 2, 3],
                vec![4, 5, 6, 7],
                vec![8, 9, 10, 11],
                vec![12, 13, 14, 15]
            ]
        );
    }

    #[test]
    fn numa_affinity_rejects_cross_node_remainder_as_insufficient() {
        let error = super::plan_numa_cpu_affinities(&[vec![0, 1, 2], vec![3, 4, 5]], 4, 1)
            .unwrap_err()
            .to_string();
        assert!(error.contains("insufficient"), "{error}");
    }
}
