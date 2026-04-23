/// PSV ファイルの局面重複削除ツール（ディスクパーティション方式）
///
/// 重複キー: 先頭 32 バイトの PackedSfen（`psv_dedup` と同じ方針）
/// 方式: 完全一致 (exact dedup)。偽陽性・偽陰性なし。
///
/// ## 仕組み
///
/// 1. **Phase 1 (partitioning)**: 入力を順次読み、PackedSfen の 64bit FNV-1a
///    ハッシュで `--partitions` 個の一時ファイルに振り分ける。`--reference`
///    指定時は参照ファイル群も同じハッシュで別サブディレクトリに振り分ける。
/// 2. **Phase 2 (deduplication)**: 各パーティションを 1 つずつ `HashSet<[u8;32]>`
///    にロードし、first-wins で出力ファイルへ追記する。参照パーティションが
///    あれば先に HashSet に入れ（出力はしない）、続けて入力パーティションを
///    照合して新規局面だけ出力する。
///
/// ピークメモリは「全ユニーク局面」ではなく「最大パーティションのユニーク局面」に
/// 抑えられるため、`psv_dedup` では載らない規模でも exact dedup が可能。
/// 代償として、一時ディスクが入力と同等サイズ必要で、I/O は約 2 倍になる。
///
/// Usage:
///   cargo run --release --bin psv_dedup_partition -- \
///     --input-dir /path/to/dir \
///     --pattern "*.bin" \
///     --output /path/to/deduped.bin \
///     --temp-dir /path/to/tmp
///
///   # reference モード: 既存 dedup 済みファイルとの差分だけ抽出
///   cargo run --release --bin psv_dedup_partition -- \
///     --reference existing_deduped.bin \
///     --input new_data.bin \
///     --output unique_new.bin \
///     --temp-dir /path/to/tmp
///
///   # 既存の一時ファイルから Phase 2 のみ再実行
///   cargo run --release --bin psv_dedup_partition -- \
///     --output /path/to/deduped.bin \
///     --temp-dir /path/to/tmp \
///     --phase2-only
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use tools::common::dedup::{
    PSV_SIZE, SFEN_SIZE, check_output_not_in_inputs, collect_input_paths, format_gib,
    get_disk_available, get_mem_available, hash_packed_sfen, same_filesystem, sum_file_sizes,
};

const INPUT_SUBDIR: &str = "input";
const REF_SUBDIR: &str = "ref";

#[derive(Parser, Debug)]
#[command(
    name = "psv_dedup_partition",
    about = "ディスクパーティションによる exact PSV 重複除去 (低メモリ)"
)]
struct Args {
    /// 参照ファイル（カンマ区切り）。HashSet に登録するが出力しない。
    /// 既存 dedup 済みファイルとの差分だけ出力したいときに使う。
    #[arg(long)]
    reference: Option<String>,

    /// 入力 PSV ファイル（カンマ区切り）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせ。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.bin")]
    pattern: String,

    /// 出力ファイルパス
    #[arg(long)]
    output: PathBuf,

    /// 一時ディレクトリ（パーティションファイルの置き場）
    #[arg(long, default_value = "./psv_dedup_partition_tmp")]
    temp_dir: PathBuf,

    /// パーティション数。多いほど Phase 2 の 1 パーティションあたりメモリが減るが
    /// file descriptor と出力バッファの総量が増える。
    #[arg(long, default_value = "1024")]
    partitions: usize,

    /// Phase 1 でパーティションごとに確保する BufWriter のバッファサイズ (KiB)
    #[arg(long, default_value = "64")]
    partition_buffer_kb: usize,

    /// 処理する入力レコードの最大件数（0 = 全件、試走向け）。
    /// 参照ファイルは常に全件読み込まれる。
    #[arg(long, default_value = "0")]
    max_positions: u64,

    /// Phase 1 をスキップして既存の一時ファイルから Phase 2 のみ実行。
    /// temp_dir/ref/ が存在すれば自動で reference モードになる。
    #[arg(long)]
    phase2_only: bool,

