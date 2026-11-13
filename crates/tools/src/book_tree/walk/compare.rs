//! 自前の main と外部定跡を辿った出口の比較。

use super::super::exit_report::{kif, side_name};
use super::super::master_index::MasterIndex;
use super::super::master_table::{Row as MasterRow, mean_eval};
use super::super::schema::Schema;
use super::*;

#[derive(Args, Debug)]
pub(in super::super) struct CompareArgs {
    /// 自前の定跡の版の元データ (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 外部定跡 (.db または .ybb)。相手の手はこの定跡だけで決める
    #[arg(long)]
    opp_book: PathBuf,
    /// 比較結果 (TSV)
    #[arg(long)]
    table: PathBuf,
    /// 日本語の比較結果 (Markdown)
    #[arg(long)]
    markdown: PathBuf,
    /// 自前の定跡を使う側
    #[arg(long, value_enum, default_value_t = SideArg::Both)]
    side: SideArg,
    /// 外部定跡の最善からこの差以内の手を辿る (0 は同値の最善をすべて辿る)
    #[arg(long, default_value_t = 0)]
    opp_book_diff: i32,
    /// 最善に割り当てる仮の重み (同値なら等分)。残りを差以内の他の手に等分
    #[arg(long, default_value_t = 0.9)]
    opp_book_best: f64,
    /// 初期局面から指した手数がこの上限に達したら打ち切る
    #[arg(long, default_value_t = 160)]
    max_ply: usize,
    /// 仮の重みがこれ未満なら打ち切る (0 なら重みで打ち切らない)
    #[arg(long, default_value_t = 1e-5)]
    weight_floor: f64,
    /// 自前の定跡が切れた局面から、両者が外部定跡の最善を指す手数の上限
    #[arg(long, default_value_t = 100)]
    cont_max: usize,
    /// 初期局面からこの手順 (空白区切りの USI) を指した局面から辿る
    #[arg(long, value_name = "USI")]
    start_moves: Option<String>,
    /// 強い対局の JSON Lines (gzip 可)。局面への到達局数と得点を数える
    #[arg(long)]
    opp_records: Option<PathBuf>,
    /// 集計する対局の、両者のレートの下限
    #[arg(long, default_value_t = 3800.0, requires = "opp_records")]
    records_min_rate: f64,
    /// 棋譜を初期局面からこの手数まで調べる
    #[arg(long, default_value_t = 200, requires = "opp_records")]
    records_max_ply: usize,
}

impl CompareArgs {
    fn check(&self) -> Result<()> {
        ensure!(self.opp_book_diff >= 0, "--opp-book-diff は 0 以上にしてください");
        ensure!(
            self.opp_book_best > 0.0 && self.opp_book_best < 1.0,
            "--opp-book-best は 0 より大きく 1 より小さくしてください (最善だけなら --opp-book-diff 0)"
        );
        ensure!(
            self.weight_floor.is_finite() && self.weight_floor >= 0.0,
            "--weight-floor は有限の 0 以上にしてください"
        );
        ensure!(self.records_min_rate.is_finite(), "--records-min-rate は有限の数にしてください");
        let schema_paths: Vec<_> =
            self.schema.iter().flat_map(|dir| super::super::schema::paths(dir)).collect();
        let mut inputs = vec![
            ("--master", self.master.as_path()),
            ("--opp-book", self.opp_book.as_path()),
        ];
        for path in &schema_paths {
            inputs.push(("--schema", path.as_path()));
        }
        if let Some(path) = &self.opp_records {
            inputs.push(("--opp-records", path.as_path()));
        }
        reject_same_file(&self.table, &inputs)?;
        inputs.push(("--table", self.table.as_path()));
        reject_same_file(&self.markdown, &inputs)?;
        Ok(())
    }

    fn options(&self) -> WalkOptions {
        WalkOptions {
            max_ply: self.max_ply,
            floor: self.weight_floor,
            // 続きの止まり方を区別するため、このコマンドで調べ直す。
            cont_max: 0,
            opp: OppOptions {
                min_games: 1,
                book_diff: self.opp_book_diff,
                book_best: self.opp_book_best,
                book_first: true,
            },
        }
    }
}

fn main_move(index: &mut MasterIndex, key: &str) -> Result<Option<String>> {
    let rows = index.rows(key)?;
    super::super::master_table::validate(&rows)?;
    let mains: Vec<_> = rows.iter().filter(|r| r.get("role") == "main").collect();
    Ok(mains.first().map(|r| r.get("move").to_owned()))
}

