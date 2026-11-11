//! 読み終わった木のうち、定跡の元データに登録していない出力先を一覧にする。

use super::output::NODE_MOVES;
use super::register::{is_done, reading_files};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const REGISTRATION_STATUS: &str = "registration_status.json";

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

/// 外した読みの照合の鍵。sec は JSON の数値として読んだ f64 のビット列で比べる。
type ReadingId = (String, u64, u64);

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
    excluded: Option<HashSet<ReadingId>>,
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
    ensure!(args.out != args.table, "--out と --table は別のファイルにしてください");
    let (registered, missing) = match &args.registered {
        Some(path) => read_registered(path)?,
        None => Default::default(),
    };
    let filters = Filters {
        registered,
        excluded: args.excluded_readings.as_deref().map(read_excluded).transpose()?,
        prefixes: args.exclude_prefix,
        keep_depth0: args.keep_depth0,
    };
    let (entries, unmatched) = collect(&args.roots, &filters)?;
    let mut list = String::new();
    let mut table = String::from("tree\tid\town\tgeneration\tplace\tverdict\tdetail\n");
    let mut counts = BTreeMap::<&str, usize>::new();
    for e in &entries {
        if e.verdict == "candidate" {
            writeln!(list, "{}", e.path)?;
        }
        writeln!(
            table,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            e.path, e.id, e.own, e.generation, e.place, e.verdict, e.detail
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

fn read_excluded(path: &Path) -> Result<HashSet<ReadingId>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    let mut set = HashSet::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row: ExcludedLine = serde_json::from_str(&line).with_context(|| {
            format!("{}:{}: 外した読みの行を読めません", path.display(), index + 1)
        })?;
        set.insert((row.key, row.nodes, row.sec.to_bits()));
    }
    Ok(set)
}

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
                detail: detail.replace(['\t', '\r', '\n'], " "),
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

fn classify(dir: &Path, real: &Path, id: &str, filters: &Filters) -> (String, String) {
    if filters.registered.contains(real) {
        return ("registered".into(), String::new());
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
    let Some(excluded) = &filters.excluded else {
        return ("candidate".into(), String::new());
    };
    match count_excluded(dir, excluded) {
        Ok((0, readings)) => ("candidate".into(), format!("matched=0 readings={readings}")),
        Ok((matched, readings)) => {
            ("excluded-reading".into(), format!("matched={matched} readings={readings}"))
        }
        // 読めない行を飛ばすと、外した読みを含む木を候補に残しうる。
        Err(e) => ("bad-readings".into(), format!("{e:#}")),
    }
}

/// 外した読みに一致した行数と、木の読みの行数。
fn count_excluded(dir: &Path, excluded: &HashSet<ReadingId>) -> Result<(usize, usize)> {
    let (mut matched, mut readings) = (0, 0);
    for path in reading_files(dir)? {
        let file = std::fs::File::open(&path)?;
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let row: ReadingLine = serde_json::from_str(&line?)
                .with_context(|| format!("{}:{}", path.display(), index + 1))?;
            readings += 1;
            if let Some(nodes) = row.nodes
                && excluded.contains(&(row.key, nodes, row.sec.to_bits()))
            {
                matched += 1;
            }
        }
    }
    Ok((matched, readings))
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

    fn filters() -> Filters {
        Filters {
            registered: HashSet::new(),
            excluded: None,
            prefixes: Vec::new(),
            keep_depth0: false,
        }
    }

    fn verdicts(root: &Path, filters: &Filters) -> BTreeMap<String, (String, String)> {
        collect(&[root.to_owned()], filters)
            .unwrap()
            .0
            .into_iter()
            .map(|e| (e.id, (e.verdict, e.detail)))
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
        let mut f = filters();
        f.prefixes = vec!["skipme".into()];
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
        let mut sorted = entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>();
        sorted.sort();
        assert_eq!(entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(), sorted);
        f.keep_depth0 = true;
        assert_eq!(verdicts(&root, &f)["shallow"].0, "candidate");
    }

    #[test]
    fn excludes_only_trees_holding_the_excluded_reading_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("out");
        tree(&root, "same/w/g1", &reading(KEY, 1000000077, "2320.998829311"));
        tree(&root, "reread/w/g1", &reading(KEY, 1000000031, "2101.25"));
        tree(&root, "sec_differs/w/g1", &reading(KEY, 1000000077, "2320.998829312"));
        tree(&root, "notation/w/g1", &reading(KEY, 1000000077, "2.320998829311e3"));
        let list = tmp.path().join("excluded.jsonl");
        std::fs::write(
            &list,
            format!(
                "{{\"key\": \"{KEY}\", \"ply\": 16, \"nodes\": 1000000077, \"sec\": 2320.998829311}}\n\n"
            ),
        )
        .unwrap();
        let mut f = filters();
        f.excluded = Some(read_excluded(&list).unwrap());
        let got = verdicts(&root, &f);
        assert_eq!(got["same"], ("excluded-reading".into(), "matched=1 readings=2".into()));
        assert_eq!(got["reread"], ("candidate".into(), "matched=0 readings=2".into()));
        assert_eq!(got["sec_differs"].0, "candidate");
        assert_eq!(got["notation"].0, "excluded-reading");
    }

    #[test]
    fn unreadable_reading_line_is_reported_instead_of_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("out");
        tree(&root, "broken/w/g1", "{");
        let mut f = filters();
        f.excluded = Some(HashSet::new());
        assert_eq!(verdicts(&root, &f)["broken"].0, "bad-readings");
        f.excluded = None;
        assert_eq!(verdicts(&root, &f)["broken"].0, "candidate");
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
        let out = tmp.path().join("cand.txt");
        let table = tmp.path().join("table.tsv");
        run(InventoryArgs {
            roots: vec![root.clone(), alias.clone()],
            registered: Some(list),
            excluded_readings: None,
            exclude_prefix: Vec::new(),
            keep_depth0: false,
            out: out.clone(),
            table: table.clone(),
        })
        .unwrap();
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
}
