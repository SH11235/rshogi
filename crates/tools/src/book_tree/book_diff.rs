//! 定跡の局面と記載順の手を比較する。
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Args, Debug)]
pub(super) struct DiffArgs {
    /// 比較元の定跡 (.db)
    old: PathBuf,
    /// 比較先の定跡 (.db)
    new: PathBuf,
}
#[derive(Default)]
pub(super) struct DbPosition {
    pub moves: Vec<String>,
    pub ply: String,
}
pub(super) fn read(path: &Path, strict: bool) -> Result<BTreeMap<String, DbPosition>> {
    let mut book = BTreeMap::<String, DbPosition>::new();
    let mut current = None;
    for (i, line) in BufReader::new(
        std::fs::File::open(path).with_context(|| format!("開けません: {}", path.display()))?,
    )
    .lines()
    .enumerate()
    {
        let line = line.with_context(|| format!("読めません: {}", path.display()))?;
        if strict {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.is_empty() || line.starts_with('#') {
                continue;
            }
            ensure!(fields.len() == 5, "{}:{}: invalid DB line", path.display(), i + 1);
            if fields[0] == "sfen" {
                let key = fields[1..4].join(" ");
                ensure!(!book.contains_key(&key), "duplicate position: {key}");
                book.insert(
                    key.clone(),
                    DbPosition {
                        moves: vec![],
                        ply: fields[4].into(),
                    },
                );
                current = Some(key);
            } else {
                let key =
                    current.as_ref().ok_or_else(|| anyhow::anyhow!("move before position"))?;
                let moves = &mut book.get_mut(key).unwrap().moves;
                ensure!(
                    !moves.iter().any(|m| m == fields[0]),
                    "duplicate move: {key} {}",
                    fields[0]
                );
                moves.push(fields[0].into());
            }
        } else if line.starts_with("sfen ") {
            let fields: Vec<_> = line.split(' ').collect();
            let key = fields[1..fields.len().min(4)].join(" ");
            book.entry(key.clone()).or_default().ply = fields.get(4).unwrap_or(&"").to_string();
            current = Some(key);
        } else if !line.is_empty()
            && !line.starts_with('#')
            && let Some(key) = &current
        {
            book.get_mut(key).unwrap().moves.push(line.split(' ').next().unwrap().into());
        }
    }
    if strict {
        ensure!(book.values().all(|p| !p.moves.is_empty()), "empty position");
    } else {
        book.retain(|_, p| !p.moves.is_empty());
    }
    Ok(book)
}
pub(super) fn run(args: DiffArgs) -> Result<()> {
    diff(&args.old, &args.new, &mut std::io::stdout().lock())
}
fn diff(old: &Path, new: &Path, out: &mut impl Write) -> Result<()> {
    let a = read(old, false)?;
    let b = read(new, false)?;
    let add: Vec<_> = b.keys().filter(|k| !a.contains_key(*k)).collect();
    let rem: Vec<_> = a.keys().filter(|k| !b.contains_key(*k)).collect();
    let changed: Vec<_> =
        a.keys().filter(|k| b.get(*k).is_some_and(|p| p.moves != a[*k].moves)).collect();
    writeln!(
        out,
        "局面数 {} → {}、追加 {}、削除 {}、手の変更 {}",
        a.len(),
        b.len(),
        add.len(),
        rem.len(),
        changed.len()
    )?;
    if !add.is_empty() || !rem.is_empty() || !changed.is_empty() {
        writeln!(out, "\n| 種別 | 手数 | 局面 | 旧 | 新 |\n|---|---|---|---|---|")?;
        for k in changed {
            writeln!(
                out,
                "| 変更 | {} | `{k}` | {} | {} |",
                b[k].ply,
                a[k].moves.join(" "),
                b[k].moves.join(" ")
            )?;
        }
        for k in add {
            writeln!(out, "| 追加 | {} | `{k}` | - | {} |", b[k].ply, b[k].moves.join(" "))?;
        }
        for k in rem {
            writeln!(out, "| 削除 | {} | `{k}` | {} | - |", a[k].ply, a[k].moves.join(" "))?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn order_duplicates_and_ply_are_preserved() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let b = d.path().join("b");
        std::fs::write(&a, "sfen x b - 1\na none 1 0 1\nb none 1 0 1\nsfen empty w - 3\n").unwrap();
        std::fs::write(&b, "sfen x b - 9\nb none 1 0 1\na none 1 0 1\n").unwrap();
        let mut out = Vec::new();
        diff(&a, &b, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "局面数 1 → 1、追加 0、削除 0、手の変更 1\n\n| 種別 | 手数 | 局面 | 旧 | 新 |\n|---|---|---|---|---|\n| 変更 | 9 | `x b -` | a b | b a |\n"
        );
        assert!(read(&a, true).is_err());
    }
}
