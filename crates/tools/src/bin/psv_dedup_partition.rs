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
///     --dedup-only
///
///   # Phase 1 (パーティション振り分け) だけを行い、入力ファイルを 1 つずつ
///   # 削除しながら処理することで、入力サイズの 2 倍の空きを必要としない。
///   # `--temp-dir` の partition ファイルには append モードで追記される。
///   for f in /data/*.bin; do
///     cargo run --release --bin psv_dedup_partition -- \
///       --partition-only --input "$f" \
///       --temp-dir /path/to/tmp --partitions 1024
///     rm "$f"
///   done
///   cargo run --release --bin psv_dedup_partition -- \
///     --dedup-only --output /path/to/deduped.bin --temp-dir /path/to/tmp
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

    /// 出力ファイルパス。`--partition-only` 時は不要。それ以外は必須。
    #[arg(long)]
    output: Option<PathBuf>,

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

    /// Phase 1 (パーティション振り分け) をスキップして既存の一時ファイルから
    /// Phase 2 (重複削除) のみ実行する。temp_dir/ref/ が存在すれば自動で
    /// reference モードになる。`--partition-only` と排他。
    #[arg(long)]
    dedup_only: bool,

    /// Phase 2 をスキップし、Phase 1 (パーティション振り分け) のみを実行する。
    /// 既存の partition ファイルがあれば追記モードで書き込むため、入力ファイルを
    /// 1 つずつ振り分け→元ファイル削除を繰り返すことで、入力と同サイズの一時領域を
    /// 一度に確保せずに済む。最後に `--dedup-only` で Phase 2 を実行すること。
    /// `--keep-temp` は暗黙で有効になり、`--output` は不要。`--dedup-only` と排他。
    #[arg(long)]
    partition_only: bool,

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
///
/// `output_path` が `None` の場合（`--partition-only` モード）は出力ディスクチェックを
/// 全てスキップする。Phase 2 を実行しないため、出力先の空き容量は問題にならない。
fn preflight_check(
    estimate: &ResourceEstimate,
    temp_dir: &Path,
    output_path: Option<&Path>,
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
    let output_parent = output_path.map(|p| {
        let parent = p.parent().unwrap_or(Path::new("."));
        if parent.as_os_str().is_empty() {
            Path::new(".").to_path_buf()
        } else {
            parent.to_path_buf()
        }
    });

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

    // 出力ディスクチェック（--partition-only 時はスキップ）
    if let Some(output_parent) = output_parent.as_deref() {
        let same_fs = same_filesystem(temp_dir, output_parent);
        // 出力上限は input 側のみ（reference は出力対象外なので除外する）
        let output_required =
            (estimate.output_upper_bound_bytes as f64 * DISK_SAFETY_FACTOR) as u64;
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
                             - 一時ファイルを整理してから --dedup-only を再実行\n\
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
                            format_gib(
                                (estimate.phase1_temp_bytes as f64 * DISK_SAFETY_FACTOR) as u64
                            ),
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
    }

    eprintln!();
    Ok(())
}

/// `--dedup-only` 時、既存 temp ディレクトリのパーティションファイルサイズを
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