    /// 完了後も一時ディレクトリを削除しない
    #[arg(long)]
    keep_temp: bool,

    /// メモリ/ディスクの事前見積りチェックをスキップして強制実行する。
    /// swap 多用・途中失敗のリスクを許容する場合のみ使う。
    #[arg(long)]
    force: bool,
}

/// `HashSet<[u8; SFEN_SIZE]>` 1 エントリあたりの推定メモリ使用量（バイト）。
/// hashbrown の RawTable は key(32B) + 制御バイト(1B) + load factor 7/8 + ヘッダ等を
/// 含めて実測で 40-48B 前後だが、余裕を見て 56B をカウントする。
const HASHSET_ENTRY_BYTES: u64 = 56;

/// ハッシュ分布のばらつきによる最大パーティションの余剰係数。
const HASH_VARIANCE_FACTOR: f64 = 1.2;

/// ディスクチェックの安全マージン (5%)。
const DISK_SAFETY_FACTOR: f64 = 1.05;

/// メモリ不足判定のしきい値 (MemAvailable の 80%)。
const MEM_THRESHOLD_FACTOR: f64 = 0.8;

struct ResourceEstimate {
    total_records: u64,
    /// Phase 1 が一時ディレクトリに書き出すバイト数 (= ref + input)
    phase1_temp_bytes: u64,
    /// 出力の上限バイト数。reference モードでは input 側だけが出力対象。
    output_upper_bound_bytes: u64,
    /// same filesystem 上で Phase 2 に一時的に必要になる追加空き容量の見積り。
    /// `--keep-temp` なしでは「現在処理中の最大 input partition ぶん」、
    /// `--keep-temp` ありでは使わない。
    phase2_peak_partition_input_bytes: u64,
    phase1_memory_bytes: u64,
    phase2_peak_memory_bytes: u64,
}

fn estimate_resources(
    ref_size_bytes: u64,
    input_size_bytes: u64,
    num_partitions: usize,
    partition_buffer_bytes: usize,
) -> ResourceEstimate {
    let total_bytes = ref_size_bytes + input_size_bytes;
    let total_records = total_bytes / PSV_SIZE as u64;
    let avg_records_per_partition = total_records as f64 / num_partitions.max(1) as f64;
    let peak_records_per_partition =
        (avg_records_per_partition * HASH_VARIANCE_FACTOR).ceil() as u64;
    let phase2_peak_mem = peak_records_per_partition.saturating_mul(HASHSET_ENTRY_BYTES);
    let phase1_mem = (num_partitions as u64).saturating_mul(partition_buffer_bytes as u64);
    let input_records = input_size_bytes / PSV_SIZE as u64;
    let avg_input_records_per_partition = input_records as f64 / num_partitions.max(1) as f64;
    let peak_input_records_per_partition =
        (avg_input_records_per_partition * HASH_VARIANCE_FACTOR).ceil() as u64;
    let phase2_peak_partition_input_bytes =
        peak_input_records_per_partition.saturating_mul(PSV_SIZE as u64);

    ResourceEstimate {
        total_records,
        phase1_temp_bytes: total_bytes,
        output_upper_bound_bytes: input_size_bytes,
        phase2_peak_partition_input_bytes,
        phase1_memory_bytes: phase1_mem,
        phase2_peak_memory_bytes: phase2_peak_mem,
    }
}

fn same_fs_output_headroom_bytes(estimate: &ResourceEstimate, keep_temp: bool) -> u64 {
    if keep_temp {
        estimate.output_upper_bound_bytes
    } else {
        estimate.phase2_peak_partition_input_bytes
    }
}