#[derive(Default)]
struct OwnValue {
    ply: Option<usize>,
    mv: String,
    value: Option<i64>,
    kind: String,
    source: String,
}

fn value_black(row: &MasterRow, key: &str) -> Result<Option<i64>> {
    Ok(mean_eval(row.get("evals"))?.map(|v| signed_value(v, key)))
}

fn signed_value(value: i32, key: &str) -> i64 {
    if black_to_move(key) {
        i64::from(value)
    } else {
        -i64::from(value)
    }
}

fn last_own(index: &mut MasterIndex, row: &Row, initial: &str) -> Result<OwnValue> {
    let mut key = initial.to_owned();
    let mut last = OwnValue::default();
    for (i, mv) in row.moves.iter().enumerate() {
        if black_to_move(&key) == (row.side == "b") {
            // 指定された開始手順が元データに無い場合、古い手の値を現在の出口の値にしない。
            last = OwnValue {
                ply: Some(i + 1),
                mv: mv.clone(),
                ..OwnValue::default()
            };
            let rows = index.rows(&key)?;
            super::super::master_table::validate(&rows)?;
            if let Some(entry) = rows.iter().find(|r| r.get("move") == mv.as_str()) {
                last.value = value_black(entry, &key)?;
                last.kind = entry.get("value_kind").to_owned();
                last.source = entry.get("source").to_owned();
            }
        }
        key = advance(&key, mv)?;
    }
    Ok(last)
}

#[derive(Default)]
struct Continuation {
    moves: Vec<String>,
    values: Vec<i64>,
    stop: &'static str,
}

fn follow(key: &str, book: &mut Probe, max: usize) -> Result<Continuation> {
    let mut key = key.to_owned();
    let mut seen = HashSet::new();
    let mut out = Continuation::default();
    loop {
        if !seen.insert(key.clone()) {
            out.stop = "repetition";
            break;
        }
        let Some((mv, value)) = book.best(&key)? else {
            out.stop = "out";
            break;
        };
        if out.moves.len() == max {
            out.stop = "max";
            break;
        }
        let Ok(child) = advance(&key, &mv) else {
            out.stop = "illegal";
            break;
        };
        out.values.push(signed_value(value, &key));
        out.moves.push(mv);
        key = child;
    }
    Ok(out)
}

struct Compared {
    own: OwnValue,
    external: Option<i64>,
    continuation: Continuation,
    prepared: bool,
}

fn details(
    row: &Row,
    index: &mut MasterIndex,
    book: &mut Probe,
    initial: &str,
    max: usize,
) -> Result<Compared> {
    let best = book.best(&row.key)?;
    let external = best.as_ref().map(|(_, v)| signed_value(*v, &row.key));
    let continuation = if row.stop == Stop::Own {
        follow(&row.key, book, max)?
    } else {
        Continuation::default()
    };
    let prepared =
        row.stop == Stop::Own && best.is_some_and(|(mv, _)| advance(&row.key, &mv).is_ok());
    Ok(Compared {
        own: last_own(index, row, initial)?,
        external,
        continuation,
        prepared,
    })
}