/// `--dedup-only` 時、既存 temp ディレクトリから partition 数を推定する。
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
                         Phase 1 時の N={} を満たしません。`--dedup-only` は不完全な temp では再開できません。",
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
///
/// `append = true` の場合、既存の partition ファイルがあれば末尾に追記する。
/// `--partition-only` で複数回起動して同じ temp_dir に積み重ねるユースケース向け。
fn partition_files_into(
    label: &str,
    inputs: &[PathBuf],
    subdir: &Path,
    num_partitions: usize,
    partition_buffer_bytes: usize,
    max_positions: u64,
    append: bool,
) -> io::Result<u64> {
    std::fs::create_dir_all(subdir)?;

    let mut writers: Vec<BufWriter<File>> = (0..num_partitions)
        .map(|i| {
            let path = subdir.join(partition_filename(i));
            let mut opts = OpenOptions::new();
            opts.create(true).write(true);
            if append {
                opts.append(true);
            } else {
                opts.truncate(true);
            }
            let file = opts.open(&path)?;
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

    // BufWriter の Drop は flush エラーを握りつぶすので、明示的に into_inner で
    // 内部 File を取り出して flush エラーを伝播させる。append モードで部分書き込みが
    // 残ると、次回起動の `--dedup-only` が壊れた temp として扱えず誤検出になる。
    for w in writers {
        // `IntoInnerError -> io::Error` の From 実装で flush 失敗を伝播させる。
        let mut file = w.into_inner()?;
        file.flush()?;
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

#[cfg(unix)]
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

// Windows 等の非 Unix 環境には `RLIMIT_NOFILE` 相当のソフトリミットが存在しないため、
// fd 上限の事前警告は no-op とする（CRT の `_setmaxstdio` はストリーム上限で別概念）。
#[cfg(not(unix))]
fn warn_fd_limit(_: usize) {}

/// temp_dir を掃除する（空ディレクトリなら削除）。
fn cleanup_if_empty(dir: &Path) -> io::Result<()> {
    if dir.is_dir() && std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(())
}

/// `partition_NNNNN.bin` 形式の **非空** ファイルが 1 つでも存在するか判定する。
///
/// `partition_files_into` は処理開始時に 0..N-1 の空 partition ファイルを一括作成し、
/// その後で入力 (reference) ファイルを open/read する。reference の open/read が
/// 失敗すると ref/ には空の partition ファイルだけが残るため、ファイル名だけで
/// 判定すると「過去失敗の残骸」と「reference 取り込み済み」を区別できない。
///
/// サイズ > 0 のファイルが 1 つでもあれば「データが書き込まれたことがある」=
/// 取り込み済みとみなす。空ファイルだけが残っている場合は失敗からのリトライを
/// 許容する (append モードで空ファイルに書き込むのは新規作成と等価)。
fn has_any_partition_file(dir: &Path) -> io::Result<bool> {
    if !dir.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.strip_prefix("partition_").and_then(|s| s.strip_suffix(".bin")).is_none() {
            continue;
        }
        if entry.metadata()?.len() > 0 {
            return Ok(true);
        }
    }
    Ok(false)
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
    if args.partition_only && args.dedup_only {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--partition-only と --dedup-only は同時に指定できません",
        ));
    }
    if !args.partition_only && args.output.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--output は必須です（--partition-only モードでのみ省略可能）",
        ));
    }
    if args.partition_only && args.output.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--partition-only モードでは --output は使えません（Phase 2 を実行しないため）",
        ));
    }
    if args.dedup_only && (args.input.is_some() || args.input_dir.is_some()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--dedup-only モードでは --input / --input-dir は使えません（既存の一時ファイルから再開するため）",
        ));
    }

    let partition_buffer_bytes = args.partition_buffer_kb * 1024;
    let input_subdir = args.temp_dir.join(INPUT_SUBDIR);
    let ref_subdir = args.temp_dir.join(REF_SUBDIR);
    // --partition-only では temp を消したら意味がない（次回起動で再利用するため）。
    let keep_temp = args.keep_temp || args.partition_only;

    if args.partition_only {
        return run_partition_only(
            &args,
            &input_subdir,
            &ref_subdir,
            partition_buffer_bytes,
            keep_temp,
        );
    }

    // 以降は通常モード or --dedup-only。Phase 2 を実行するので --output は必須。
    let output_path = args.output.as_ref().expect("output 必須は上で検証済み");

    // Phase 2 で使う partition 数。--dedup-only 時は既存 temp dir から自動検出して
    // Phase 1 時の N と一致させる (データ欠損を防ぐ)。
    let partitions: usize;
    let has_reference_partitions: bool;

    let (phase1_ref_records, phase1_input_records) = if args.dedup_only {
        eprintln!("=== Phase 1 skipped (--dedup-only) ===");
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
                "Info: --dedup-only で {} から {} 個の partition を検出しました \
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
            Some(output_path),
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
        check_output_not_in_inputs(output_path, &inputs)?;
        check_output_not_in_inputs(output_path, &ref_paths)?;

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
            Some(output_path),
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
                false,
            )?
        };

        let input_records = partition_files_into(
            "input",
            &inputs,
            &input_subdir,
            partitions,
            partition_buffer_bytes,
            args.max_positions,
            false,
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
        output_path,
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
    if !args.dedup_only {
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
    println!("Output file:       {}", output_path.display());

    Ok(())
}

/// `--partition-only` モードの実行ハンドラ。
///
/// Phase 1 のみを実行し、既存の partition があれば追記する。Phase 2 (重複削除) は
/// 行わないため、入力ファイルを 1 つずつ振り分け→元ファイル削除を繰り返すことで、
/// 入力と同サイズの一時領域を一度に確保せずに済む。
fn run_partition_only(
    args: &Args,
    input_subdir: &Path,
    ref_subdir: &Path,
    partition_buffer_bytes: usize,
    keep_temp: bool,
) -> io::Result<()> {
    // ---- 早期検証 (重い I/O 前に全て返す) ----
    let ref_paths = match args.reference.as_deref() {
        Some(r) => parse_reference_paths(r)?,
        None => Vec::new(),
    };

    // ref_subdir に reference データが書き込まれている状態で --reference を再指定すると、
    // 同じ参照集合を二重登録するか、別集合と混ざって意味が変わる。どちらもユーザの
    // 意図に反するので即エラー。ただし `has_any_partition_file` はサイズ > 0 のファイルだけ
    // カウントするため、過去の失敗で空 partition ファイルだけが残っているケース（reference の
    // open/read 失敗等）は失敗とみなしてリトライを許容する。
    if !ref_paths.is_empty() && has_any_partition_file(ref_subdir)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} に reference partition データが既に書き込まれています。--partition-only で \
                 --reference を指定できるのは初回のみです。\n\
                 - 続けて入力を追加するだけなら --reference を外してください。\n\
                 - 過去の中途失敗から復旧したい場合は ref/ を手動で削除してから再実行してください \
                   (部分書き込みの append は二重登録になるため自動復旧はしません)。",
                ref_subdir.display(),
            ),
        ));
    }

    let inputs =
        collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)?;
    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "入力ファイルが見つかりません (--partition-only には --input または --input-dir が必須)",
        ));
    }

    // 既存 partition があれば数を揃える。不一致なら即エラー (ハッシュ空間が変わると
    // 過去ファイルとの整合が取れなくなりデータが壊れる)。
    let existing_input_count = detect_partition_count(input_subdir)?;
    let existing_ref_count = detect_partition_count(ref_subdir)?;
    if existing_input_count != 0 && existing_input_count != args.partitions {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "--partitions={} ですが {} には {} 個の partition が既にあります。\
                 過去の起動と同じ値を指定するか、temp ディレクトリを別にしてください。",
                args.partitions,
                input_subdir.display(),
                existing_input_count,
            ),
        ));
    }
    if existing_ref_count != 0 && existing_ref_count != args.partitions {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "--partitions={} ですが {} には {} 個の reference partition があります。",
                args.partitions,
                ref_subdir.display(),
                existing_ref_count,
            ),
        ));
    }
    // input と ref の N 不一致は temp が壊れているか、過去に違う --partitions で
    // 作られたものが混在している。修復は人間にしかできないので即エラー。
    if existing_input_count != 0
        && existing_ref_count != 0
        && existing_input_count != existing_ref_count
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} の input partition 数 ({}) と reference partition 数 ({}) が不一致です。\
                 temp ディレクトリが壊れている可能性があります。",
                args.temp_dir.display(),
                existing_input_count,
                existing_ref_count,
            ),
        ));
    }

    let partitions = args.partitions;
    warn_fd_limit(partitions);

    // 出力ディスクは見ない。今回追加で必要な temp サイズだけ見積もる。
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
        /* output_path = */ None,
        /* skip_temp_check = */ false,
        keep_temp,
        args.force,
    )?;

    eprintln!(
        "=== Phase 1 only: Partitioning ({partitions} partitions, {} mode) ===",
        if existing_input_count > 0 {
            "append"
        } else {
            "create"
        }
    );
    eprintln!(
        "  partition buffer: {} KiB/partition (total ~{:.1} MiB)",
        args.partition_buffer_kb,
        (partitions * partition_buffer_bytes) as f64 / (1024.0 * 1024.0),
    );

    let ref_records = if ref_paths.is_empty() {
        0
    } else {
        partition_files_into(
            "reference",
            &ref_paths,
            ref_subdir,
            partitions,
            partition_buffer_bytes,
            0,
            true,
        )?
    };

    let input_records = partition_files_into(
        "input",
        &inputs,
        input_subdir,
        partitions,
        partition_buffer_bytes,
        args.max_positions,
        true,
    )?;

    let total_input_bytes = sum_existing_partition_bytes(input_subdir)?;
    let total_ref_bytes = sum_existing_partition_bytes(ref_subdir)?;

    println!("=== Partition Only Summary ===");
    if ref_records > 0 {
        println!("Reference records (this run): {ref_records}");
    }
    println!("Input records (this run):     {input_records}");
    println!(
        "Cumulative temp size:         input={} ref={}",
        format_gib(total_input_bytes),
        format_gib(total_ref_bytes)
    );
    println!("Temp dir:                     {}", args.temp_dir.display());
    println!();
    println!("次のステップ:");
    println!("  - 残りの入力ファイルがあれば同じコマンドを繰り返してください");
    println!(
        "  - 全件の振り分けが終わったら以下で Phase 2 を実行してください:\n\
         \n\
         \t--dedup-only --output <出力ファイル> --temp-dir {} --partitions {}",
        args.temp_dir.display(),
        partitions,
    );

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
    fn dedup_only_same_fs_uses_headroom_not_full_output() {
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

    #[test]
    fn has_any_partition_file_distinguishes_empty_and_populated() {
        let dir = TempDir::new().unwrap();
        assert!(!has_any_partition_file(dir.path()).unwrap());

        // partition と無関係な名前のファイルは無視
        std::fs::write(dir.path().join("not_a_partition.bin"), []).unwrap();
        assert!(!has_any_partition_file(dir.path()).unwrap());

        // 空の partition ファイルは「過去失敗の残骸」とみなして false (再試行を許容)
        std::fs::write(dir.path().join(partition_filename(0)), []).unwrap();
        std::fs::write(dir.path().join(partition_filename(1)), []).unwrap();
        assert!(!has_any_partition_file(dir.path()).unwrap());

        // 1 バイトでもデータがあれば「取り込み済み」とみなして true
        std::fs::write(dir.path().join(partition_filename(0)), b"x").unwrap();
        assert!(has_any_partition_file(dir.path()).unwrap());
    }

    /// `--partition-only` を 2 回繰り返した結果が、1 回の通常 Phase1 と同等になる。
    /// (append モードが正しく動き、ハッシュ振り分けが安定していることを確認)
    #[test]
    fn partition_only_append_matches_single_pass() {
        // 4 種類の PSV レコード (sfen 部分だけ違えば良い)
        let make_psv = |seed: u8| -> [u8; PSV_SIZE] {
            let mut buf = [0u8; PSV_SIZE];
            for (i, b) in buf.iter_mut().enumerate() {
                *b = seed.wrapping_add(i as u8);
            }
            buf
        };
        let recs: Vec<[u8; PSV_SIZE]> = (0..16u8).map(make_psv).collect();

        // ファイル A, B に 8 件ずつ書く
        let workdir = TempDir::new().unwrap();
        let file_a = workdir.path().join("a.bin");
        let file_b = workdir.path().join("b.bin");
        {
            let mut f = File::create(&file_a).unwrap();
            for r in &recs[..8] {
                f.write_all(r).unwrap();
            }
            let mut f = File::create(&file_b).unwrap();
            for r in &recs[8..] {
                f.write_all(r).unwrap();
            }
        }

        // 経路 1: 1 回 Phase1 で a, b を一度に振り分け (truncate モード)
        let single_dir = TempDir::new().unwrap();
        let single_subdir = single_dir.path().join(INPUT_SUBDIR);
        partition_files_into(
            "input",
            &[file_a.clone(), file_b.clone()],
            &single_subdir,
            4,
            64 * 1024,
            0,
            false,
        )
        .unwrap();

        // 経路 2: --partition-only 風に a, b を別々に append
        let multi_dir = TempDir::new().unwrap();
        let multi_subdir = multi_dir.path().join(INPUT_SUBDIR);
        partition_files_into(
            "input",
            std::slice::from_ref(&file_a),
            &multi_subdir,
            4,
            64 * 1024,
            0,
            true,
        )
        .unwrap();
        partition_files_into(
            "input",
            std::slice::from_ref(&file_b),
            &multi_subdir,
            4,
            64 * 1024,
            0,
            true,
        )
        .unwrap();

        // 各 partition のバイト列が完全一致すること
        for i in 0..4 {
            let single = std::fs::read(single_subdir.join(partition_filename(i))).unwrap();
            let multi = std::fs::read(multi_subdir.join(partition_filename(i))).unwrap();
            assert_eq!(single, multi, "partition {i} mismatch between single-pass and append");
        }
    }
}