fn output_disk_required_bytes(
    estimate: &ResourceEstimate,
    same_fs: bool,
    skip_temp_check: bool,
    keep_temp: bool,
) -> u64 {
    if same_fs {
        let same_fs_headroom =
            (same_fs_output_headroom_bytes(estimate, keep_temp) as f64 * DISK_SAFETY_FACTOR) as u64;
        if skip_temp_check {
            same_fs_headroom
        } else {
            ((estimate.phase1_temp_bytes as f64 * DISK_SAFETY_FACTOR) as u64)
                .saturating_add(same_fs_headroom)
        }
    } else {
        (estimate.output_upper_bound_bytes as f64 * DISK_SAFETY_FACTOR) as u64
    }
}

/// 不足チェック結果を INFO 出力し、不足時は Err（`force` なら Warning）。
fn preflight_check(
    estimate: &ResourceEstimate,
    temp_dir: &Path,
    output_path: &Path,
    skip_temp_check: bool,
    keep_temp: bool,
    force: bool,
) -> io::Result<()> {
    eprintln!("=== Resource Estimate ===");
    eprintln!(
        "Total input records:  {} ({} bytes / {})",
        estimate.total_records, estimate.phase1_temp_bytes, PSV_SIZE
    );
    if !skip_temp_check {
        eprintln!(
            "Phase 1 temp disk:    {} (cleaned up on success)",
            format_gib(estimate.phase1_temp_bytes),
        );
        eprintln!(
            "Phase 1 memory:       {} (fixed: partitions × buffer)",
            format_gib(estimate.phase1_memory_bytes),
        );
    }
    eprintln!(
        "Phase 2 peak memory:  {} (HashSet of largest partition, ~{:.2}x variance)",
        format_gib(estimate.phase2_peak_memory_bytes),
        HASH_VARIANCE_FACTOR,
    );

    let peak_mem = estimate.phase1_memory_bytes.max(estimate.phase2_peak_memory_bytes);

    // --- メモリチェック ---
    if let Some(avail) = get_mem_available() {
        let threshold = (avail as f64 * MEM_THRESHOLD_FACTOR) as u64;
        eprintln!(
            "Memory available:     {} (threshold {:.0}% = {})",
            format_gib(avail),
            MEM_THRESHOLD_FACTOR * 100.0,
            format_gib(threshold),
        );
        if peak_mem > threshold {
            let msg = format!(
                "メモリ不足: 推定ピーク {} が threshold {} を超えます。\n\
                 対処法:\n\
                 - --partitions を大きくする（1 パーティションのメモリが減る）\n\
                 - --force で強制続行（swap 使用の可能性）",
                format_gib(peak_mem),
                format_gib(threshold),
            );
            if force {
                eprintln!("Warning (--force): {msg}");
            } else {
                return Err(io::Error::other(msg));
            }
        }
    } else {
        eprintln!("Memory available:     (取得不可、メモリチェックをスキップ)");
    }

    // --- ディスクチェック ---
    let output_parent = {
        let p = output_path.parent().unwrap_or(Path::new("."));
        if p.as_os_str().is_empty() {
            Path::new(".")
        } else {
            p
        }
    };

    if !skip_temp_check {
        let temp_required = (estimate.phase1_temp_bytes as f64 * DISK_SAFETY_FACTOR) as u64;
        if let Some(avail) = get_disk_available(temp_dir) {
            eprintln!("Temp disk available:  {} ({})", format_gib(avail), temp_dir.display());
            if temp_required > avail {
                let msg = format!(
                    "一時ディスク不足: 約 {} 必要ですが {} しか空きがありません ({})。\n\
                     対処法:\n\
                     - --temp-dir で容量のあるファイルシステムを指定\n\
                     - --force で強制続行",
                    format_gib(temp_required),
                    format_gib(avail),
                    temp_dir.display(),
                );
                if force {
                    eprintln!("Warning (--force): {msg}");
                } else {
                    return Err(io::Error::other(msg));
                }
            }
        }
    }

    // 出力ディスクチェック（temp と同一 fs の場合は Phase 2 の一時的な headroom を見る）
    let same_fs = same_filesystem(temp_dir, output_parent);
    // 出力上限は input 側のみ（reference は出力対象外なので除外する）
    let output_required = (estimate.output_upper_bound_bytes as f64 * DISK_SAFETY_FACTOR) as u64;
    if let Some(avail) = get_disk_available(output_parent) {
        if same_fs == Some(true) {
            let same_fs_headroom = (same_fs_output_headroom_bytes(estimate, keep_temp) as f64
                * DISK_SAFETY_FACTOR) as u64;
            eprintln!(
                "Output disk:          same filesystem as temp ({}). Phase 2 では temp を削除しながら \
                 出力するため、追加で必要な空き容量は最悪 {}{}。",
                output_parent.display(),
                format_gib(same_fs_headroom),
                if keep_temp {
                    "（--keep-temp のため output 全量ぶん）"
                } else {
                    "（処理中の最大 input partition 想定）"
                }
            );
            let same_fs_required =
                output_disk_required_bytes(estimate, true, skip_temp_check, keep_temp);
            if same_fs_required > avail {
                let msg = if skip_temp_check {
                    format!(
                        "出力ディスク不足: same filesystem 上で Phase 2 に追加 headroom {} 必要ですが \
                         {} しか空きがありません ({})。\n\
                         対処法:\n\
                         - temp/output を別ファイルシステムに分ける\n\
                         - 一時ファイルを整理してから --phase2-only を再実行\n\
                         - --force で強制続行",
                        format_gib(same_fs_headroom),
                        format_gib(avail),
                        output_parent.display(),
                    )
                } else {
                    format!(
                        "ディスク不足: same filesystem 上で Phase 1 temp {} と Phase 2 headroom {} の合計が \
                         必要ですが {} しか空きがありません ({})。\n\
                         対処法:\n\
                         - temp/output を別ファイルシステムに分ける\n\
                         - --partitions を増やして最大 partition を小さくする\n\
                         - --force で強制続行",
                        format_gib((estimate.phase1_temp_bytes as f64 * DISK_SAFETY_FACTOR) as u64),
                        format_gib(same_fs_headroom),
                        format_gib(avail),
                        output_parent.display(),
                    )
                };
                if force {
                    eprintln!("Warning (--force): {msg}");
                } else {
                    return Err(io::Error::other(msg));
                }
            }
        } else {
            eprintln!(
                "Output disk available:{} ({}, 出力上限 {})",
                format_gib(avail),
                output_parent.display(),
                format_gib(output_required),
            );
            if output_required > avail {
                let msg = format!(
                    "出力ディスク不足: 上限 {} 必要ですが {} しか空きがありません ({})。\n\
                     対処法:\n\
                     - 別ファイルシステムの --output を指定\n\
                     - --force で強制続行",
                    format_gib(output_required),
                    format_gib(avail),
                    output_parent.display(),
                );
                if force {
                    eprintln!("Warning (--force): {msg}");
                } else {
                    return Err(io::Error::other(msg));
                }
            }
        }
    }

    eprintln!();
    Ok(())
}

