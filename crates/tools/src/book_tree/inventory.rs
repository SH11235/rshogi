//! 読み終わった木のうち、定跡の元データに登録していない出力先を一覧にする。

use super::audit::{reject_same_file, resolve_output};
use super::output::NODE_MOVES;
use super::register::{is_done, reading_files};
use super::transfer::sec_bits;
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

const REGISTRATION_STATUS: &str = "registration_status.json";
const CANDIDATE: &str = "candidate";
const REGISTERED: &str = "registered";

/// 外した読みを 1 回に表へ載せる件数 (約 4 MiB)。一覧がこれより長ければ回を分けて照合する。
const EXCLUDED_CHUNK: usize = 262_144;

#[derive(Args, Debug)]
pub(super) struct InventoryArgs {
    /// 木の出力先を探す根。この下の registration_status.json を持つディレクトリを出力先として数える (繰り返し指定)
    #[arg(long = "root", required = true, value_name = "DIR")]
    roots: Vec<PathBuf>,
    /// 登録済みの出力先を 1 行に 1 つ書いたファイル。リンクや別名を解決した実体のパスで照合する
    #[arg(long, value_name = "FILE")]
    registered: Option<PathBuf>,
    /// 外した読みの一覧 (JSON Lines)。key・nodes・sec が全部一致する読みを持つ木を外す
    #[arg(long, value_name = "FILE")]
    excluded_readings: Option<PathBuf>,
    /// 木の id がこの接頭辞で始まる出力先を外す (繰り返し指定)
    #[arg(long, value_name = "PREFIX")]
    exclude_prefix: Vec<String>,
    /// 根の局面しか読んでいない木 (d1.jsonl が無い) も候補に残す
    #[arg(long)]
    keep_depth0: bool,
    /// 登録に渡す出力先の一覧 (1 行に 1 つ) の書き出し先
    #[arg(long, value_name = "FILE")]
    out: PathBuf,
    /// 全部の出力先の判定の表 (TSV) の書き出し先
    #[arg(long, value_name = "FILE")]
    table: PathBuf,
}

/// 読みの key・nodes・sec の SHA-256 の先頭 16 バイト。
type ReadingId = [u8; 16];
/// 一時ファイルの 1 行: 読みの照合の値と、出力先の番号。
const ROW_BYTES: usize = 20;

#[derive(Deserialize)]
struct ExcludedLine {
    key: String,
    nodes: u64,
    sec: f64,
}

#[derive(Deserialize)]
struct ReadingLine {
    key: String,
    nodes: Option<u64>,
    sec: f64,
}

struct Filters {
    registered: HashSet<PathBuf>,
    prefixes: Vec<String>,
    keep_depth0: bool,
}

#[derive(Debug, PartialEq)]
struct Entry {
    path: String,
    id: String,
    own: String,
    generation: String,
    place: String,
    verdict: String,
    detail: String,
}

pub(super) fn run(args: InventoryArgs) -> Result<()> {
    let mut inputs = Vec::new();
    inputs.extend(args.registered.as_deref().map(|p| ("--registered", p)));
    inputs.extend(args.excluded_readings.as_deref().map(|p| ("--excluded-readings", p)));
    reject_same_file(&args.out, &inputs)?;
    reject_same_file(&args.table, &inputs)?;
    ensure!(
        resolve_output(&args.out)? != resolve_output(&args.table)?,
        "--out と --table は別のファイルにしてください"
    );
    let (registered, missing) = match &args.registered {
        Some(path) => read_registered(path)?,
        None => Default::default(),
    };
    let filters = Filters {
        registered,
        prefixes: args.exclude_prefix,
        keep_depth0: args.keep_depth0,
    };
    let (mut entries, unmatched) = collect(&args.roots, &filters)?;
    if let Some(list) = &args.excluded_readings {
        let tmp = args
            .table
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        apply_excluded(&mut entries, list, tmp, EXCLUDED_CHUNK)?;
    }
    mark_copies(&mut entries);
    let mut list = String::new();
    let mut table = String::from("tree\tid\town\tgeneration\tplace\tverdict\tdetail\n");
    let mut counts = BTreeMap::<&str, usize>::new();
    for e in &entries {
        if e.verdict == CANDIDATE {
            writeln!(list, "{}", e.path)?;
        }
        writeln!(
            table,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            e.path,
            e.id,
            e.own,
            e.generation,
            e.place,
            e.verdict,
            e.detail.replace(['\t', '\r', '\n'], " ")
        )?;
        *counts.entry(&e.verdict).or_default() += 1;
    }
    write_atomic(&args.out, &list)?;
    write_atomic(&args.table, &table)?;
    println!("木の出力先 {}", entries.len());
    for (verdict, count) in counts {
        println!("{verdict} {count}");
    }
    if args.registered.is_some() {
        println!("登録済みの一覧のうち根の下に無かったパス {}", unmatched + missing);
    }
    Ok(())
}