fn opt(v: Option<impl ToString>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

fn format_table(
    rows: &[Row],
    values: &[Compared],
    stats: Option<&[[records::Reach; 3]]>,
) -> String {
    let mut out = String::from(
        "side\tstop\tnext_ply\tweight\tprepared\town_move_ply\town_move\town_value_black\town_value_kind\town_source\topp_value_black\tcont_plies\tcont_stop\tcont_end_black\tcont_values_black\tkey\tmoves\tcont\tn0\tscore0\tn10\tscore10\tn20\tscore20\tblack_wins0\twhite_wins0\tdraws0\n",
    );
    for (i, (row, value)) in rows.iter().zip(values).enumerate() {
        let cont = &value.continuation;
        let mut cells = vec![
            row.side.to_owned(),
            row.stop.as_str().to_owned(),
            (row.ply + 1).to_string(),
            format!("{:.12}", row.weight),
            value.prepared.to_string(),
            opt(value.own.ply),
            value.own.mv.clone(),
            opt(value.own.value),
            value.own.kind.clone(),
            value.own.source.clone(),
            opt(value.external),
            if row.stop == Stop::Own {
                cont.moves.len().to_string()
            } else {
                String::new()
            },
            cont.stop.to_owned(),
            opt(cont.values.last()),
            cont.values.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "),
            row.key.clone(),
            row.moves.join(" "),
            cont.moves.join(" "),
        ];
        if let Some(stats) = stats {
            for (reach, offset) in stats[i].iter().zip(records::OFFSETS) {
                if row.ply >= offset {
                    cells.push(reach.games.to_string());
                    cells.push(reach.score_text());
                } else {
                    cells.extend([String::new(), String::new()]);
                }
            }
            cells.extend([
                stats[i][0].black_wins.to_string(),
                stats[i][0].white_wins.to_string(),
                stats[i][0].draws.to_string(),
            ]);
        } else {
            cells.extend(vec![String::new(); 9]);
        }
        writeln!(out, "{}", cells.join("\t")).expect("String に書ける");
    }
    out
}

fn shown(v: Option<i64>) -> String {
    v.map(|v| format!("{v:+}")).unwrap_or_else(|| "不明".into())
}

fn markdown(
    args: &CompareArgs,
    rows: &[Row],
    values: &[Compared],
    stats: Option<&[[records::Reach; 3]]>,
) -> Result<String> {
    let mut out = format!(
        "# 自前の定跡と外部定跡の比較\n\n自前は main、相手は外部定跡の最善から {} 点以内の手を辿る。最善の同値はすべて辿る。最善以外の対象手がある場合は最善に {:.1}%、他の対象手に残りを等分する。最善だけの場合は 100% を同値の最善で等分する。この仮の重みは来る確率ではない。同じ手数で同じ局面に合流した重みは合算し、手順は代表 1 本を示す。\n\n値はすべてプラスが先手良し。自前の値は最後に指した自前の手の値で、定跡が切れた局面の読みではない。外部定跡の値とも尺度が異なる。出口は自前が読み始める局面。準備の線は自前が先に切れ、外部定跡の代表の最善がまだ指せる線で、良し悪しは判定しない。\n\n辿りは {} 手、仮の重み {} 未満で打ち切る。循環は辿りの手数上限で止まる。外部定跡の続きは両者が最善を指す代表 1 本で、同値ならファイルで先の手を選び、{} 手・定跡切れ・循環・指せない手を区別する。続きの末尾の値は最後に引けた合法手の値で、打ち切りの場合は定跡の末端の値ではない。\n",
        args.opp_book_diff,
        args.opp_book_best * 100.0,
        args.max_ply,
        args.weight_floor,
        args.cont_max
    );
    let prefix: Vec<String> = args
        .start_moves
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if prefix.is_empty() {
        out.push_str("\n初期局面から重み 1 で辿る。\n");
    } else {
        writeln!(out, "\n開始手順を強制: {}。次の {} 手目から重み 1 として辿る条件付きの比較。この開始手順は自前・外部の定跡の手と一致する必要はなく、初期局面からこの手順に来る重みは表していない。", kif(&prefix)?, prefix.len() + 1).expect("String に書ける");
    }
    if args.opp_records.is_some() {
        writeln!(out, "\n対局の実績は両者 R{} 以上の対局を、初期局面から {} 手まで局面で数える。得点は自前の側で勝ち 1・引き分け 0.5。出口・10 手前・20 手前の局数と得点率を示し、局数 0 の得点は不明。10 手前・20 手前が初期局面より前になる欄は対象局面なしとする。勝敗だけで準備の線の危険度を決めない。", args.records_min_rate, args.records_max_ply).expect("String に書ける");
    } else {
        out.push_str("\n対局の棋譜は未指定。局数・得点は未集計。\n");
    }
    out.push_str("\n止まり: own = 自前の手番で main が無い、out = 相手の手番で外部定跡に手が無い、tiny = 重みの下限、max = 手数の上限、rare = 指せない外部定跡の手。\n");
    for (prepared, title) in [
        (true, "準備の線 (自前が先に切れる)"),
        (false, "その他の止まり"),
    ] {
        writeln!(out, "\n## {title}\n\n| 自前の側 | 止まり | 次の手番号 | 仮の重み | 最後の自前の手・値・出所 | 外部定跡の値 | 外部定跡の続き | 到達局数・得点率 (出口 / 10手前 / 20手前) | 全手順 |\n|---|---|---:|---:|---|---:|---|---|---|").expect("String に書ける");
        for (i, (row, value)) in rows
            .iter()
            .zip(values)
            .enumerate()
            .filter(|(_, (_, value))| value.prepared == prepared)
        {
            let own = if let Some(ply) = value.own.ply {
                let move_name =
                    kif(&row.moves[..ply])?.rsplit(' ').next().unwrap_or_default().to_owned();
                format!(
                    "{ply} 手目 {move_name} {} ({}, {})",
                    shown(value.own.value),
                    if value.own.kind.is_empty() {
                        "出所不明"
                    } else {
                        &value.own.kind
                    },
                    value.own.source
                )
            } else {
                "無し".into()
            };
            let cont = &value.continuation;
            let continuation = if row.stop == Stop::Own {
                format!(
                    "{} 手・{}・末尾 {}",
                    cont.moves.len(),
                    cont.stop,
                    shown(cont.values.last().copied())
                )
            } else {
                "対象外".into()
            };
            let score = stats
                .map(|s| {
                    s[i].iter()
                        .zip(records::OFFSETS)
                        .map(|(r, offset)| {
                            if row.ply < offset {
                                return "対象局面なし".to_owned();
                            }
                            format!(
                                "{} 局・{}",
                                r.games,
                                if r.games == 0 {
                                    "不明".into()
                                } else {
                                    r.score_text()
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_else(|| "未集計".into());
            let mut path = kif(&row.moves)?;
            if !cont.moves.is_empty() {
                let all: Vec<_> = row.moves.iter().chain(&cont.moves).cloned().collect();
                let full = kif(&all)?;
                let tail = full.split(' ').skip(row.moves.len()).collect::<Vec<_>>().join(" ");
                write!(path, " → 外部定跡の続き: {tail}").expect("String に書ける");
            }
            writeln!(
                out,
                "| {} | {} | {} 手目 | {:.8} | {} | {} | {} | {} | {} |",
                side_name(row.side),
                row.stop.as_str(),
                row.ply + 1,
                row.weight,
                own,
                shown(value.external),
                continuation,
                score,
                path
            )
            .expect("String に書ける");
        }
    }
    Ok(out)
}

pub(in super::super) fn run(args: CompareArgs) -> Result<()> {
    args.check()?;
    let (start, start_moves) = start_position(HIRATE_KEY, args.start_moves.as_deref())?;
    ensure!(start_moves.len() <= args.max_ply, "--start-moves が --max-ply を超えています");
    let schema = Schema::load(args.schema.as_deref())?;
    let mut index = MasterIndex::open(&args.master, &schema)?;
    let external = open_book(&args.opp_book)?;
    let mut sources = Sources {
        freq: None,
        book: Some(book_probe(&external)),
    };
    let from = WalkFrom {
        start: &start,
        start_moves: &start_moves,
        overrides: &HashMap::new(),
    };
    let mut rows = Vec::new();
    for &side in args.side.sides() {
        rows.extend(walk_using(
            &mut |key| main_move(&mut index, key),
            &mut sources,
            side,
            &from,
            &mut NoTag,
            args.options(),
        )?);
    }
    rows.sort_by(|a, b| {
        a.side
            .cmp(b.side)
            .then(b.weight.total_cmp(&a.weight))
            .then(a.key.cmp(&b.key))
            .then(a.ply.cmp(&b.ply))
    });
    let book = sources.book.as_mut().expect("外部定跡を指定済み");
    let values = rows
        .iter()
        .map(|r| details(r, &mut index, book, HIRATE_KEY, args.cont_max))
        .collect::<Result<Vec<_>>>()?;
    let stats = args
        .opp_records
        .as_deref()
        .map(|path| {
            records::reach_stats(
                &rows,
                HIRATE_KEY,
                path,
                &records::RecordOptions {
                    min_rate: args.records_min_rate,
                    max_ply: args.records_max_ply,
                },
            )
        })
        .transpose()?;
    let table = format_table(&rows, &values, stats.as_deref());
    let report = markdown(&args, &rows, &values, stats.as_deref())?;
    write_atomic(&args.table, &table)?;
    write_atomic(&args.markdown, &report)?;
    println!(
        "{} 行 (自前が先に切れる線 {} 行)",
        rows.len(),
        values.iter().filter(|v| v.prepared).count()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