/// `--phase2-only` 時、既存 temp ディレクトリのパーティションファイルサイズを
/// 合計して total bytes を得る。
fn sum_existing_partition_bytes(dir: &Path) -> io::Result<u64> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// `--phase2-only` 時、既存 temp ディレクトリから partition 数を推定する。
///
/// `partition_NNNNN.bin` 形式のファイルを列挙し、最大 index + 1 を返す。
/// Phase 1 は 0..N-1 のすべての partition ファイルを空でも作成するため、
/// ファイル数ではなく最大 index を使うことで途中削除された場合も正しく
/// Phase 1 時点の N を取得できる。
fn detect_partition_count(dir: &Path) -> io::Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut max_idx: Option<usize> = None;
    let mut count = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(stripped) = name.strip_prefix("partition_").and_then(|s| s.strip_suffix(".bin"))
        else {
            continue;
        };
        let Ok(idx) = stripped.parse::<usize>() else {
            continue;
        };
        max_idx = Some(max_idx.map_or(idx, |m| m.max(idx)));
        count += 1;
    }
    match max_idx {
        Some(m) => {
            let detected = m + 1;
            if count < detected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "temp {} の partition が欠損しています: {} 個しか見つからず \
                         Phase 1 時の N={} を満たしません。`--phase2-only` は不完全な temp では再開できません。",
                        dir.display(),
                        count,
                        detected,
                    ),
                ));
            }
            Ok(detected)
        }
        None => Ok(0),
    }
}