/// 実体のパスの集合と、解決できなかった行の数。
fn read_registered(path: &Path) -> Result<(HashSet<PathBuf>, usize)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{} を読めません", path.display()))?;
    let mut set = HashSet::new();
    let mut missing = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match Path::new(line).canonicalize() {
            Ok(real) => {
                set.insert(real);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing += 1,
            Err(e) => return Err(e).with_context(|| format!("{line} を解決できません")),
        }
    }
    Ok((set, missing))
}

/// パス順の出力先と、登録済みの一覧のうち根の下で見つからなかった数。
fn collect(roots: &[PathBuf], filters: &Filters) -> Result<(Vec<Entry>, usize)> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for root in roots {
        ensure!(
            root.is_dir(),
            "木の出力先を探す根がディレクトリではありません: {}",
            root.display()
        );
        let mut walk = walkdir::WalkDir::new(root).sort_by_file_name().into_iter();
        while let Some(item) = walk.next() {
            let item = item?;
            if !item.file_type().is_dir() || !item.path().join(REGISTRATION_STATUS).is_file() {
                continue;
            }
            // 出力先は入れ子にならないので、中の読みや DB を辿らない。
            walk.skip_current_dir();
            let real = item.path().canonicalize()?;
            if !seen.insert(real.clone()) {
                continue;
            }
            let path = item
                .path()
                .to_str()
                .with_context(|| format!("UTF-8 でないパスです: {}", item.path().display()))?;
            ensure!(!path.contains(['\t', '\r', '\n']), "タブや改行を含むパスです: {path:?}");
            let (id, own, generation) = layout(&real);
            let (verdict, detail) = classify(item.path(), &real, &id, filters);
            entries.push(Entry {
                path: path.to_owned(),
                place: place(root, item.path()),
                id,
                own,
                generation,
                verdict,
                detail,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let unmatched = filters.registered.iter().filter(|p| !seen.contains(*p)).count();
    Ok((entries, unmatched))
}

/// `<id>/<own>/g<世代>` の並びから id・own・世代を取る。並びが違えば `-`。
fn layout(real: &Path) -> (String, String, String) {
    let name =
        |p: Option<&Path>| p.and_then(Path::file_name).and_then(|n| n.to_str()).map(str::to_owned);
    let generation = name(Some(real)).filter(|g| {
        g.strip_prefix('g')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    let own = name(real.parent());
    let id = name(real.parent().and_then(Path::parent));
    match (id, own, generation) {
        (Some(id), Some(own), Some(generation)) => (id, own, generation),
        _ => ("-".into(), "-".into(), "-".into()),
    }
}

/// 根から `<id>/<own>/g<世代>` の手前までの相対パス。複数の台の写しを 1 つの根に並べたときに台を見分ける。
fn place(root: &Path, dir: &Path) -> String {
    let rel: Vec<_> = dir
        .strip_prefix(root)
        .map(|r| r.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    if rel.len() > 3 {
        rel[..rel.len() - 3].join("/")
    } else {
        "-".into()
    }
}

/// 外した読みと写しの照合より前の判定。
fn classify(dir: &Path, real: &Path, id: &str, filters: &Filters) -> (String, String) {
    if filters.registered.contains(real) {
        return (REGISTERED.into(), String::new());
    }
    let status: serde_json::Value = match std::fs::read(dir.join(REGISTRATION_STATUS))
        .map_err(anyhow::Error::from)
        .and_then(|bytes| Ok(serde_json::from_slice(&bytes)?))
    {
        Ok(status) => status,
        Err(e) => return ("bad-status-json".into(), e.to_string()),
    };
    if status.get("ready").and_then(serde_json::Value::as_bool) != Some(true) {
        return ("not-ready".into(), String::new());
    }
    match std::fs::read_to_string(dir.join("STATUS")) {
        Ok(text) if is_done(&text) => {}
        Ok(text) => {
            let word = text.split_whitespace().next().unwrap_or("");
            return (format!("status:{word}"), format!("{text:?}"));
        }
        Err(e) => return ("status:".into(), e.to_string()),
    }
    if !dir.join(NODE_MOVES).is_file() {
        return ("no-node-moves".into(), String::new());
    }
    if let Some(prefix) = filters.prefixes.iter().find(|p| id.starts_with(p.as_str())) {
        return ("prefix".into(), prefix.clone());
    }
    if !filters.keep_depth0 && !dir.join("d1.jsonl").is_file() {
        return ("depth0".into(), String::new());
    }
    (CANDIDATE.into(), String::new())
}

fn reading_id(key: &str, nodes: u64, sec: f64) -> ReadingId {
    let mut hash = Sha256::new();
    hash.update(key.as_bytes());
    hash.update(nodes.to_le_bytes());
    hash.update(sec_bits(sec).to_le_bytes());
    hash.finalize()[..16].try_into().expect("16 バイト")
}

/// 候補の木の読みを外した読みの一覧と照合し、判定と detail を書き換える。
///
/// 木の読みは照合の値だけを `tmp` の一時ファイルに並べ、一覧は `chunk` 件ずつ表に載せて一時ファイルを
/// 1 回ずつ走査する。メモリは一覧の行数にも木の読みの行数にも比例しない。
fn apply_excluded(entries: &mut [Entry], list: &Path, tmp: &Path, chunk: usize) -> Result<()> {
    let mut rows = BufWriter::new(tempfile::tempfile_in(tmp)?);
    let mut readings = vec![0usize; entries.len()];
    let mut matched = vec![0usize; entries.len()];
    for (index, entry) in entries.iter_mut().enumerate().filter(|(_, e)| e.verdict == CANDIDATE) {
        match write_readings(Path::new(&entry.path), u32::try_from(index)?, &mut rows) {
            Ok(count) => readings[index] = count,
            // 読めない行を飛ばすと、外した読みを含む木を候補に残しうる。
            Err(e) => {
                entry.verdict = "bad-readings".into();
                entry.detail = format!("{e:#}");
            }
        }
    }
    let mut rows = rows.into_inner()?;
    let file = File::open(list).with_context(|| format!("{} を読めません", list.display()))?;
    let mut lines = BufReader::new(file).lines().enumerate();
    loop {
        let mut table = Vec::new();
        while table.len() < chunk
            && let Some((index, line)) = lines.next()
        {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let row: ExcludedLine = serde_json::from_str(&line).with_context(|| {
                format!("{}:{}: 外した読みの行を読めません", list.display(), index + 1)
            })?;
            table.push(reading_id(&row.key, row.nodes, row.sec));
        }
        let last = table.len() < chunk;
        table.sort_unstable();
        // 一致した行を次の回に残さない。一覧の同じ読みが別の回に分かれても 1 回だけ数える。
        let mut rest = if last {
            None
        } else {
            Some(BufWriter::new(tempfile::tempfile_in(tmp)?))
        };
        rows.rewind()?;
        let mut reader = BufReader::new(rows);
        let mut row = [0; ROW_BYTES];
        loop {
            if reader.read(&mut row[..1])? == 0 {
                break;
            }
            reader.read_exact(&mut row[1..])?;
            let id: ReadingId = row[..16].try_into()?;
            if table.binary_search(&id).is_ok() {
                matched[u32::from_le_bytes(row[16..].try_into()?) as usize] += 1;
            } else if let Some(rest) = &mut rest {
                rest.write_all(&row)?;
            }
        }
        match rest {
            Some(rest) => rows = rest.into_inner()?,
            None => break,
        }
    }
    for (index, entry) in entries.iter_mut().enumerate().filter(|(_, e)| e.verdict == CANDIDATE) {
        entry.detail = format!("matched={} readings={}", matched[index], readings[index]);
        if matched[index] > 0 {
            entry.verdict = "excluded-reading".into();
        }
    }
    Ok(())
}

/// 木の読みの照合の値を書き、読みの行数を返す。実測の nodes が無い読みはどの外した読みとも一致しないので書かない。
fn write_readings(dir: &Path, index: u32, out: &mut impl Write) -> Result<usize> {
    let mut readings = 0;
    for path in reading_files(dir)? {
        let file = File::open(&path)?;
        for (line_index, line) in BufReader::new(file).lines().enumerate() {
            let row: ReadingLine = serde_json::from_str(&line?)
                .with_context(|| format!("{}:{}", path.display(), line_index + 1))?;
            readings += 1;
            if let Some(nodes) = row.nodes {
                out.write_all(&reading_id(&row.key, nodes, row.sec))?;
                out.write_all(&index.to_le_bytes())?;
            }
        }
    }
    Ok(readings)
}

/// 同じ id・own・世代の出力先のうち、登録済みの出力先か先の候補と内容が同じ候補を外す。
///
/// 同じ木を別の場所に写した出力先を 2 回登録に渡さない。内容の違う同名の出力先は読み直した別の木なので残す。
fn mark_copies(entries: &mut [Entry]) {
    let mut groups = BTreeMap::<_, Vec<usize>>::new();
    for (index, e) in entries.iter().enumerate() {
        if e.id != "-" && (e.verdict == CANDIDATE || e.verdict == REGISTERED) {
            groups
                .entry((e.id.clone(), e.own.clone(), e.generation.clone()))
                .or_default()
                .push(index);
        }
    }
    for members in groups.into_values() {
        if members.len() < 2 || members.iter().all(|&i| entries[i].verdict != CANDIDATE) {
            continue;
        }
        // 読めない出力先はどれとも同じ内容とみなさない。
        let hashes: Vec<_> = members
            .iter()
            .map(|&i| content_hash(Path::new(&entries[i].path)).ok())
            .collect();
        let same = |a: usize, b: usize| hashes[a].is_some() && hashes[a] == hashes[b];
        let mut kept = Vec::new();
        for (m, &index) in members.iter().enumerate() {
            if entries[index].verdict != CANDIDATE {
                continue;
            }
            let registered = (0..members.len())
                .find(|&o| entries[members[o]].verdict == REGISTERED && same(m, o));
            let (verdict, other) = match (registered, kept.iter().find(|&&o| same(m, o))) {
                (Some(o), _) => ("registered-copy", o),
                (None, Some(&o)) => ("duplicate", o),
                (None, None) => {
                    kept.push(m);
                    continue;
                }
            };
            entries[index].detail = entries[members[other]].path.clone();
            entries[index].verdict = verdict.into();
        }
        for &m in &kept {
            let other = (0..members.len()).find(|&o| {
                o != m && (entries[members[o]].verdict == REGISTERED || kept.contains(&o))
            });
            if let Some(o) = other {
                let path = entries[members[o]].path.clone();
                let detail = &mut entries[members[m]].detail;
                if !detail.is_empty() {
                    detail.push(' ');
                }
                write!(detail, "differs-from={path}").expect("String への書き込み");
            }
        }
    }
}

/// 登録に使う材料 (読みのファイルと `node_moves.tsv`) の名前と内容のハッシュ。
fn content_hash(dir: &Path) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut paths = reading_files(dir)?;
    paths.push(dir.join(NODE_MOVES));
    for path in paths {
        let mut file = File::open(&path)?;
        hash.update(path.file_name().context("ファイル名がありません")?.as_encoded_bytes());
        hash.update(file.metadata()?.len().to_le_bytes());
        std::io::copy(&mut file, &mut hash)?;
    }
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";

    fn reading(key: &str, nodes: u64, sec: &str) -> String {
        format!(
            r#"{{"tag":"t","ply":1,"key":"{key}","nodes_limit":1000,"nodes":{nodes},"sec":{sec},"bestmove":"7g7f"}}"#
        )
    }

    fn tree(root: &Path, rel: &str, d0: &str) -> PathBuf {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(REGISTRATION_STATUS), r#"{"ready":true,"unresolved":{}}"#).unwrap();
        std::fs::write(dir.join("STATUS"), "DONE\n").unwrap();
        std::fs::write(dir.join(NODE_MOVES), "").unwrap();
        std::fs::write(dir.join("d0.jsonl"), format!("{d0}\n")).unwrap();
        std::fs::write(dir.join("d1.jsonl"), format!("{}\n", reading("other", 1001, "1.5")))
            .unwrap();
        dir
    }

    fn args(tmp: &Path, roots: &[&Path]) -> InventoryArgs {
        InventoryArgs {
            roots: roots.iter().map(|r| r.to_path_buf()).collect(),
            registered: None,
            excluded_readings: None,
            exclude_prefix: Vec::new(),
            keep_depth0: false,
            out: tmp.join("cand.txt"),
            table: tmp.join("table.tsv"),
        }
    }

    /// 表の行ごとの (出力先, 判定, detail)。
    fn run_table(args: InventoryArgs) -> Vec<(String, String, String)> {
        let table = args.table.clone();
        run(args).unwrap();
        std::fs::read_to_string(table)
            .unwrap()
            .lines()
            .skip(1)
            .map(|l| {
                let c: Vec<_> = l.split('\t').collect();
                (c[0].to_owned(), c[5].to_owned(), c[6].to_owned())
            })
            .collect()
    }

    fn by_id(args: InventoryArgs) -> BTreeMap<String, (String, String)> {
        run_table(args)
            .into_iter()
            .map(|(path, verdict, detail)| {
                let id = Path::new(&path).ancestors().nth(2).unwrap().file_name().unwrap();
                (id.to_str().unwrap().to_owned(), (verdict, detail))
            })
            .collect()
    }

    #[test]
    fn classifies_each_reason_and_reports_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("boxes");
        let ok = reading(KEY, 1000, "2.5");
        tree(&root, "b1/out/good/w/g1", &ok);
        let dir = tree(&root, "b1/out/notready/w/g1", &ok);
        std::fs::write(dir.join(REGISTRATION_STATUS), r#"{"ready":false}"#).unwrap();
        let dir = tree(&root, "b1/out/running/w/g1", &ok);
        std::fs::write(dir.join("STATUS"), "RUNNING\n").unwrap();
        let dir = tree(&root, "b1/out/nostatus/w/g1", &ok);
        std::fs::remove_file(dir.join("STATUS")).unwrap();
        let dir = tree(&root, "b1/out/nomoves/w/g1", &ok);
        std::fs::remove_file(dir.join(NODE_MOVES)).unwrap();
        tree(&root, "b1/out/skipme_1/w/g1", &ok);
        let dir = tree(&root, "b2/out/shallow/b/g2", &ok);
        std::fs::remove_file(dir.join("d1.jsonl")).unwrap();
        let dir = tree(&root, "b2/out/badjson/b/g1", &ok);
        std::fs::write(dir.join(REGISTRATION_STATUS), "{").unwrap();
        let mut f = Filters {
            registered: HashSet::new(),
            prefixes: vec!["skipme".into()],
            keep_depth0: false,
        };
        let (entries, _) = collect(std::slice::from_ref(&root), &f).unwrap();
        let got: BTreeMap<_, _> =
            entries.iter().map(|e| (e.id.as_str(), e.verdict.as_str())).collect();
        assert_eq!(
            got,
            BTreeMap::from([
                ("good", "candidate"),
                ("notready", "not-ready"),
                ("running", "status:RUNNING"),
                ("nostatus", "status:"),
                ("nomoves", "no-node-moves"),
                ("skipme_1", "prefix"),
                ("shallow", "depth0"),
                ("badjson", "bad-status-json"),
            ])
        );
        let shallow = entries.iter().find(|e| e.id == "shallow").unwrap();
        assert_eq!(
            (shallow.own.as_str(), shallow.generation.as_str(), shallow.place.as_str()),
            ("b", "g2", "b2/out")
        );
        let paths: Vec<_> = entries.iter().map(|e| e.path.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
        f.keep_depth0 = true;
        let (entries, _) = collect(std::slice::from_ref(&root), &f).unwrap();
        assert_eq!(entries.iter().find(|e| e.id == "shallow").unwrap().verdict, "candidate");
    }

    #[test]
    fn excludes_only_trees_holding_the_excluded_reading_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("out");
        tree(&root, "same/w/g1", &reading(KEY, 1000000077, "2320.998829311"));
        tree(&root, "reread/w/g1", &reading(KEY, 1000000031, "2101.25"));
        tree(&root, "sec_differs/w/g1", &reading(KEY, 1000000077, "2320.998829312"));
        tree(&root, "notation/w/g1", &reading(KEY, 1000000077, "2.320998829311e3"));
        tree(&root, "zero/w/g1", &reading(KEY, 5, "-0.0"));
        let list = tmp.path().join("excluded.jsonl");
        std::fs::write(
            &list,
            format!(
                "{{\"key\": \"{KEY}\", \"ply\": 16, \"nodes\": 1000000077, \"sec\": 2320.998829311}}\n\n\
                 {{\"key\": \"{KEY}\", \"nodes\": 5, \"sec\": 0.0}}\n"
            ),
        )
        .unwrap();
        let mut a = args(tmp.path(), &[&root]);
        a.excluded_readings = Some(list);
        let got = by_id(a);
        assert_eq!(got["same"], ("excluded-reading".into(), "matched=1 readings=2".into()));
        assert_eq!(got["reread"], ("candidate".into(), "matched=0 readings=2".into()));
        assert_eq!(got["sec_differs"].0, "candidate");
        assert_eq!(got["notation"].0, "excluded-reading");
        assert_eq!(got["zero"].0, "excluded-reading");
    }

    #[test]
    fn excluded_list_longer_than_the_table_is_matched_in_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("out");
        let dir = tree(&root, "many/w/g1", &reading(KEY, 1, "1.0"));
        let more: String = (2..=4).map(|n| reading(KEY, n, "1.0") + "\n").collect();
        std::fs::write(dir.join("d1.jsonl"), more).unwrap();
        tree(&root, "clean/w/g1", &reading(KEY, 9, "1.0"));
        let list = tmp.path().join("excluded.jsonl");
        // 同じ読みが別の回に分かれても、木の 1 行は 1 回だけ数える。
        let lines: String = [1, 7, 3, 1, 8, 4, 3]
            .iter()
            .map(|n| format!("{{\"key\":\"{KEY}\",\"nodes\":{n},\"sec\":1.0}}\n"))
            .collect();
        std::fs::write(&list, lines).unwrap();
        for chunk in [1, 2, 3, 7, 100] {
            let filters = Filters {
                registered: HashSet::new(),
                prefixes: Vec::new(),
                keep_depth0: false,
            };
            let (mut entries, _) = collect(std::slice::from_ref(&root), &filters).unwrap();
            apply_excluded(&mut entries, &list, tmp.path(), chunk).unwrap();
            let got: Vec<_> = entries
                .iter()
                .map(|e| (e.id.as_str(), e.verdict.as_str(), e.detail.as_str()))
                .collect();
            assert_eq!(
                got,
                [
                    ("clean", "candidate", "matched=0 readings=2"),
                    ("many", "excluded-reading", "matched=3 readings=4"),
                ],
                "chunk={chunk}"
            );
        }
    }

    #[test]
    fn unreadable_reading_line_is_reported_instead_of_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("out");
        tree(&root, "broken/w/g1", "{");
        let list = tmp.path().join("excluded.jsonl");
        std::fs::write(&list, "").unwrap();
        assert_eq!(by_id(args(tmp.path(), &[&root]))["broken"].0, "candidate");
        let mut a = args(tmp.path(), &[&root]);
        a.excluded_readings = Some(list);
        assert_eq!(by_id(a)["broken"].0, "bad-readings");
    }

    #[test]
    fn copy_of_a_registered_tree_is_not_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, c) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("c"));
        let line = reading(KEY, 1000, "2.5");
        let registered = tree(&a, "t/w/g1", &line);
        tree(&b, "t/w/g1", &line);
        // 登録していない木の写しどうしは、パス順で最初の 1 つだけ候補にする。
        tree(&b, "u/w/g1", &line);
        tree(&c, "u/w/g1", &line);
        let list = tmp.path().join("registered.txt");
        std::fs::write(&list, format!("{}\n", registered.display())).unwrap();
        let mut args = args(tmp.path(), &[&a, &b, &c]);
        args.registered = Some(list);
        let out = args.out.clone();
        let p = |root: &Path, rel: &str| root.join(rel).display().to_string();
        assert_eq!(
            run_table(args),
            [
                (p(&a, "t/w/g1"), "registered".to_owned(), String::new()),
                (p(&b, "t/w/g1"), "registered-copy".to_owned(), p(&a, "t/w/g1")),
                (p(&b, "u/w/g1"), "candidate".to_owned(), String::new()),
                (p(&c, "u/w/g1"), "duplicate".to_owned(), p(&b, "u/w/g1")),
            ]
        );
        assert_eq!(std::fs::read_to_string(out).unwrap(), format!("{}\n", p(&b, "u/w/g1")));
    }

    #[test]
    fn same_named_trees_with_different_content_are_judged_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b, c) = (tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("c"));
        let registered = tree(&a, "t/w/g1", &reading(KEY, 1000, "2.5"));
        tree(&b, "t/w/g1", &reading(KEY, 1001, "3.5"));
        let moved = tree(&c, "t/w/g1", &reading(KEY, 1000, "2.5"));
        std::fs::write(moved.join(NODE_MOVES), "changed\n").unwrap();
        // 世代が違えば別の木。
        tree(&c, "t/w/g2", &reading(KEY, 1000, "2.5"));
        let list = tmp.path().join("registered.txt");
        std::fs::write(&list, format!("{}\n", registered.display())).unwrap();
        let mut args = args(tmp.path(), &[&a, &b, &c]);
        args.registered = Some(list);
        let p = |root: &Path, rel: &str| root.join(rel).display().to_string();
        assert_eq!(
            run_table(args),
            [
                (p(&a, "t/w/g1"), "registered".to_owned(), String::new()),
                (
                    p(&b, "t/w/g1"),
                    "candidate".to_owned(),
                    format!("differs-from={}", p(&a, "t/w/g1"))
                ),
                (
                    p(&c, "t/w/g1"),
                    "candidate".to_owned(),
                    format!("differs-from={}", p(&a, "t/w/g1"))
                ),
                (p(&c, "t/w/g2"), "candidate".to_owned(), String::new()),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn registered_list_matches_through_aliased_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("real");
        let done = tree(&root, "done/w/g1", &reading(KEY, 1000, "2.5"));
        tree(&root, "todo/w/g1", &reading(KEY, 1000, "2.5"));
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let list = tmp.path().join("registered.txt");
        std::fs::write(
            &list,
            format!(
                "{}\n\n{}\n",
                alias.join("done/w/g1").display(),
                root.join("gone/w/g1").display()
            ),
        )
        .unwrap();
        let (registered, missing) = read_registered(&list).unwrap();
        assert_eq!(missing, 1);
        assert!(registered.contains(&done.canonicalize().unwrap()));
        let mut args = args(tmp.path(), &[&root, &alias]);
        args.registered = Some(list);
        let (out, table) = (args.out.clone(), args.table.clone());
        run(args).unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            format!("{}\n", root.join("todo/w/g1").display())
        );
        let table = std::fs::read_to_string(&table).unwrap();
        let rows: Vec<Vec<&str>> = table.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            [
                "tree",
                "id",
                "own",
                "generation",
                "place",
                "verdict",
                "detail"
            ]
        );
        assert_eq!(rows[1][1..6], ["done", "w", "g1", "-", "registered"]);
        assert_eq!(rows[2][1..6], ["todo", "w", "g1", "-", "candidate"]);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_outputs_naming_the_same_file_or_an_input() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("out");
        tree(&root, "t/w/g1", &reading(KEY, 1000, "2.5"));
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let mut a = args(tmp.path(), &[&root]);
        a.out = real.join("x.txt");
        a.table = alias.join("x.txt");
        assert!(run(a).unwrap_err().to_string().contains("別のファイル"));
        assert!(!real.join("x.txt").exists());
        let mut a = args(tmp.path(), &[&root]);
        a.out = real.join("sub/../x.txt");
        a.table = real.join("x.txt");
        assert!(run(a).is_err());
        let list = real.join("registered.txt");
        std::fs::write(&list, "").unwrap();
        let mut a = args(tmp.path(), &[&root]);
        a.registered = Some(list.clone());
        a.table = alias.join("registered.txt");
        assert!(run(a).is_err());
        assert_eq!(std::fs::read_to_string(&list).unwrap(), "");
    }
}