fn partition_filename(partition: usize) -> String {
    format!("partition_{partition:05}.bin")
}

/// reference 引数をパスのベクタにパースする。
fn parse_reference_paths(reference: &str) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for s in reference.split(',') {
        let p = PathBuf::from(s.trim());
        if !p.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("reference ファイルが見つかりません: {}", p.display()),
            ));
        }
        paths.push(p);
    }
    Ok(paths)
}

/// 入力ファイル群を 1 つのサブディレクトリ下のパーティションに振り分ける。
///
/// `max_positions > 0` の場合は `total_records` がその値に達した時点で
/// 途中終了する。`max_positions == 0` なら全件処理する。
fn partition_files_into(
    label: &str,
    inputs: &[PathBuf],
    subdir: &Path,
    num_partitions: usize,
    partition_buffer_bytes: usize,
    max_positions: u64,
) -> io::Result<u64> {
    std::fs::create_dir_all(subdir)?;

    let mut writers: Vec<BufWriter<File>> = (0..num_partitions)
        .map(|i| {
            let path = subdir.join(partition_filename(i));
            let file = OpenOptions::new().create(true).write(true).truncate(true).open(&path)?;
            Ok::<_, io::Error>(BufWriter::with_capacity(partition_buffer_bytes, file))
        })
        .collect::<io::Result<Vec<_>>>()?;

    let mut total_records = 0u64;
    let mut buf = [0u8; PSV_SIZE];
    let start = std::time::Instant::now();

    'outer: for input in inputs {
        eprintln!("  [{label}] {}", input.display());
        let file = File::open(input)?;
        let meta = file.metadata()?;
        let size = meta.len();
        if !size.is_multiple_of(PSV_SIZE as u64) {
            eprintln!("Warning: {} size {size} is not a multiple of {PSV_SIZE}", input.display());
        }
        let mut reader = BufReader::with_capacity(8 << 20, file);

        loop {
            if max_positions > 0 && total_records >= max_positions {
                break 'outer;
            }

            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            let sfen: &[u8; SFEN_SIZE] = buf[..SFEN_SIZE].try_into().unwrap();
            let h = hash_packed_sfen(sfen);
            let partition = (h as usize) % num_partitions;
            writers[partition].write_all(&buf)?;

            total_records += 1;
            if total_records.is_multiple_of(100_000_000) {
                let elapsed = start.elapsed().as_secs_f64();
                let speed = total_records as f64 / elapsed / 1e6;
                eprintln!(
                    "    {:.0}M partitioned, {:.1}s ({:.1}M rec/s)",
                    total_records as f64 / 1e6,
                    elapsed,
                    speed,
                );
            }
        }
    }

    for w in &mut writers {
        w.flush()?;
    }

    let elapsed = start.elapsed().as_secs_f64();
    eprintln!("  [{label}] done: {total_records} records, {elapsed:.1}s");

    Ok(total_records)
}

/// パーティションファイルを読み、各レコードのコールバックを呼ぶ。
fn read_partition_records(
    path: &Path,
    mut on_record: impl FnMut(&[u8; PSV_SIZE], &[u8; SFEN_SIZE]) -> io::Result<()>,
) -> io::Result<u64> {
    let file = File::open(path)?;
    let meta = file.metadata()?;
    let size = meta.len();
    if size == 0 {
        return Ok(0);
    }
    if !size.is_multiple_of(PSV_SIZE as u64) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "partition {} size {size} is not a multiple of {PSV_SIZE} (破損の可能性)",
                path.display(),
            ),
        ));
    }

    let mut reader = BufReader::with_capacity(8 << 20, file);
    let mut buf = [0u8; PSV_SIZE];
    let mut count = 0u64;
    loop {
        match reader.read_exact(&mut buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let sfen: [u8; SFEN_SIZE] = buf[..SFEN_SIZE].try_into().unwrap();
        on_record(&buf, &sfen)?;
        count += 1;
    }
    Ok(count)
}

/// Phase 2: 各パーティションを HashSet で exact dedup し、出力に追記する。
///
/// `ref_subdir` が `Some` ならパーティション先頭で reference を HashSet に
/// ロードし（出力はしない）、続けて input パーティションを照合する。
///
/// 戻り値は `(reference_records, input_records_seen, unique_output_records)`。
fn deduplicate_partitions(
    input_subdir: &Path,
    ref_subdir: Option<&Path>,
    num_partitions: usize,
    output_path: &Path,
    keep_temp: bool,
) -> io::Result<(u64, u64, u64)> {
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
    {
        std::fs::create_dir_all(parent)?;
    }

    let out_file = File::create(output_path)?;
    let mut writer = BufWriter::with_capacity(16 << 20, out_file);

    let mut total_ref = 0u64;
    let mut total_seen = 0u64;
    let mut total_unique = 0u64;
    let start = std::time::Instant::now();

    for partition in 0..num_partitions {
        let mut seen: HashSet<[u8; SFEN_SIZE]> = HashSet::new();

        // --- Phase 2a: reference partition を HashSet にロード（出力しない） ---
        if let Some(ref_dir) = ref_subdir {
            let ref_path = ref_dir.join(partition_filename(partition));
            if !ref_path.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "reference partition が見つかりません: {} (temp ディレクトリ破損の可能性)",
                        ref_path.display()
                    ),
                ));
            }
            let ref_records = read_partition_records(&ref_path, |_rec, sfen| {
                seen.insert(*sfen);
                Ok(())
            })?;
            total_ref += ref_records;
            if !keep_temp {
                let _ = std::fs::remove_file(&ref_path);
            }
        }

        // --- Phase 2b: input partition を streaming し、未登録なら出力 ---
        let input_path = input_subdir.join(partition_filename(partition));
        if !input_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "input partition が見つかりません: {} (temp ディレクトリ破損の可能性)",
                    input_path.display()
                ),
            ));
        }

        let mut unique_in_partition = 0u64;
        let input_records = read_partition_records(&input_path, |rec, sfen| {
            if seen.insert(*sfen) {
                writer.write_all(rec)?;
                unique_in_partition += 1;
            }
            Ok(())
        })?;

        total_seen += input_records;
        total_unique += unique_in_partition;

        drop(seen);
        if !keep_temp {
            let _ = std::fs::remove_file(&input_path);
        }

        if partition.is_multiple_of(64) || partition + 1 == num_partitions {
            let elapsed = start.elapsed().as_secs_f64();
            eprintln!(
                "  partition {}/{}  ref={:.1}M seen={:.1}M unique={:.1}M  {:.1}s",
                partition + 1,
                num_partitions,
                total_ref as f64 / 1e6,
                total_seen as f64 / 1e6,
                total_unique as f64 / 1e6,
                elapsed,
            );
        }
    }

    writer.flush()?;
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!(
        "Phase 2 done: {total_unique} unique / {total_seen} input records (ref loaded: {total_ref}), {elapsed:.1}s",
    );

    Ok((total_ref, total_seen, total_unique))
}

fn warn_fd_limit(num_partitions: usize) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit は POSIX 標準で、rlimit 構造体への書き込みのみ行う。
    // 失敗時は戻り値が -1 になるだけで副作用はない。
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    if rc != 0 {
        return;
    }
    let soft = limit.rlim_cur as usize;
    // Phase 1 で同時に開く fd ≈ num_partitions + 入力 1 + stderr/stdout など余裕 16
    let required = num_partitions + 32;
    if soft < required {
        eprintln!(
            "Warning: RLIMIT_NOFILE soft limit = {soft}, but {required} fd needed for {num_partitions} partitions. \
            `ulimit -n {required}` で引き上げてから再実行してください。",
        );
    }
}

/// temp_dir を掃除する（空ディレクトリなら削除）。
fn cleanup_if_empty(dir: &Path) -> io::Result<()> {
    if dir.is_dir() && std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    if args.partitions == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--partitions は 1 以上を指定してください",
        ));
    }
    if args.partition_buffer_kb == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--partition-buffer-kb は 1 以上を指定してください",
        ));
    }

    let partition_buffer_bytes = args.partition_buffer_kb * 1024;
    let input_subdir = args.temp_dir.join(INPUT_SUBDIR);
    let ref_subdir = args.temp_dir.join(REF_SUBDIR);

    // Phase 2 で使う partition 数。--phase2-only 時は既存 temp dir から自動検出して
    // Phase 1 時の N と一致させる (データ欠損を防ぐ)。
    let partitions: usize;

    let has_reference_partitions: bool;

    let (phase1_ref_records, phase1_input_records) = if args.phase2_only {
        eprintln!("=== Phase 1 skipped (--phase2-only) ===");
        if !args.temp_dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("一時ディレクトリが存在しません: {}", args.temp_dir.display()),
            ));
        }
        if !input_subdir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "入力パーティションが見つかりません: {} (Phase 1 を先に実行してください)",
                    input_subdir.display(),
                ),
            ));
        }

        let detected = detect_partition_count(&input_subdir)?;
        if detected == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "partition ファイルが見つかりません: {} (ファイル名形式 partition_NNNNN.bin)",
                    input_subdir.display(),
                ),
            ));
        }
        if detected != args.partitions {
            eprintln!(
                "Info: --phase2-only で {} から {} 個の partition を検出しました \
                 (CLI の --partitions={} は上書きされます)。",
                input_subdir.display(),
                detected,
                args.partitions,
            );
        }
        partitions = detected;
        warn_fd_limit(partitions);

        if ref_subdir.is_dir() {
            let ref_detected = detect_partition_count(&ref_subdir)?;
            if ref_detected != 0 && ref_detected != partitions {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "partition 数の不一致: input={partitions}, ref={ref_detected}. \
                         temp ディレクトリが壊れている可能性があります。",
                    ),
                ));
            }
            has_reference_partitions = ref_detected != 0;
            if has_reference_partitions {
                eprintln!("  reference パーティション検出: {}", ref_subdir.display());
            }
        } else {
            has_reference_partitions = false;
        }

        let existing_input_bytes = sum_existing_partition_bytes(&input_subdir)?;
        let existing_ref_bytes = sum_existing_partition_bytes(&ref_subdir)?;
        let estimate = estimate_resources(
            existing_ref_bytes,
            existing_input_bytes,
            partitions,
            partition_buffer_bytes,
        );
        preflight_check(
            &estimate,
            &args.temp_dir,
            &args.output,
            /* skip_temp_check = */ true,
            args.keep_temp,
            args.force,
        )?;

        (0u64, 0u64)
    } else {
        partitions = args.partitions;
        warn_fd_limit(partitions);

        let ref_paths = match args.reference.as_deref() {
            Some(r) => parse_reference_paths(r)?,
            None => Vec::new(),
        };

        let inputs =
            collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)?;
        if inputs.is_empty() {
            eprintln!("入力ファイルが見つかりません");
            return Ok(());
        }
        check_output_not_in_inputs(&args.output, &inputs)?;
        check_output_not_in_inputs(&args.output, &ref_paths)?;

        let ref_size = sum_file_sizes(&ref_paths)?;
        let input_size = sum_file_sizes(&inputs)?;
        let capped_input_size = if args.max_positions > 0 {
            input_size.min(args.max_positions.saturating_mul(PSV_SIZE as u64))
        } else {
            input_size
        };
        let estimate =
            estimate_resources(ref_size, capped_input_size, partitions, partition_buffer_bytes);
        preflight_check(
            &estimate,
            &args.temp_dir,
            &args.output,
            /* skip_temp_check = */ false,
            args.keep_temp,
            args.force,
        )?;

        eprintln!("=== Phase 1: Partitioning ({partitions} partitions) ===");
        eprintln!(
            "  partition buffer: {} KiB/partition (total ~{:.1} MiB)",
            args.partition_buffer_kb,
            (partitions * partition_buffer_bytes) as f64 / (1024.0 * 1024.0),
        );

        let ref_records = if ref_paths.is_empty() {
            // 過去の reference 残骸が残っていたら掃除しておく
            if ref_subdir.is_dir() {
                std::fs::remove_dir_all(&ref_subdir)?;
            }
            has_reference_partitions = false;
            0
        } else {
            has_reference_partitions = true;
            partition_files_into(
                "reference",
                &ref_paths,
                &ref_subdir,
                partitions,
                partition_buffer_bytes,
                0, // reference は常に全件
            )?
        };

        let input_records = partition_files_into(
            "input",
            &inputs,
            &input_subdir,
            partitions,
            partition_buffer_bytes,
            args.max_positions,
        )?;

        (ref_records, input_records)
    };

    let ref_dir_opt = if has_reference_partitions && ref_subdir.is_dir() {
        Some(ref_subdir.as_path())
    } else {
        None
    };

    eprintln!(
        "\n=== Phase 2: Deduplication{} ===",
        if ref_dir_opt.is_some() {
            " (with reference)"
        } else {
            ""
        }
    );
    let (ref_seen, input_seen, unique) = deduplicate_partitions(
        &input_subdir,
        ref_dir_opt,
        partitions,
        &args.output,
        args.keep_temp,
    )?;

    if !args.keep_temp {
        cleanup_if_empty(&input_subdir)?;
        if ref_dir_opt.is_some() {
            cleanup_if_empty(&ref_subdir)?;
        }
        cleanup_if_empty(&args.temp_dir)?;
    }

    let duplicates = input_seen.saturating_sub(unique);
    let dup_pct = if input_seen > 0 {
        100.0 * duplicates as f64 / input_seen as f64
    } else {
        0.0
    };
    println!("=== Partition Dedup Summary ===");
    if !args.phase2_only {
        if phase1_ref_records > 0 {
            println!("Reference records: {phase1_ref_records}");
        }
        println!("Input records:     {phase1_input_records}");
    }
    if ref_seen > 0 {
        println!("Reference seen:    {ref_seen} (Phase 2 でロード)");
    }
    println!("Input seen:        {input_seen} (Phase 2 入力)");
    println!(
        "Output records:    {unique} ({:.2}%)",
        100.0 * unique as f64 / input_seen.max(1) as f64,
    );
    println!("Duplicates:        {duplicates} ({dup_pct:.2}%)");
    println!("Output file:       {}", args.output.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn same_fs_headroom_without_keep_temp_is_peak_partition_only() {
        let estimate = estimate_resources(400, 4_000, 4, 64 * 1024);

        assert_eq!(same_fs_output_headroom_bytes(&estimate, false), 1_200);
    }

    #[test]
    fn same_fs_headroom_with_keep_temp_is_full_output() {
        let estimate = estimate_resources(400, 4_000, 4, 64 * 1024);

        assert_eq!(same_fs_output_headroom_bytes(&estimate, true), 4_000);
    }

    #[test]
    fn phase2_only_same_fs_uses_headroom_not_full_output() {
        let estimate = estimate_resources(400, 4_000, 4, 64 * 1024);

        assert_eq!(output_disk_required_bytes(&estimate, true, true, false), 1_260);
        assert_eq!(output_disk_required_bytes(&estimate, false, true, false), 4_200);
    }

    #[test]
    fn detect_partition_count_rejects_missing_partition() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(partition_filename(0)), []).unwrap();
        std::fs::write(dir.path().join(partition_filename(2)), []).unwrap();

        let err = detect_partition_count(dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("欠損"));
    }
}
