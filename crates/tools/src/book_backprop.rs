//! YANEURAOU-DB2016 テキスト `.db` の評価値を negamax 逆伝播するライブラリ。
//!
//! `book_backprop` bin と `book_mine run` の双方から呼ぶ。
//!
//! 数千万手の定跡を読めるよう、入力の文字列は局面の代表 SFEN だけを持つ。手は値と行き先の番号の配列で持ち、
//! 書き出しでは入力を読み直して、指し手欄などは入力の文字列をそのまま使う。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::hash::{DefaultHasher, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move, Piece, PieceType, Square, Value};

use crate::book_db::ply_of;
pub use crate::book_db::{BOOK_HEADER, child_position_after_move, strip_ply};
use crate::book_retrograde::{self, Arc, NONE, Step};
use crate::common::io::partial_path;

/// 千日手の値に指定できる範囲。詰みの値 (32000) より内側に限り、値をずらしても桁あふれしないようにする。
const DRAW_VALUE_LIMIT: i32 = 30_000;

/// 2 回目以降の読み込みで、1 回目と違う内容を読んだときのエラー。
const CHANGED: &str = "定跡が読み込みの途中で変わりました";

/// 合法な指し手を持つ行。非合法手と `none` 行は持たない。
const ROW_USABLE: u8 = 1;
/// 反転キーで合流した辺。
const ROW_FLIP: u8 = 2;
/// 未探索の子への辺を除いた行。値を書き換えない。
const ROW_PRESERVE: u8 = 4;
/// 手順の合流を補完した行。局面の行の最後に並び、探索ラベルを持たない。
const ROW_ADDED: u8 = 8;

const NODE_BLACK: u8 = 1;
const NODE_CHECKED: u8 = 2;
/// 入力時に全行が value=0 / depth=0 で、`skip_unsearched_children` のとき全行を書き換えない局面。
const NODE_PRESERVED: u8 = 4;

/// 伝播値と既存ラベル値の合成方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MergeMode {
    /// 純 negamax。伝播値で置き換える。
    Replace,
    /// 自分の手は既存ラベル値と伝播値の小さい方を採る。相手の手は伝播値のまま。
    Min,
}

impl MergeMode {
    fn as_str(self) -> &'static str {
        match self {
            MergeMode::Replace => "replace",
            MergeMode::Min => "min",
        }
    }
}

/// book 内子局面への辺。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub to: usize,
    pub via_flip: bool,
}

/// 候補手ごとの伝播前後の値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveValue {
    pub old: i32,
    pub new: i32,
    pub edge: Option<Edge>,
    /// 合法な指し手を持つ行か。非合法手と `none` 行は `false`。
    pub usable: bool,
}

/// 逆伝播の追加オプション。既定値は `book_backprop` の従来挙動。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackpropOptions {
    /// 非合法手と `none` 行を局面の best (伝播値と best の変化集計) から除く。
    /// 行自体は値を変えずに書き出す。
    pub skip_unusable_moves: bool,
    /// 未探索の子を評価値 0 と扱って親の探索値を上書きしないよう、全行 depth=0 の子への辺を除く。
    /// value は判定に使わず、候補手の無い局面は除外しない。
    /// 入力時に全行 value=0 / depth=0 の局面は、未探索マーカーを保つため出力も保持する。
    pub skip_unsearched_children: bool,
    /// 異なる手順で同一局面へ合流する合法手を補完し、手順間で候補手を共有する。
    pub add_transposition_moves: bool,
}

/// 千日手の値。それぞれ手番の側から見た値で、先手を自分として解くときは `black`、後手なら `white` を使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrawValues {
    pub black: i32,
    pub white: i32,
}

impl DrawValues {
    /// 先手・後手とも同じ値。
    pub fn both(value: i32) -> Self {
        Self {
            black: value,
            white: value,
        }
    }

    fn of(self, black: bool) -> i32 {
        if black { self.black } else { self.white }
    }
}

/// 局面の best 計算に共通の設定。
#[derive(Debug, Clone, Copy)]
struct BestParams {
    draws: DrawValues,
    merge: MergeMode,
    skip_unusable_moves: bool,
}

impl BestParams {
    fn counts(self, graph: &Graph, row: usize) -> bool {
        !self.skip_unusable_moves || graph.usable(row)
    }
}

/// 局面を頂点、候補手を行とするグラフ。局面は ply 抜き key の昇順に並べ、同じ key の `sfen` 行の手は入力順に連結する。
pub struct Graph {
    /// 書き出しで読み直す入力。
    source: Option<PathBuf>,
    options: BackpropOptions,
    /// 局面の代表 SFEN (同じ key の行のうち最小 ply の行)。
    sfens: Texts,
    node_flags: Vec<u8>,
    /// 局面 `v` の行は `row_start[v]..row_start[v + 1]`。
    row_start: Vec<u32>,
    /// 局面 `v` の `sfen` 行の入力での位置は `blocks[block_start[v]..block_start[v + 1]]` (入力順)。
    block_start: Vec<u32>,
    blocks: Vec<u64>,
    /// 局面の `sfen` 行ごとの行のハッシュ値と、入力の残りの部分。読み直した入力を 1 回目と比べる。
    hashes: Vec<u64>,
    source_check: SourceCheck,
    old: Vec<i32>,
    new: Vec<i32>,
    /// 行き先の局面。book 内に子局面が無ければ `NONE`。
    to: Vec<u32>,
    row_flags: Vec<u8>,
    /// 補完した行の指し手 (`Move` の下位 16 bit)。行の順に並ぶ。
    added: Vec<u16>,
    pub flip_edges: usize,
    pub illegal_moves: usize,
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("nodes", &self.len())
            .field("moves", &self.total_rows())
            .finish_non_exhaustive()
    }
}

impl Graph {
    /// 局面の数。
    pub fn len(&self) -> usize {
        self.node_flags.len()
    }

    /// 局面が無いか (`sfen` 行の無い入力)。
    pub fn is_empty(&self) -> bool {
        self.node_flags.is_empty()
    }

    /// 局面の代表 SFEN (ply 付き)。
    pub fn sfen(&self, node: usize) -> &str {
        self.sfens.get(node)
    }

    /// 局面の ply 抜き key。
    pub fn key(&self, node: usize) -> &str {
        strip_ply(self.sfen(node))
    }

    /// ply 抜き key の局面を引く。
    pub fn find(&self, key: &str) -> Option<usize> {
        find_key(&self.sfens, key)
    }

    /// 局面の候補手。入力の行の順で、補完した手は最後に並ぶ。
    pub fn moves(&self, node: usize) -> impl Iterator<Item = MoveValue> + '_ {
        self.rows(node).map(|row| MoveValue {
            old: self.old[row],
            new: self.new[row],
            edge: self.edge_to(row).map(|to| Edge {
                to,
                via_flip: self.row_flags[row] & ROW_FLIP != 0,
            }),
            usable: self.usable(row),
        })
    }

    fn rows(&self, node: usize) -> Range<usize> {
        self.row_start[node] as usize..self.row_start[node + 1] as usize
    }

    fn blocks(&self, node: usize) -> &[u64] {
        &self.blocks[self.block_start[node] as usize..self.block_start[node + 1] as usize]
    }

    /// 局面 key が先手番か。
    fn black(&self, node: usize) -> bool {
        self.node_flags[node] & NODE_BLACK != 0
    }

    /// 局面の手番の色の番号 (0 = 先手、1 = 後手)。
    fn color(&self, node: usize) -> usize {
        usize::from(!self.black(node))
    }

    fn checked(&self, node: usize) -> bool {
        self.node_flags[node] & NODE_CHECKED != 0
    }

    fn preserved(&self, node: usize) -> bool {
        self.node_flags[node] & NODE_PRESERVED != 0
    }

    fn edge_to(&self, row: usize) -> Option<usize> {
        (self.to[row] != NONE).then_some(self.to[row] as usize)
    }

    fn usable(&self, row: usize) -> bool {
        self.row_flags[row] & ROW_USABLE != 0
    }

    fn preserve(&self, row: usize) -> bool {
        self.row_flags[row] & ROW_PRESERVE != 0
    }

    fn added(&self, row: usize) -> bool {
        self.row_flags[row] & ROW_ADDED != 0
    }

    fn total_rows(&self) -> usize {
        self.old.len()
    }
}

/// 1 つの文字列に並べた文字列の一覧。
#[derive(Default)]
struct Texts {
    text: String,
    start: Vec<usize>,
    len: Vec<u32>,
    /// 差し替えで使わなくなった領域の長さ。
    unused: usize,
}

impl Texts {
    fn len(&self) -> usize {
        self.start.len()
    }

    fn get(&self, index: usize) -> &str {
        let start = self.start[index];
        &self.text[start..start + self.len[index] as usize]
    }

    fn push(&mut self, text: &str) -> Result<u32> {
        let index = u32::try_from(self.start.len())
            .ok()
            .filter(|&index| index != NONE)
            .ok_or_else(|| anyhow!("局面が多すぎます"))?;
        self.len.push(text_len(text)?);
        self.start.push(self.text.len());
        self.text.push_str(text);
        Ok(index)
    }

    /// 文字列を差し替える。元の領域に収まればそこへ書き、収まらなければ後ろに足す。
    /// 使わなくなった領域が全体の半分を超えたら詰め直し、差し替えの回数によらず入力の大きさに収める。
    fn replace(&mut self, index: usize, text: &str) -> Result<()> {
        let (start, old) = (self.start[index], self.len[index] as usize);
        let len = text_len(text)?;
        if text.len() <= old && self.text.is_char_boundary(start + text.len()) {
            self.text.replace_range(start..start + text.len(), text);
            self.unused += old - text.len();
        } else {
            self.unused += old;
            self.start[index] = self.text.len();
            self.text.push_str(text);
        }
        self.len[index] = len;
        if self.unused > self.text.len() / 2 {
            let mut text = String::with_capacity(self.text.len() - self.unused);
            for index in 0..self.len() {
                let start = text.len();
                text.push_str(self.get(index));
                self.start[index] = start;
            }
            self.text = text;
            self.unused = 0;
        }
        Ok(())
    }

    /// `order` の順に並べ直す。
    fn permuted(self, order: &[u32]) -> Self {
        Self {
            start: order.iter().map(|&i| self.start[i as usize]).collect(),
            len: order.iter().map(|&i| self.len[i as usize]).collect(),
            text: self.text,
            unused: self.unused,
        }
    }
}

fn text_len(text: &str) -> Result<u32> {
    u32::try_from(text.len()).context("SFEN 行が長すぎます")
}

/// 昇順の SFEN の一覧から ply 抜き key を二分探索で引く。
fn find_key(sfens: &Texts, key: &str) -> Option<usize> {
    let (mut lo, mut hi) = (0, sfens.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match strip_ply(sfens.get(mid)).cmp(key) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(mid),
        }
    }
    None
}

/// 番号を値に持つ開番地法のハッシュ表。キーの比較とハッシュ値は呼び出し側が番号から求める。
#[derive(Default)]
struct IdTable {
    slots: Vec<u32>,
    len: usize,
}

impl IdTable {
    fn find(&self, hash: u64, eq: impl Fn(u32) -> bool) -> Option<u32> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut slot = hash as usize & mask;
        loop {
            match self.slots[slot] {
                NONE => return None,
                id if eq(id) => return Some(id),
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// 番号を加える。表を広げるときは `hash_of` で入っている番号のハッシュ値を求め直す。
    fn insert(&mut self, hash: u64, id: u32, hash_of: impl Fn(u32) -> u64) {
        if (self.len + 1) * 2 > self.slots.len() {
            let old = std::mem::replace(
                &mut self.slots,
                vec![NONE; ((self.len + 1) * 2).next_power_of_two().max(16)],
            );
            for id in old.into_iter().filter(|&id| id != NONE) {
                self.place(hash_of(id), id);
            }
        }
        self.place(hash, id);
        self.len += 1;
    }

    fn place(&mut self, hash: u64, id: u32) {
        let mask = self.slots.len() - 1;
        let mut slot = hash as usize & mask;
        while self.slots[slot] != NONE {
            slot = (slot + 1) & mask;
        }
        self.slots[slot] = id;
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

/// 盤 81 マスの駒 (5 bit ずつ)・手番・先手と後手の持ち駒を詰めた値。SFEN の ply 抜き key と 1 対 1 に対応する。
#[derive(Clone, Copy, PartialEq, Eq)]
struct PositionKey([u64; 8]);

impl PositionKey {
    /// 局面の値と、盤を 180 度回して先後を入れ替えた局面 (反転キー) の値。
    fn pair(pos: &Position) -> [Self; 2] {
        let mut words = [[0u64; 8]; 2];
        let mut put = |key: usize, bit: usize, width: usize, value: u64| {
            words[key][bit / 64] |= value << (bit % 64);
            if bit % 64 + width > 64 {
                words[key][bit / 64 + 1] |= value >> (64 - bit % 64);
            }
        };
        for sq in Square::all() {
            let pc = pos.piece_on(sq);
            let flipped = if pc.is_some() {
                Piece::new(pc.color().opponent(), pc.piece_type())
            } else {
                pc
            };
            // 駒の値は色 (bit 4) と駒種 (0..=14) で 31 以下。
            put(0, sq.index() * 5, 5, u64::from(pc.raw()));
            put(1, sq.inverse().index() * 5, 5, u64::from(flipped.raw()));
        }
        let side = pos.side_to_move();
        put(0, 405, 1, side.index() as u64);
        put(1, 405, 1, side.opponent().index() as u64);
        for (key, [first, second]) in [
            (0, [Color::Black, Color::White]),
            (1, [Color::White, Color::Black]),
        ] {
            put(key, 406, 32, u64::from(pos.hand(first).raw()));
            put(key, 438, 32, u64::from(pos.hand(second).raw()));
        }
        words.map(Self)
    }

    fn hash(&self) -> u64 {
        let mut hash = 0u64;
        for word in self.0 {
            hash = (hash ^ word).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
        }
        // 表の番地は下位 bit で決めるので、上位 bit も混ぜる。
        hash ^ (hash >> 32)
    }
}

/// 正規の SFEN の key を持つ局面を [`PositionKey`] で引く索引。
#[derive(Default)]
struct PositionIndex {
    keys: Vec<PositionKey>,
    nodes: Vec<u32>,
    table: IdTable,
}

impl PositionIndex {
    fn insert(&mut self, key: PositionKey, node: u32) {
        let id = self.keys.len() as u32;
        self.keys.push(key);
        self.nodes.push(node);
        let keys = &self.keys;
        self.table.insert(key.hash(), id, |id| keys[id as usize].hash());
    }

    fn get(&self, key: &PositionKey) -> Option<usize> {
        self.table
            .find(key.hash(), |id| self.keys[id as usize] == *key)
            .map(|id| self.nodes[id as usize] as usize)
    }
}

/// 逆伝播の集計。
#[derive(Debug, Default, Clone)]
pub struct PropagationStats {
    pub updated_moves: usize,
    /// 値が変わった手の、変化量の絶対値ごとの手数。
    pub abs_deltas: BTreeMap<i32, usize>,
    pub nontrivial_sccs: usize,
    pub max_scc_size: usize,
    pub draw_moves: usize,
    /// 伝播の深さごとの局面数。
    pub depths: BTreeMap<usize, usize>,
    /// best 値が変化した局面のうち、変化の大きい 20 局面 (同じ大きさは SFEN 順)。
    pub changed_nodes: Vec<NodeChange>,
    /// 自分・相手が王手をかけ続けられる循環の状態数 (先手・後手として解いた 2 回の合計)。
    pub check_core_states: usize,
    /// 同値手を選ぶと保証が破れるため、保証手だけを残した局面数。
    pub restricted_ties: usize,
}

/// report に載せる、best 値が変化した局面の数。
const TOP_CHANGES: usize = 20;

/// best 値が変化した局面。
#[derive(Debug, Clone)]
pub struct NodeChange {
    pub sfen: String,
    pub old_best: i32,
    pub new_best: i32,
}

/// 定跡を読み、逆伝播した `.db` と任意の report を書き出す (`book_backprop` の処理本体)。
pub fn backprop_file(
    book: &Path,
    out: &Path,
    report: Option<&Path>,
    draws: DrawValues,
    merge: MergeMode,
) -> Result<PropagationStats> {
    backprop_file_with(book, out, report, draws, merge, BackpropOptions::default())
}

/// [`backprop_file`] に追加オプションを指定する版。
pub fn backprop_file_with(
    book: &Path,
    out: &Path,
    report: Option<&Path>,
    draws: DrawValues,
    merge: MergeMode,
    options: BackpropOptions,
) -> Result<PropagationStats> {
    let mut graph = read_graph(book, options)?;
    let stats = propagate_values(&mut graph, draws, merge)?;
    write_backprop_book(&graph, out)?;
    if let Some(path) = report {
        write_report(&graph, &stats, path, merge, draws)?;
    }
    Ok(stats)
}

/// 入力を 1 行ずつ読み、行の先頭の位置を数える。
struct LineReader {
    inner: BufReader<File>,
    pos: u64,
    line: String,
}

impl LineReader {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            inner: BufReader::new(file),
            pos: 0,
            line: String::new(),
        })
    }

    /// 入力を読み直す。長さと、最初の `sfen` 行より前の行が 1 回目と同じか確かめる。
    fn reopen(path: &Path, source: &SourceCheck) -> Result<Self> {
        let mut reader =
            Self::open(path).with_context(|| format!("定跡を開けません: {}", path.display()))?;
        if reader.inner.get_ref().metadata()?.len() != source.len {
            bail!("{CHANGED}: {}", path.display());
        }
        let mut prefix = LineHash::default();
        while reader.pos < source.prefix_end {
            let Some((_, line)) = reader.next_line()? else {
                break;
            };
            prefix.line(line);
        }
        if reader.pos != source.prefix_end || prefix.finish() != source.prefix_hash {
            bail!("{CHANGED}: {}", path.display());
        }
        Ok(reader)
    }

    /// 読み直しを終えたときにも入力の長さが変わっていないか確かめる。読んでいる間に足された行は
    /// 局面の区切りの `sfen` 行で止まって読まないことがあるため。
    fn finish(self, path: &Path, source: &SourceCheck) -> Result<()> {
        if self.inner.get_ref().metadata()?.len() != source.len {
            bail!("{CHANGED}: {}", path.display());
        }
        Ok(())
    }

    /// 次の行と、その先頭の位置。
    fn next_line(&mut self) -> Result<Option<(u64, &str)>> {
        self.line.clear();
        let offset = self.pos;
        let read = self.inner.read_line(&mut self.line)?;
        if read == 0 {
            return Ok(None);
        }
        self.pos += read as u64;
        Ok(Some((offset, &self.line)))
    }

    fn seek(&mut self, offset: u64) -> Result<()> {
        if offset != self.pos {
            let delta = i64::try_from(offset)? - i64::try_from(self.pos)?;
            self.inner.seek_relative(delta)?;
            self.pos = offset;
        }
        Ok(())
    }
}

/// 定跡の 1 行の種類。
enum Line<'a> {
    /// 空行・`#` 行・`//` 行。
    Skip,
    /// `sfen` 行の SFEN。
    Sfen(&'a str),
    /// 指し手行。
    Row(&'a str),
}

fn classify(line: &str) -> Line<'_> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() || line.starts_with('#') || line.starts_with("//") {
        return Line::Skip;
    }
    match line.strip_prefix("sfen ") {
        Some(rest) => Line::Sfen(rest.trim()),
        None => Line::Row(line),
    }
}

/// 指し手行の欄。指し手と予想手の `none` / `None` / `resign` は `None`。
struct RowFields<'a> {
    move_usi: Option<&'a str>,
    ponder_usi: Option<&'a str>,
    value: i32,
    depth: i32,
    count: u64,
}

fn parse_row(line: &str) -> RowFields<'_> {
    fn field(token: &str) -> Option<&str> {
        match token {
            "none" | "None" | "resign" => None,
            other => Some(other),
        }
    }
    let mut tokens = line.split_whitespace();
    let move_usi = tokens.next().and_then(field);
    let ponder_usi = tokens.next().and_then(field);
    let value = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let depth = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let count = tokens.next().map_or(1, |t| t.parse::<u64>().unwrap_or(0));
    RowFields {
        move_usi,
        ponder_usi,
        value,
        depth,
        count,
    }
}

/// 行のハッシュ値。`sfen` 行から次の `sfen` 行の前まで (コメント・空行も含む) の内容を 1 回目と比べる。
#[derive(Default)]
struct LineHash(DefaultHasher);

impl LineHash {
    /// 改行を含む、読んだままの行を加える。
    fn line(&mut self, line: &str) {
        self.0.write(line.as_bytes());
    }

    fn finish(&self) -> u64 {
        self.0.finish()
    }
}

/// 1 回目の読み込みで確かめた入力の長さと、最初の `sfen` 行より前の行のハッシュ値。
/// `sfen` 行から次の `sfen` 行の前までは局面ごとのハッシュ値で比べるので、合わせて入力の全ての行を比べる。
#[derive(Clone, Copy, Default)]
struct SourceCheck {
    len: u64,
    prefix_end: u64,
    prefix_hash: u64,
}

/// 局面の `sfen` 行ごとのハッシュ値を入力の順にまとめる。
fn node_hash(blocks: impl IntoIterator<Item = u64>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for block in blocks {
        hasher.write_u64(block);
    }
    hasher.finish()
}

/// 局面の入力の行を、`sfen` 行ごとに入力の順で `f` へ渡す。読んだ内容が 1 回目と同じ (ハッシュ値が `expected`) か確かめる。
fn for_each_row(
    reader: &mut LineReader,
    blocks: &[u64],
    expected: u64,
    mut f: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut hashes = Vec::with_capacity(blocks.len());
    for &offset in blocks {
        reader.seek(offset)?;
        let mut hash = LineHash::default();
        match reader.next_line()? {
            Some((_, line)) if matches!(classify(line), Line::Sfen(_)) => hash.line(line),
            _ => bail!(CHANGED),
        }
        while let Some((_, line)) = reader.next_line()? {
            match classify(line) {
                Line::Skip => hash.line(line),
                Line::Sfen(_) => break,
                Line::Row(row) => {
                    hash.line(line);
                    f(row)?;
                }
            }
        }
        hashes.push(hash.finish());
    }
    if node_hash(hashes) != expected {
        bail!(CHANGED);
    }
    Ok(())
}

/// 1 回目の読み込みで集める局面の一覧。局面は ply 抜き key の昇順。
struct Scan {
    sfens: Texts,
    rows: Vec<u32>,
    block_start: Vec<u32>,
    blocks: Vec<u64>,
    /// 局面の `sfen` 行ごとの行のハッシュ値 ([`node_hash`])。
    hashes: Vec<u64>,
    check: SourceCheck,
}

/// 局面の key・代表 SFEN・行の数と、`sfen` 行の位置を集める。
fn scan(path: &Path) -> Result<Scan> {
    let mut reader = LineReader::open(path)?;
    let mut sfens = Texts::default();
    let mut plies = Vec::new();
    let mut rows: Vec<u32> = Vec::new();
    let mut table = IdTable::default();
    let mut blocks: Vec<(u32, u64, u64)> = Vec::new();
    let mut total_rows = 0u32;
    let mut current = None;
    let mut hash = LineHash::default();
    let mut check = SourceCheck::default();
    while let Some((offset, line)) = reader.next_line()? {
        match classify(line) {
            Line::Skip => hash.line(line),
            Line::Sfen(sfen) => {
                let finished = std::mem::take(&mut hash).finish();
                match blocks.last_mut() {
                    Some(block) => block.2 = finished,
                    None => {
                        check.prefix_end = offset;
                        check.prefix_hash = finished;
                    }
                }
                hash.line(line);
                let key = strip_ply(sfen);
                let hash = hash_bytes(key.as_bytes());
                let id = match table.find(hash, |id| strip_ply(sfens.get(id as usize)) == key) {
                    Some(id) => {
                        let ply = ply_of(sfen);
                        if ply < plies[id as usize] {
                            sfens.replace(id as usize, sfen)?;
                            plies[id as usize] = ply;
                        }
                        id
                    }
                    None => {
                        let id = sfens.push(sfen)?;
                        plies.push(ply_of(sfen));
                        rows.push(0);
                        let sfens = &sfens;
                        table.insert(hash, id, |id| {
                            hash_bytes(strip_ply(sfens.get(id as usize)).as_bytes())
                        });
                        id
                    }
                };
                blocks.push((id, offset, 0));
                current = Some(id);
            }
            Line::Row(_) => {
                hash.line(line);
                if let Some(id) = current {
                    total_rows = total_rows.checked_add(1).context("手が多すぎます")?;
                    rows[id as usize] += 1;
                }
            }
        }
    }
    match blocks.last_mut() {
        Some(block) => block.2 = hash.finish(),
        None => {
            check.prefix_end = reader.pos;
            check.prefix_hash = hash.finish();
        }
    }
    check.len = reader.pos;
    drop((reader, table, plies));
    let mut order: Vec<u32> = (0..sfens.len() as u32).collect();
    order.sort_unstable_by(|&a, &b| {
        strip_ply(sfens.get(a as usize)).cmp(strip_ply(sfens.get(b as usize)))
    });
    let mut rank = vec![0u32; order.len()];
    for (node, &id) in order.iter().enumerate() {
        rank[id as usize] = node as u32;
    }
    let rows = order.iter().map(|&id| rows[id as usize]).collect();
    let sfens = sfens.permuted(&order);
    drop(order);
    for block in &mut blocks {
        block.0 = rank[block.0 as usize];
    }
    blocks.sort_unstable();
    if u32::try_from(blocks.len()).is_err() {
        bail!("sfen 行が多すぎます");
    }
    let mut block_start = vec![0u32; sfens.len() + 1];
    for &(node, _, _) in &blocks {
        block_start[node as usize + 1] += 1;
    }
    for node in 0..sfens.len() {
        block_start[node + 1] += block_start[node];
    }
    let hashes = (0..sfens.len())
        .map(|node| {
            let range = block_start[node] as usize..block_start[node + 1] as usize;
            node_hash(blocks[range].iter().map(|&(_, _, hash)| hash))
        })
        .collect();
    Ok(Scan {
        sfens,
        rows,
        block_start,
        blocks: blocks.into_iter().map(|(_, offset, _)| offset).collect(),
        hashes,
        check,
    })
}

/// 行の指し手欄が `none` などで指し手を持たない。
const MOVE_NONE: u32 = u32::MAX;
/// `Move` の表記に戻らない指し手欄。元の文字列は別に持つ。
const MOVE_TEXT: u32 = 1 << 16;
/// `Move::from_usi` で読めない指し手欄。
const MOVE_INVALID: u32 = 1 << 17;

/// 2 回目の読み込みで集める入力の行。指し手は `Move` の下位 16 bit と [`MOVE_TEXT`] などの印で持つ。
struct InputRows {
    value: Vec<i32>,
    moves: Vec<u32>,
    /// [`MOVE_TEXT`] の行の元の指し手欄 (行番号の昇順)。
    texts: Vec<(u32, Box<str>)>,
    /// 局面の全行が depth=0 か。
    all_depth0: Vec<bool>,
    /// 局面の全行が value=0 かつ depth=0 か (行の無い局面も含む)。
    unexplored: Vec<bool>,
}

impl InputRows {
    /// 行の指し手欄の文字列。
    fn text(&self, row: usize) -> String {
        let mv = self.moves[row];
        if mv & MOVE_TEXT == 0 {
            return Move::from_u16(mv as u16).to_usi();
        }
        let index = self
            .texts
            .binary_search_by_key(&(row as u32), |&(row, _)| row)
            .expect("MOVE_TEXT の行は元の文字列を持つ");
        self.texts[index].1.to_string()
    }
}

/// 正規の USI 表記 (`7g7f` / `7g7f+` / `P*5e`) の指し手を、文字列を作らずに読む。他の表記は `None`。
/// 返す値は `Move::from_usi` と同じ。`1a1a` は `Move::NONE` (`none` と書く値) になるので除く。
fn canonical_move(text: &str) -> Option<Move> {
    let bytes = text.as_bytes();
    let square = |at: usize| {
        let (file, rank) = (bytes[at], bytes[at + 1]);
        ((b'1'..=b'9').contains(&file) && (b'a'..=b'i').contains(&rank))
            .then(|| Square::from_usi(&text[at..at + 2]))
            .flatten()
    };
    match bytes.len() {
        4 if bytes[1] == b'*' => {
            let piece = match bytes[0] {
                b'P' => PieceType::Pawn,
                b'L' => PieceType::Lance,
                b'N' => PieceType::Knight,
                b'S' => PieceType::Silver,
                b'G' => PieceType::Gold,
                b'B' => PieceType::Bishop,
                b'R' => PieceType::Rook,
                _ => return None,
            };
            Some(Move::new_drop(piece, square(2)?))
        }
        4 => Some(Move::new_move(square(0)?, square(2)?, false)).filter(|&mv| mv != Move::NONE),
        5 if bytes[4] == b'+' => Some(Move::new_move(square(0)?, square(2)?, true)),
        _ => None,
    }
}

/// 局面の順に入力を読み直し、行の値と指し手を集める。
fn read_rows(path: &Path, scan: &Scan) -> Result<InputRows> {
    let mut reader = LineReader::reopen(path, &scan.check)?;
    let total: usize = scan.rows.iter().map(|&rows| rows as usize).sum();
    let mut input = InputRows {
        value: Vec::with_capacity(total),
        moves: Vec::with_capacity(total),
        texts: Vec::new(),
        all_depth0: Vec::with_capacity(scan.rows.len()),
        unexplored: Vec::with_capacity(scan.rows.len()),
    };
    for node in 0..scan.rows.len() {
        let blocks =
            &scan.blocks[scan.block_start[node] as usize..scan.block_start[node + 1] as usize];
        let first = input.value.len();
        let (mut depth0, mut unexplored) = (true, true);
        for_each_row(&mut reader, blocks, scan.hashes[node], |line| {
            let fields = parse_row(line);
            depth0 &= fields.depth == 0;
            unexplored &= fields.value == 0 && fields.depth == 0;
            let row = input.value.len() as u32;
            input.value.push(fields.value);
            input.moves.push(match fields.move_usi {
                None => MOVE_NONE,
                Some(text) => match canonical_move(text) {
                    Some(mv) => u32::from(mv.raw()),
                    None => match Move::from_usi(text) {
                        Some(mv) if mv.to_usi() == text => u32::from(mv.raw()),
                        decoded => {
                            input.texts.push((row, text.into()));
                            decoded.map_or(MOVE_TEXT | MOVE_INVALID, |mv| {
                                MOVE_TEXT | u32::from(mv.raw())
                            })
                        }
                    },
                },
            });
            Ok(())
        })?;
        if input.value.len() - first != scan.rows[node] as usize {
            bail!("{CHANGED}: {}", path.display());
        }
        input.all_depth0.push(depth0);
        input.unexplored.push(unexplored);
    }
    reader.finish(path, &scan.check)?;
    Ok(input)
}

/// 定跡を読み、book 内子局面への辺 (反転 key 合流を含む) を張ったグラフを作る。
/// 同一 key の行は最小 ply の sfen を代表にして手を連結する。未探索の判定は入力時の depth を使う。
pub fn read_graph(path: &Path, options: BackpropOptions) -> Result<Graph> {
    let scan = scan(path)
        .with_context(|| format!("定跡を rshogi-book で読めません: {}", path.display()))?;
    let input = read_rows(path, &scan)?;
    build_graph(path, options, scan, input)
}

/// 指し手が合法でない理由。
enum Rejected {
    Unreadable,
    NotApplicable,
    Illegal,
}

/// 局面 `pos` で USI 指し手 (読めなかった欄は `None`) を合法性検査付きで解釈する。
fn legal_move(pos: &Position, decoded: Option<Move>) -> Result<Move, Rejected> {
    let decoded = decoded.ok_or(Rejected::Unreadable)?;
    let mv = pos.to_move(decoded).ok_or(Rejected::NotApplicable)?;
    if mv == Move::NONE || !pos.pseudo_legal(mv) || !pos.is_legal(mv) {
        return Err(Rejected::Illegal);
    }
    Ok(mv)
}

fn build_graph(
    path: &Path,
    options: BackpropOptions,
    scan: Scan,
    input: InputRows,
) -> Result<Graph> {
    let Scan {
        sfens,
        rows,
        block_start,
        blocks,
        hashes,
        check,
    } = scan;
    let n = sfens.len();
    let skipped: Vec<bool> = (0..n)
        .map(|v| options.skip_unsearched_children && rows[v] > 0 && input.all_depth0[v])
        .collect();
    // 子局面は SFEN 文字列を作らず、盤・手番・持ち駒の並び (SFEN の ply 抜き key と 1 対 1) で引く。
    // 反転は 1 対 1 で 2 回で戻るので、「子局面の反転キーが key の局面」と「反転キーが子局面に等しい局面」は同じ。
    // 引くだけで反復しないので、ハッシュの順序は出力に影響しない。
    let mut index = PositionIndex::default();
    let mut node_flags = Vec::with_capacity(n);
    let mut position = Position::new();
    for (v, &unexplored) in input.unexplored.iter().enumerate() {
        let sfen = sfens.get(v);
        position.set_sfen(sfen).map_err(|e| anyhow!("不正な SFEN: {sfen}: {e}"))?;
        let key = strip_ply(sfen);
        // 子局面は正規の SFEN (`to_sfen`) で表した key の局面にだけ合流する。持ち駒の並びなどが違う同じ局面の行は
        // 別の局面として残し、索引には入れない。
        if strip_ply(&position.to_sfen()) == key {
            index.insert(PositionKey::pair(&position)[0], v as u32);
        }
        let mut flags = 0;
        if key.split_whitespace().nth(1) != Some("w") {
            flags |= NODE_BLACK;
        }
        if !position.checkers().is_empty() {
            flags |= NODE_CHECKED;
        }
        if options.skip_unsearched_children && unexplored {
            flags |= NODE_PRESERVED;
        }
        node_flags.push(flags);
    }

    let total = input.value.len();
    let mut row_start = Vec::with_capacity(n + 1);
    row_start.push(0u32);
    let mut old = Vec::with_capacity(total);
    let mut to = Vec::with_capacity(total);
    let mut row_flags = Vec::with_capacity(total);
    let mut added = Vec::new();
    let mut flip_edges = 0;
    let mut illegal_moves = 0;
    let mut first = 0usize;
    for v in 0..n {
        let sfen = sfens.get(v);
        position.set_sfen(sfen).map_err(|e| anyhow!("不正な SFEN: {sfen}: {e}"))?;
        let input_rows = first..first + rows[v] as usize;
        first = input_rows.end;
        for row in input_rows.clone() {
            let mv = input.moves[row];
            let mut flags = 0;
            let mut edge = None;
            if mv != MOVE_NONE {
                let decoded = (mv & MOVE_INVALID == 0).then(|| Move::from_u16(mv as u16));
                match legal_move(&position, decoded) {
                    Ok(mv) => {
                        flags |= ROW_USABLE;
                        let gives_check = position.gives_check(mv);
                        position.do_move(mv, gives_check);
                        let [direct, flipped] = PositionKey::pair(&position);
                        edge = index
                            .get(&direct)
                            .map(|to| (to, false))
                            .or_else(|| index.get(&flipped).map(|to| (to, true)));
                        position.undo_move(mv);
                    }
                    Err(reason) => {
                        illegal_moves += 1;
                        let text = input.text(row);
                        let error = match reason {
                            Rejected::Unreadable => format!("USI 指し手が不正です: {text}"),
                            Rejected::NotApplicable => {
                                format!("局面に適用できない指し手です: {text}: {sfen}")
                            }
                            Rejected::Illegal => format!("非合法手です: {text}: {sfen}"),
                        };
                        eprintln!(
                            "警告: 非合法手を逆伝播から除外します: sfen={sfen} move={text} error={error}"
                        );
                    }
                }
            }
            if edge.is_some_and(|(to, _)| skipped[to]) {
                flags |= ROW_PRESERVE;
                edge = None;
            }
            if let Some((_, true)) = edge {
                flags |= ROW_FLIP;
                flip_edges += 1;
            }
            old.push(input.value[row]);
            to.push(edge.map_or(NONE, |(to, _)| to as u32));
            row_flags.push(flags);
        }
        if options.add_transposition_moves && node_flags[v] & NODE_PRESERVED == 0 {
            let listed: BTreeSet<String> = input_rows
                .filter(|&row| input.moves[row] != MOVE_NONE)
                .map(|row| input.text(row))
                .collect();
            let mut legal = MoveList::new();
            generate_legal_all(&position, &mut legal);
            for &mv in legal.iter() {
                let usi = mv.to_usi();
                if listed.contains(&usi) {
                    continue;
                }
                let gives_check = position.gives_check(mv);
                position.do_move(mv, gives_check);
                let child = position.to_sfen();
                position.undo_move(mv);
                let edge =
                    find_key(&sfens, strip_ply(&child)).map(|to| (to, false)).or_else(|| {
                        rshogi_book::flipped_key(&child)
                            .and_then(|flipped| find_key(&sfens, strip_ply(&flipped)))
                            .map(|to| (to, true))
                    });
                let Some((child, via_flip)) = edge.filter(|&(to, _)| !skipped[to]) else {
                    continue;
                };
                flip_edges += usize::from(via_flip);
                old.push(0);
                to.push(child as u32);
                row_flags.push(ROW_USABLE | ROW_ADDED | if via_flip { ROW_FLIP } else { 0 });
                added.push(mv.raw());
            }
        }
        row_start.push(u32::try_from(old.len()).context("手が多すぎます")?);
    }
    drop((input, index));
    Ok(Graph {
        source: Some(path.to_path_buf()),
        options,
        sfens,
        node_flags,
        row_start,
        source_check: check,
        block_start,
        blocks,
        hashes,
        new: old.clone(),
        old,
        to,
        row_flags,
        added,
        flip_edges,
        illegal_moves,
    })
}

/// 先手・後手をそれぞれ自分として後退解析し、候補手の値を逆伝播する。
pub fn propagate_values(
    graph: &mut Graph,
    draws: DrawValues,
    merge: MergeMode,
) -> Result<PropagationStats> {
    let params = BestParams {
        draws,
        merge,
        skip_unusable_moves: graph.options.skip_unusable_moves,
    };
    for draw in [draws.black, draws.white] {
        if !(-DRAW_VALUE_LIMIT..=DRAW_VALUE_LIMIT).contains(&draw) {
            bail!(
                "千日手の値は -{DRAW_VALUE_LIMIT}..={DRAW_VALUE_LIMIT} で指定してください: {draw}"
            );
        }
    }
    let mut stats = PropagationStats::default();
    let mut sides = Vec::with_capacity(2);
    for black in [true, false] {
        let game = SideGame::new(graph, params, black)?;
        let solution = book_retrograde::solve(&game)?;
        stats.check_core_states += solution.core_size[0] + solution.core_size[1];
        sides.push(game.into_side(solution));
    }
    // 局面の手番の側を自分として解いた結果は保存されている向き、反対の側の結果は反転キーで合流した向き。
    let results = |graph: &Graph, v: usize| sides[graph.color(v)].result(graph, params, v);
    let mirrored = |graph: &Graph, v: usize| sides[1 - graph.color(v)].result(graph, params, v);
    stats.restricted_ties = write_certified(graph, params, &results, &mirrored)?;
    drop(sides);

    let scc = Components::of(graph);
    stats.nontrivial_sccs = scc.nontrivial.iter().filter(|&&v| v).count();
    stats.max_scc_size = scc.max_size;
    let mut changed = Vec::new();
    for node in 0..graph.len() {
        let comp = scc.comp_of[node] as usize;
        let draw = draws.of(graph.black(node));
        let counted = || graph.rows(node).filter(|&row| params.counts(graph, row));
        let old_best = counted().map(|row| graph.old[row]).max().unwrap_or(draw);
        let new_best = counted().map(|row| graph.new[row]).max().unwrap_or(draw);
        if old_best != new_best {
            changed.push(((new_best - old_best).abs(), node as u32, old_best, new_best));
            if changed.len() >= 2 * TOP_CHANGES {
                top_changes(graph, &mut changed);
            }
        }
        *stats.depths.entry(scc.depth[comp] as usize).or_default() += 1;
        for row in graph.rows(node) {
            let (old, new) = (graph.old[row], graph.new[row]);
            if old != new {
                stats.updated_moves += 1;
                *stats.abs_deltas.entry((new - old).abs()).or_default() += 1;
            }
            if new == draw
                && graph
                    .edge_to(row)
                    .is_some_and(|to| scc.nontrivial[comp] && scc.comp_of[to] as usize == comp)
            {
                stats.draw_moves += 1;
            }
        }
    }
    top_changes(graph, &mut changed);
    stats.changed_nodes = changed
        .into_iter()
        .map(|(_, node, old_best, new_best)| NodeChange {
            sfen: graph.sfen(node as usize).to_string(),
            old_best,
            new_best,
        })
        .collect();
    Ok(stats)
}

/// best 値の変化 `(変化の大きさ, 局面, 変化前, 変化後)` を大きい順 (同じ大きさは SFEN 順) に並べ、上位だけを残す。
fn top_changes(graph: &Graph, changed: &mut Vec<(i32, u32, i32, i32)>) {
    changed.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| graph.sfen(a.1 as usize).cmp(graph.sfen(b.1 as usize)))
    });
    changed.truncate(TOP_CHANGES);
}

/// 局面の手番の側を自分として解いた結果。値は手番の側から見た値。
#[derive(Debug, Clone)]
struct NodeResult {
    value: i32,
    /// 行ごとの値と出口までの手数。best の計算に数えない行は `None`。
    rows: Vec<Option<(i32, Option<u32>)>>,
    guaranteed: Option<usize>,
    preferred: Option<usize>,
}

/// 局面ごとの [`NodeResult`] を作る関数。
type ResultsOf<'a> = dyn Fn(&Graph, usize) -> Option<NodeResult> + 'a;

/// 解いた値を書き出し値にし、保証検査を通るまで危ない同値手を保証手だけに絞る。絞った局面数を返す。
fn write_certified(
    graph: &mut Graph,
    params: BestParams,
    results: &ResultsOf<'_>,
    mirrored: &ResultsOf<'_>,
) -> Result<usize> {
    let n = graph.len();
    let mirrored_values: Vec<Option<i32>> =
        (0..n).map(|v| mirrored(graph, v).map(|m| m.value)).collect();
    // 同値手は値を保つ手の中から選び、他は保存時だけ -1 する。選ぶと保証が破れる局面は保証手だけを残す。
    let mut restricted = vec![false; n];
    loop {
        assign_outputs(graph, params, results, mirrored, &restricted);
        let conflicts = flip_conflicts(graph, params, mirrored);
        if conflicts > 0 {
            bail!(
                "反転キーで合流した {conflicts} 局面で、保存されている向きの行が反対の向きの値を保ちません。                 1 行では両方の向きを表せないので book は出力しません。千日手の値を先後で揃えてください"
            );
        }
        let violations = certify(graph, params, &mirrored_values);
        if violations.nodes.is_empty() {
            break;
        }
        let fresh: Vec<_> = violations.nodes.iter().copied().filter(|&v| !restricted[v]).collect();
        if fresh.is_empty() {
            bail!("{}", violations.message(graph));
        }
        for v in fresh {
            restricted[v] = true;
        }
    }
    let count = restricted.iter().filter(|&&r| r).count();
    if count > 0 {
        eprintln!("同値手を選ぶと保証が破れる {} 局面で、保証手以外の同値手を -1 しました", count);
    }
    Ok(count)
}

/// 反転キーで合流した向きで、書き出し値が最大の行のどれを指しても、その向きで解いた値を保つか。
/// 保たない局面の数を返す。千日手の値が先後で同じなら、両方の向きは同じ問題なので 0 になる。
fn flip_conflicts(graph: &Graph, params: BestParams, mirrored: &ResultsOf<'_>) -> usize {
    (0..graph.len())
        .filter(|&v| {
            let Some(mirror) = mirrored(graph, v) else {
                return false;
            };
            let rows = graph.rows(v);
            let best = rows
                .clone()
                .filter(|&row| params.counts(graph, row))
                .map(|row| graph.new[row])
                .max();
            rows.enumerate().any(|(idx, row)| {
                params.counts(graph, row)
                    && Some(graph.new[row]) == best
                    && mirror.rows[idx].is_none_or(|(value, _)| value != mirror.value)
            })
        })
        .count()
}

/// 先手 (`black`) か後手を自分とした後退解析の問題。千日手の値を自分の側の値に固定した 2 人零和の問題で、
/// 値は自分から見て千日手が 0 になるようにずらす。
///
/// 局面と実際の手番の組を頂点にする。反転キーで合流した辺は保存されている局面の手番と実際の手番が逆になるが、
/// 手を指すごとに実際の手番は必ず入れ替わる。`min` では、相手が book 外で咎められる (自分の手の探索ラベル)
/// ことを、自分の手と行き先の間に置いた相手の分岐 (ラベルの値の葉か、そのまま進むか) で表す。
/// 手は 4 バイトの [`Target`] で持つ。
struct SideGame<'a> {
    graph: &'a Graph,
    params: BestParams,
    /// 自分の色の番号 (0 = 先手、1 = 後手)。
    me: usize,
    draw: i32,
    /// `state[v][c]`: 局面 `v` を実際の手番 `c` で指す頂点。届かない向きは `NONE`。
    state: Vec<[u32; 2]>,
    /// 局面と手番の頂点の `局面 * 2 + 手番`。分岐の頂点はこの後ろに並ぶ。
    vertex: Vec<u32>,
    /// `min` の分岐の頂点が表す行。
    branch_row: Vec<u32>,
    /// `min` のとき、局面の自分の手番の頂点で、分岐を挟む最初の行の分岐の頂点。
    /// 同じ局面の分岐の頂点は行の順に連番になる。
    branch_base: Vec<u32>,
    /// 頂点 `s` の手 `j` は `targets[arc_start[s] + j]`。
    arc_start: Vec<u32>,
    targets: Vec<Target>,
    /// `skip_unusable_moves` のとき、局面 `v` の手番号 `j` の行は `counted[counted_start[v] + j]`。
    counted_start: Vec<u32>,
    counted: Vec<u32>,
}

/// 後退解析の手を 4 バイトに詰めた値。
///
/// - 最上位 bit が 0: 行き先の頂点 (下位 30 bit) と、王手をかける手か (その上の bit)。
///   局面を進めない分岐の手は、分岐の頂点の 2 番目の手として手の位置で分かる。
/// - `Target::ROW_LEAF`: 葉。値は行の値から求め直す。
/// - それ以外: 葉で、値 (解く側から見た値) を `LEAF_BIAS` だけずらして下位 31 bit に持つ。
///   数千万本の葉の値を 1 回の読み出しで得るため。収まらない値は `ROW_LEAF` にする。
#[derive(Clone, Copy)]
struct Target(u32);

impl Target {
    const LEAF: u32 = 1 << 31;
    const ROW_LEAF: Self = Self(u32::MAX);
    const LEAF_BIAS: i32 = 1 << 30;
    const CHECK: u32 = 1 << 30;
    const VERTEX: u32 = Self::CHECK - 1;

    fn vertex(to: u32, check: bool) -> Self {
        Self(to | if check { Self::CHECK } else { 0 })
    }

    fn leaf(value: i32) -> Self {
        match value.checked_add(Self::LEAF_BIAS) {
            Some(biased) if biased >= 0 && (biased as u32) < Self::LEAF - 1 => {
                Self(Self::LEAF | biased as u32)
            }
            _ => Self::ROW_LEAF,
        }
    }
}

impl<'a> SideGame<'a> {
    fn new(graph: &'a Graph, params: BestParams, black: bool) -> Result<Self> {
        let n = graph.len();
        if n > (NONE / 2) as usize {
            bail!("局面が多すぎて後退解析できません: {n}");
        }
        let (mut counted_start, mut counted) = (Vec::new(), Vec::new());
        if params.skip_unusable_moves {
            counted_start.push(0);
            for v in 0..n {
                counted.extend(
                    graph.rows(v).filter(|&row| params.counts(graph, row)).map(|row| row as u32),
                );
                counted_start.push(counted.len() as u32);
            }
        }
        let mut game = Self {
            graph,
            params,
            me: usize::from(!black),
            draw: params.draws.of(black),
            state: Vec::new(),
            vertex: Vec::new(),
            branch_row: Vec::new(),
            branch_base: Vec::new(),
            arc_start: Vec::new(),
            targets: Vec::new(),
            counted_start,
            counted,
        };
        let mut reachable = vec![[false; 2]; n];
        let mut stack: Vec<u32> = (0..n).map(|v| (v * 2 + graph.color(v)) as u32).collect();
        for &s in &stack {
            reachable[s as usize / 2][s as usize % 2] = true;
        }
        while let Some(s) = stack.pop() {
            let (v, c) = (s as usize / 2, s as usize % 2);
            for j in 0..game.moves(v) {
                if let Some(to) = graph.edge_to(game.row(v, j))
                    && !reachable[to][1 - c]
                {
                    reachable[to][1 - c] = true;
                    stack.push((to * 2 + 1 - c) as u32);
                }
            }
        }
        drop(stack);
        game.state = vec![[NONE; 2]; n];
        for (v, reachable) in reachable.iter().enumerate() {
            for (c, &reachable) in reachable.iter().enumerate() {
                if reachable {
                    game.state[v][c] = target_vertex(game.vertex.len())?;
                    game.vertex.push((v * 2 + c) as u32);
                }
            }
        }
        drop(reachable);
        let min = params.merge == MergeMode::Min;
        if min {
            game.branch_base = vec![NONE; n];
        }
        // 分岐の頂点は、局面と手番の頂点の順・行の順に番号を振る。
        let base = game.vertex.len();
        game.arc_start.push(0);
        for s in 0..base {
            let (v, c) = (game.vertex[s] as usize / 2, game.vertex[s] as usize % 2);
            for j in 0..game.moves(v) {
                let row = game.row(v, j);
                let Some(to) = graph.edge_to(row) else {
                    let old = graph.old[row];
                    let mine = if c == game.me { old } else { -old };
                    game.targets.push(Target::leaf(mine - game.draw));
                    continue;
                };
                let target = if min && c == game.me && !graph.added(row) {
                    let branch = target_vertex(base + game.branch_row.len())?;
                    if game.branch_base[v] == NONE {
                        game.branch_base[v] = branch;
                    }
                    game.branch_row.push(row as u32);
                    branch
                } else {
                    game.state[to][1 - c]
                };
                game.targets.push(Target::vertex(target, graph.checked(to)));
            }
            game.arc_start.push(arc_index(game.targets.len())?);
        }
        for b in 0..game.branch_row.len() {
            let row = game.branch_row[b] as usize;
            let to = graph.to[row] as usize;
            game.targets.push(Target::leaf(graph.old[row] - game.draw));
            game.targets.push(Target::vertex(game.state[to][1 - game.me], false));
            game.arc_start.push(arc_index(game.targets.len())?);
        }
        Ok(game)
    }

    /// 局面 `v` の best の計算に数える行の数。
    fn moves(&self, v: usize) -> usize {
        if self.params.skip_unusable_moves {
            (self.counted_start[v + 1] - self.counted_start[v]) as usize
        } else {
            self.graph.rows(v).len()
        }
    }

    /// 局面 `v` の `j` 番目の数える行。
    fn row(&self, v: usize, j: usize) -> usize {
        if self.params.skip_unusable_moves {
            self.counted[self.counted_start[v] as usize + j] as usize
        } else {
            self.graph.row_start[v] as usize + j
        }
    }

    /// 解いた結果のうち、書き出しに使う分だけを残す。
    fn into_side(self, solution: book_retrograde::Solution) -> Side {
        let graph = self.graph;
        let n = graph.len();
        let mut guaranteed = vec![NONE; n];
        let mut preferred = vec![NONE; n];
        for v in 0..n {
            let s = self.state[v][self.me];
            if s == NONE {
                continue;
            }
            // 頂点内の手番号を局面内の行番号にする。
            let index = |j: u32| match j {
                NONE => NONE,
                j => (self.row(v, j as usize) - graph.row_start[v] as usize) as u32,
            };
            guaranteed[v] = index(solution.guaranteed[s as usize]);
            preferred[v] = index(solution.preferred[s as usize]);
        }
        Side {
            me: self.me,
            draw: self.draw,
            state: self.state,
            branch_base: self.branch_base,
            values: solution.values,
            distance: solution.distance,
            guaranteed,
            preferred,
        }
    }
}

/// 頂点番号にする。[`Target`] に収まるよう 30 bit に限る。
fn target_vertex(index: usize) -> Result<u32> {
    u32::try_from(index)
        .ok()
        .filter(|&id| id <= Target::VERTEX)
        .ok_or_else(|| anyhow!("頂点が多すぎて後退解析できません"))
}

fn arc_index(index: usize) -> Result<u32> {
    u32::try_from(index).map_err(|_| anyhow!("手が多すぎて後退解析できません"))
}

impl book_retrograde::Game for SideGame<'_> {
    fn vertices(&self) -> usize {
        self.vertex.len() + self.branch_row.len()
    }

    fn own(&self, s: usize) -> bool {
        s < self.vertex.len() && self.vertex[s] as usize % 2 == self.me
    }

    fn degree(&self, s: usize) -> usize {
        (self.arc_start[s + 1] - self.arc_start[s]) as usize
    }

    fn arc(&self, s: usize, j: usize) -> Arc {
        let Target(target) = self.targets[self.arc_start[s] as usize + j];
        if target & Target::LEAF == 0 {
            let step = if s >= self.vertex.len() {
                Step::Pass
            } else if target & Target::CHECK != 0 {
                Step::Check
            } else {
                Step::Quiet
            };
            return Arc {
                to: target & Target::VERTEX,
                leaf: 0,
                step,
            };
        }
        let leaf = if target != Target::ROW_LEAF.0 {
            (target & !Target::LEAF) as i32 - Target::LEAF_BIAS
        } else {
            match self.vertex.get(s) {
                Some(&vc) => {
                    let old = self.graph.old[self.row(vc as usize / 2, j)];
                    let mine = if vc as usize % 2 == self.me {
                        old
                    } else {
                        -old
                    };
                    mine - self.draw
                }
                None => self.graph.old[self.branch_row[s - self.vertex.len()] as usize] - self.draw,
            }
        };
        Arc {
            to: NONE,
            leaf,
            step: Step::Quiet,
        }
    }

    fn win(&self) -> [i32; 2] {
        let mate = Value::MATE.raw();
        [mate - self.draw, mate + self.draw]
    }
}

/// 先手か後手を自分として解いた結果のうち、書き出しに使う分。
struct Side {
    me: usize,
    draw: i32,
    state: Vec<[u32; 2]>,
    /// `min` のとき、局面の自分の手番の頂点で、分岐を挟む最初の行の分岐の頂点。
    /// 同じ局面の分岐の頂点は行の順に連番になる。
    branch_base: Vec<u32>,
    /// 頂点の値と出口までの手数 (`NONE` は決まらない)。
    values: Vec<i32>,
    distance: Vec<u32>,
    /// 局面の自分の手番の頂点で、保証手・選ぶ手の局面内の行番号。
    guaranteed: Vec<u32>,
    preferred: Vec<u32>,
}

impl Side {
    /// 局面 `v` を実際の手番の側を自分として解いた結果。その向きで届かない局面は `None`。
    fn result(&self, graph: &Graph, params: BestParams, v: usize) -> Option<NodeResult> {
        let s = self.state[v][self.me];
        if s == NONE {
            return None;
        }
        let draw = self.draw;
        let mut branch = self.branch_base.get(v).copied().unwrap_or(NONE);
        let rows = graph
            .rows(v)
            .map(|row| {
                if !params.counts(graph, row) {
                    return None;
                }
                let Some(to) = graph.edge_to(row) else {
                    let leaf = graph.old[row] - draw;
                    return Some((leaf + draw, Some(0)));
                };
                let target = if params.merge == MergeMode::Min && !graph.added(row) {
                    branch += 1;
                    branch - 1
                } else {
                    self.state[to][1 - self.me]
                } as usize;
                let distance = (self.distance[target] != NONE).then(|| self.distance[target] + 1);
                Some((self.values[target] + draw, distance))
            })
            .collect();
        let row = |j: u32| (j != NONE).then_some(j as usize);
        Some(NodeResult {
            value: self.values[s as usize] + draw,
            rows,
            guaranteed: row(self.guaranteed[v]),
            preferred: row(self.preferred[v]),
        })
    }
}

/// 解いた値を書き出す値にする。値が best と同じ行は、選ぶ手と、それと同じ手数で出口へ向かう手だけを残し、
/// 他は -1 する (保証手だけを残す局面では選ぶ手以外を全て -1)。補正は書き出しだけで親へ伝えない。
///
/// 反転キーで合流した向きにも届く局面では、その向きでも値を保つ手から選ぶ。千日手の値が先後で違うと、
/// 保存されている向きで短く出口へ向かう手が、反対の向きでは最善でないことがあるため。
fn assign_outputs(
    graph: &mut Graph,
    params: BestParams,
    results: &ResultsOf<'_>,
    mirrored: &ResultsOf<'_>,
    restricted: &[bool],
) {
    for (node, &restricted) in restricted.iter().enumerate() {
        let preserved = graph.preserved(node);
        let result = results(graph, node);
        let mirror = mirrored(graph, node);
        let common = |idx: usize| {
            mirror
                .as_ref()
                .is_none_or(|m| m.rows[idx].is_some_and(|(value, _)| value == m.value))
        };
        let chosen = result.as_ref().and_then(|r| {
            if restricted {
                return r.guaranteed;
            }
            if let Some(c) = [r.preferred, r.guaranteed].into_iter().flatten().find(|&c| common(c))
            {
                return Some(c);
            }
            // 両方の向きで値を保つ手のうち、勝ちは短く、負けは長く出口へ向かう手。
            let winning = r.value >= params.draws.of(graph.black(node));
            let candidates = (0..r.rows.len()).filter(|&idx| {
                r.rows[idx].is_some_and(|(value, _)| value == r.value) && common(idx)
            });
            let key = |idx: usize| {
                let distance = r.rows[idx].and_then(|(_, d)| d);
                match (distance, winning) {
                    (Some(d), true) => (0, i64::from(d)),
                    (Some(d), false) => (0, -i64::from(d)),
                    (None, _) => (1, 0),
                }
            };
            candidates.min_by_key(|&idx| key(idx)).or(r.preferred)
        });
        let chosen_distance =
            result.as_ref().zip(chosen).and_then(|(r, c)| r.rows[c].and_then(|(_, d)| d));
        for (idx, row) in graph.rows(node).enumerate() {
            let value = match result.as_ref().and_then(|r| r.rows[idx]) {
                Some((value, distance))
                    if !preserved && !graph.preserve(row) && graph.usable(row) =>
                {
                    let best = result.as_ref().is_some_and(|r| r.value == value);
                    let keep = chosen == Some(idx)
                        || (!restricted
                            && distance.is_some()
                            && distance == chosen_distance
                            && common(idx));
                    if best && !keep {
                        // 最小の合法値 -MATE は据え置き、-INFINITE を生成しない。
                        value.saturating_sub(1).max(-Value::MATE.raw())
                    } else {
                        value
                    }
                }
                _ => graph.old[row],
            };
            graph.new[row] = value;
        }
    }
}

/// 保証検査で見つかった局面。
struct Violations {
    /// 保存されている向きで約束が破られる局面。
    nodes: BTreeSet<usize>,
    perpetual_check: BTreeSet<usize>,
    repetition: BTreeSet<usize>,
}

impl Violations {
    fn message(&self, graph: &Graph) -> String {
        let sample = |nodes: &BTreeSet<usize>| {
            nodes.iter().take(5).map(|&v| graph.key(v)).collect::<Vec<_>>().join(" / ")
        };
        format!(
            "逆伝播した値の保証検査に失敗しました。book は出力しません: 連続王手で負ける循環へ持ち込まれる {} 局面 [{}]、千日手へ逃げられる {} 局面 [{}]",
            self.perpetual_check.len(),
            sample(&self.perpetual_check),
            self.repetition.len(),
            sample(&self.repetition)
        )
    }
}

/// 書き出す値で定跡を指したとき、循環によって値の約束が破られないかを、先手・後手をそれぞれ自分として検査する。
///
/// 自分は各局面で書き出し値が最大の手 (同値の -1 補正を受けなかった手) のどれを選んでもよく、
/// 相手は全手を選べるとして、次のどちらかの局面を返す。
/// - 詰まされる値でないのに、相手が自分の連続王手の循環 (反則負け) へ持ち込める。
/// - 自分の千日手の値より良い値なのに、相手が王手をやめながら続けられる循環 (千日手) へ持ち込める。
///
/// 値の整合は前提にせず、約束した局面から悪い循環へ届くかを到達可能性で調べる。
fn certify(graph: &Graph, params: BestParams, mirrored: &[Option<i32>]) -> Violations {
    let mut violations = Violations {
        nodes: BTreeSet::new(),
        perpetual_check: BTreeSet::new(),
        repetition: BTreeSet::new(),
    };
    let mate = Value::MATE.raw();
    for black in [true, false] {
        let states = CertifyStates::new(graph, params, mirrored, black);
        let draw = params.draws.of(black);
        let follows = |s: usize, idx: usize, _t: usize| states.follows(s, idx);

        // 自分の手が全て王手になる循環。強連結成分の状態はどれもその成分内の循環に乗る。
        let (comp, cyclic) = states.components(|s, idx, t| {
            states.follows(s, idx) && (states.is_opponent(s) || graph.checked(t / 2))
        });
        let seeds: Vec<bool> = comp.iter().map(|&c| cyclic[c as usize]).collect();
        drop(comp);
        let perpetual = states.violations(&states.reach(seeds, follows), |s| states.own(s) > -mate);

        // 相手が王手をしない手を含む循環。成分内の辺はどれもその成分内の循環に乗るので、
        // そうした相手の手が成分内にあれば、相手が王手を続けて負ける循環ではない。
        let (comp, _) = states.components(follows);
        let mut repeats = vec![false; comp.len()];
        for s in (0..states.len()).filter(|&s| states.is_opponent(s)) {
            if states.children(s).any(|(_, t)| !graph.checked(t / 2) && comp[s] == comp[t]) {
                repeats[comp[s] as usize] = true;
            }
        }
        let seeds: Vec<bool> = comp.iter().map(|&c| repeats[c as usize]).collect();
        drop(comp);
        let repetition = states.violations(&states.reach(seeds, follows), |s| states.own(s) > draw);

        violations.nodes.extend(perpetual.iter().chain(&repetition));
        violations.perpetual_check.extend(perpetual);
        violations.repetition.extend(repetition);
    }
    violations
}

/// [`certify`] が辿る、局面 `v` と実際の手番 `c` (0 = 先手、1 = 後手) を組にした状態 `2 * v + c` のグラフ。
///
/// 反転キーで合流した辺は保存されている局面の手番と実際の手番が逆になるので、実際の手番で自分か相手かを判定する。
struct CertifyStates<'a> {
    graph: &'a Graph,
    params: BestParams,
    /// 自分の手番の状態の偶奇。
    me: usize,
    values: Vec<i32>,
    /// 局面 `v` へ入る辺 `(親局面, 親の局面内の行番号)` は `reverse[reverse_start[v]..reverse_start[v + 1]]`。
    reverse_start: Vec<u32>,
    reverse: Vec<(u32, u32)>,
    /// 保存されている向きの局面から指して届く状態。反転キーで合流した向きはここに含まれるものだけが実在する。
    real: Vec<bool>,
    /// 反転キーで合流した向きで解いた値 (手番の側から見た値)。
    mirrored: &'a [Option<i32>],
}

impl<'a> CertifyStates<'a> {
    fn new(graph: &'a Graph, params: BestParams, mirrored: &'a [Option<i32>], black: bool) -> Self {
        let n = graph.len();
        let values = (0..n)
            .map(|v| {
                graph
                    .rows(v)
                    .filter(|&row| params.counts(graph, row))
                    .map(|row| graph.new[row])
                    .max()
                    .unwrap_or(params.draws.of(graph.black(v)))
            })
            .collect();
        let edges = || {
            (0..n).flat_map(move |node| {
                graph.rows(node).enumerate().filter_map(move |(idx, row)| {
                    graph
                        .edge_to(row)
                        .filter(|_| params.counts(graph, row))
                        .map(|to| (node, idx, to))
                })
            })
        };
        let mut reverse_start = vec![0u32; n + 1];
        for (_, _, to) in edges() {
            reverse_start[to + 1] += 1;
        }
        for v in 0..n {
            reverse_start[v + 1] += reverse_start[v];
        }
        let mut next = reverse_start.clone();
        let mut reverse = vec![(0, 0); reverse_start[n] as usize];
        for (node, idx, to) in edges() {
            reverse[next[to] as usize] = (node as u32, idx as u32);
            next[to] += 1;
        }
        drop(next);
        let mut states = Self {
            graph,
            params,
            me: usize::from(!black),
            values,
            reverse_start,
            reverse,
            real: vec![false; 2 * n],
            mirrored,
        };
        let mut stack: Vec<usize> = (0..n).map(|v| 2 * v + graph.color(v)).collect();
        for &s in &stack {
            states.real[s] = true;
        }
        while let Some(s) = stack.pop() {
            let children: Vec<usize> = states.children(s).map(|(_, t)| t).collect();
            for t in children {
                if !states.real[t] {
                    states.real[t] = true;
                    stack.push(t);
                }
            }
        }
        states
    }

    fn len(&self) -> usize {
        self.graph.len() * 2
    }

    fn is_opponent(&self, s: usize) -> bool {
        s % 2 != self.me
    }

    /// 自分から見た状態の値。行の値は実際に手番の側から見た値として使われる。
    /// 反転キーで合流した向きの局面では、書き出す値は保存されている向きの値なので、その向きで解いた値を約束とする。
    /// 書き出す最大値の行がその値を保つことは [`flip_conflicts`] で確かめてある。
    fn own(&self, s: usize) -> i32 {
        let v = s / 2;
        let stored = s % 2 == self.graph.color(v);
        let value = if stored {
            self.values[v]
        } else {
            self.mirrored[v].unwrap_or(self.values[v])
        };
        if self.is_opponent(s) { -value } else { value }
    }

    /// 自分は書き出し値が最大の手だけを、相手は全手を指しうる。
    /// 相手の探索ラベルが悪くても、book どおりに指す側は book 外の咎める手を指さないので相手の選択肢に残す。
    fn follows(&self, s: usize, idx: usize) -> bool {
        let v = s / 2;
        self.is_opponent(s)
            || self.graph.new[self.graph.row_start[v] as usize + idx] == self.values[v]
    }

    fn children(&self, s: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let flip = 1 - s % 2;
        let graph = self.graph;
        graph.rows(s / 2).enumerate().filter_map(move |(idx, row)| {
            graph
                .edge_to(row)
                .filter(|_| self.params.counts(graph, row))
                .map(|to| (idx, to * 2 + flip))
        })
    }

    fn parents(&self, t: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let flip = 1 - t % 2;
        let v = t / 2;
        self.reverse[self.reverse_start[v] as usize..self.reverse_start[v + 1] as usize]
            .iter()
            .map(move |&(node, idx)| (node as usize * 2 + flip, idx as usize))
    }

    /// `allow(親, 手番号, 子)` を満たす辺だけで強連結成分を求め、成分番号と各成分が循環を持つかを返す。
    /// 辺は必ず手番を入れ替えるので自己ループはなく、2 状態以上の成分だけが循環を持つ。
    fn components(&self, allow: impl Fn(usize, usize, usize) -> bool) -> (Vec<u32>, Vec<bool>) {
        let n = self.len();
        let mut visited = vec![false; n];
        let mut order = Vec::with_capacity(n);
        let mut dfs = Vec::new();
        for root in 0..n {
            if visited[root] {
                continue;
            }
            visited[root] = true;
            dfs.push((root, self.children(root)));
            while let Some((s, children)) = dfs.last_mut() {
                let s = *s;
                match children.find(|&(idx, t)| !visited[t] && allow(s, idx, t)) {
                    Some((_, t)) => {
                        visited[t] = true;
                        dfs.push((t, self.children(t)));
                    }
                    None => {
                        order.push(s as u32);
                        dfs.pop();
                    }
                }
            }
        }
        drop(visited);
        let mut comp = vec![u32::MAX; n];
        let mut cyclic = Vec::new();
        let mut stack = Vec::new();
        for &root in order.iter().rev() {
            let root = root as usize;
            if comp[root] != u32::MAX {
                continue;
            }
            let id = cyclic.len() as u32;
            comp[root] = id;
            stack.push(root);
            let mut size = 0;
            while let Some(t) = stack.pop() {
                size += 1;
                for (s, idx) in self.parents(t) {
                    if comp[s] == u32::MAX && allow(s, idx, t) {
                        comp[s] = id;
                        stack.push(s);
                    }
                }
            }
            cyclic.push(size > 1);
        }
        (comp, cyclic)
    }

    /// `allow` を満たす辺を逆に辿り、`seeds` の状態へ届く状態を返す。
    fn reach(
        &self,
        mut seeds: Vec<bool>,
        allow: impl Fn(usize, usize, usize) -> bool,
    ) -> Vec<bool> {
        let mut stack: Vec<usize> = (0..seeds.len()).filter(|&s| seeds[s]).collect();
        while let Some(t) = stack.pop() {
            for (s, idx) in self.parents(t) {
                if !seeds[s] && allow(s, idx, t) {
                    seeds[s] = true;
                    stack.push(s);
                }
            }
        }
        seeds
    }

    /// 届く状態のうち、自分の手番で `promises` を満たす値を約束している局面を返す。
    /// 相手の手番の値は相手の約束で、`min` では相手側の上界なので自分の約束にはならない。
    /// 相手の手番から悪い循環へ届く道は自分の手番の局面を通るので、ここで見落としは出ない。
    /// 反転キーで合流した向きも、保存されている向きの行がその向きの値を保つと確かめてから数える。
    fn violations(&self, reached: &[bool], promises: impl Fn(usize) -> bool) -> BTreeSet<usize> {
        (0..self.len())
            .filter(|&s| !self.is_opponent(s) && self.real[s] && reached[s] && promises(s))
            .map(|s| s / 2)
            .collect()
    }
}

/// book 内の子局面への辺の強連結成分。
struct Components {
    comp_of: Vec<u32>,
    nontrivial: Vec<bool>,
    /// 成分から縮約したグラフの末端までの最長の辺の数。
    depth: Vec<u32>,
    max_size: usize,
}

impl Components {
    /// Kosaraju 法で求める。成分は縮約したグラフの位相順に番号が付き、成分をまたぐ辺は番号の大きい成分へ向かう。
    fn of(graph: &Graph) -> Self {
        let n = graph.len();
        let mut start = vec![0u32; n + 1];
        let mut children = Vec::new();
        let mut buf = Vec::new();
        for v in 0..n {
            buf.clear();
            buf.extend(graph.rows(v).filter_map(|row| graph.edge_to(row).map(|t| t as u32)));
            buf.sort_unstable();
            buf.dedup();
            children.extend_from_slice(&buf);
            start[v + 1] = children.len() as u32;
        }
        let children_of = |v: usize| &children[start[v] as usize..start[v + 1] as usize];

        let mut visited = vec![false; n];
        let mut order = Vec::with_capacity(n);
        let mut stack: Vec<(u32, u32)> = Vec::new();
        for root in 0..n {
            if visited[root] {
                continue;
            }
            visited[root] = true;
            stack.push((root as u32, 0));
            while let Some(top) = stack.last_mut() {
                let (v, k) = (top.0 as usize, top.1 as usize);
                match children_of(v).get(k) {
                    Some(&child) => {
                        top.1 += 1;
                        if !visited[child as usize] {
                            visited[child as usize] = true;
                            stack.push((child, 0));
                        }
                    }
                    None => {
                        order.push(v as u32);
                        stack.pop();
                    }
                }
            }
        }
        drop(visited);

        let mut reverse_start = vec![0u32; n + 1];
        for &child in &children {
            reverse_start[child as usize + 1] += 1;
        }
        for v in 0..n {
            reverse_start[v + 1] += reverse_start[v];
        }
        let mut next = reverse_start.clone();
        let mut reverse = vec![0u32; children.len()];
        for v in 0..n {
            for &child in children_of(v) {
                reverse[next[child as usize] as usize] = v as u32;
                next[child as usize] += 1;
            }
        }
        drop(next);

        let mut comp_of = vec![NONE; n];
        let mut sizes = Vec::new();
        let mut members = Vec::new();
        for &root in order.iter().rev() {
            if comp_of[root as usize] != NONE {
                continue;
            }
            let id = sizes.len() as u32;
            comp_of[root as usize] = id;
            members.push(root);
            let mut size = 0usize;
            while let Some(w) = members.pop() {
                size += 1;
                let w = w as usize;
                for &parent in &reverse[reverse_start[w] as usize..reverse_start[w + 1] as usize] {
                    if comp_of[parent as usize] == NONE {
                        comp_of[parent as usize] = id;
                        members.push(parent);
                    }
                }
            }
            sizes.push(size);
        }
        drop((order, reverse, reverse_start));

        let comps = sizes.len();
        let mut nontrivial: Vec<bool> = sizes.iter().map(|&size| size > 1).collect();
        let mut by_comp_start = vec![0u32; comps + 1];
        for &c in &comp_of {
            by_comp_start[c as usize + 1] += 1;
        }
        for c in 0..comps {
            by_comp_start[c + 1] += by_comp_start[c];
        }
        let mut next = by_comp_start.clone();
        let mut by_comp = vec![0u32; n];
        for (v, &c) in comp_of.iter().enumerate() {
            by_comp[next[c as usize] as usize] = v as u32;
            next[c as usize] += 1;
        }
        drop(next);
        let mut depth = vec![0u32; comps];
        for c in (0..comps).rev() {
            for &v in &by_comp[by_comp_start[c] as usize..by_comp_start[c + 1] as usize] {
                for &child in children_of(v as usize) {
                    let to = comp_of[child as usize] as usize;
                    if to == c {
                        nontrivial[c] = true;
                    } else {
                        depth[c] = depth[c].max(depth[to] + 1);
                    }
                }
            }
        }
        Self {
            comp_of,
            nontrivial,
            depth,
            max_size: sizes.iter().copied().max().unwrap_or(0),
        }
    }
}

/// 書き出す 1 行。指し手欄と予想手欄は局面ごとの文字列の範囲。
struct OutRow {
    move_usi: Range<usize>,
    ponder_usi: Range<usize>,
    depth: i32,
    count: u64,
}

/// 逆伝播後の値で `.db` を書き出す。局面は key 昇順、手は count 降順 → USI 昇順。
/// 指し手欄・予想手欄・depth・count は入力を読み直して書く。入力と同じパスにも書けるよう、
/// 同じディレクトリの一時ファイルに書いてから置き換える。
pub fn write_backprop_book(graph: &Graph, out: &Path) -> Result<()> {
    let source = graph
        .source
        .as_deref()
        .ok_or_else(|| anyhow!("内部エラー: 入力の定跡がありません"))?;
    let (partial, file) =
        create_partial(out).with_context(|| format!("出力を作成できません: {}", out.display()))?;
    let written = write_rows(graph, source, BufWriter::new(file)).and_then(|()| {
        std::fs::rename(&partial, out)
            .with_context(|| format!("出力を置き換えられません: {}", out.display()))
    });
    if written.is_err() {
        // 失敗の報告が本題なので、一時ファイルを消せなかったことは伝えない。
        std::fs::remove_file(&partial).ok();
    }
    written
}

/// `out` と同じディレクトリに、既存のファイル (入力を含む) と重ならない名前で一時ファイルを作る。
fn create_partial(out: &Path) -> Result<(PathBuf, File)> {
    let base = partial_path(out);
    let mut attempt = 0u32;
    loop {
        let mut path = base.clone().into_os_string();
        if attempt > 0 {
            path.push(format!(".{attempt}"));
        }
        let path = PathBuf::from(path);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt = attempt.checked_add(1).context("一時ファイルの名前が尽きました")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_rows(graph: &Graph, source: &Path, mut writer: BufWriter<File>) -> Result<()> {
    writer.write_all(BOOK_HEADER.as_bytes())?;
    writer.write_all(b"\n")?;
    let mut reader = LineReader::reopen(source, &graph.source_check)?;
    let mut text = String::new();
    let mut rows = Vec::new();
    let mut order = Vec::new();
    let mut added = graph.added.iter();
    for node in 0..graph.len() {
        writeln!(writer, "sfen {}", graph.sfen(node))?;
        text.clear();
        rows.clear();
        for_each_row(&mut reader, graph.blocks(node), graph.hashes[node], |line| {
            let fields = parse_row(line);
            rows.push(OutRow {
                move_usi: push_field(&mut text, fields.move_usi),
                ponder_usi: push_field(&mut text, fields.ponder_usi),
                depth: fields.depth,
                count: fields.count,
            });
            Ok(())
        })?;
        let range = graph.rows(node);
        for row in range.clone().skip(rows.len()) {
            if !graph.added(row) {
                bail!("{CHANGED}: {}", source.display());
            }
            let mv = added.next().ok_or_else(|| anyhow!("内部エラー: 補完した手がありません"))?;
            // 補完手には探索ラベルがない。depth=1 を出自の固定マーカーとして用い、
            // 未探索の 0/0 と区別する（伝播距離や探索した深さを表すものではない）。
            rows.push(OutRow {
                move_usi: push_field(&mut text, Some(&Move::from_u16(*mv).to_usi())),
                ponder_usi: push_field(&mut text, None),
                depth: 1,
                count: 0,
            });
        }
        if rows.len() != range.len() {
            bail!("{CHANGED}: {}", source.display());
        }
        order.clear();
        order.extend(0..rows.len());
        order.sort_by(|&a, &b| {
            rows[b]
                .count
                .cmp(&rows[a].count)
                .then_with(|| text[rows[a].move_usi.clone()].cmp(&text[rows[b].move_usi.clone()]))
        });
        for &idx in &order {
            let row = &rows[idx];
            writeln!(
                writer,
                "{} {} {} {} {}",
                &text[row.move_usi.clone()],
                &text[row.ponder_usi.clone()],
                graph.new[range.start + idx],
                row.depth,
                row.count
            )?;
        }
    }
    writer.into_inner().map_err(|e| e.into_error())?;
    reader.finish(source, &graph.source_check)?;
    Ok(())
}

fn push_field(text: &mut String, value: Option<&str>) -> Range<usize> {
    let start = text.len();
    text.push_str(value.unwrap_or("none"));
    start..text.len()
}

/// Markdown レポートを書き出す。
pub fn write_report(
    graph: &Graph,
    stats: &PropagationStats,
    path: &Path,
    merge: MergeMode,
    draws: DrawValues,
) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("report を作成できません: {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let total_moves = graph.total_rows();
    let total_edges = graph.to.iter().filter(|&&to| to != NONE).count();
    let direct_edges = total_edges - graph.flip_edges;

    writeln!(writer, "# book_backprop report")?;
    writeln!(writer)?;
    writeln!(writer, "## Summary")?;
    writeln!(writer)?;
    writeln!(writer, "- merge mode: {}", merge.as_str())?;
    writeln!(writer, "- draw value: black {} / white {}", draws.black, draws.white)?;
    writeln!(writer, "- nodes: {}", graph.len())?;
    writeln!(writer, "- moves: {total_moves}")?;
    writeln!(writer, "- updated moves: {}", stats.updated_moves)?;
    writeln!(writer, "- in-book edges: {total_edges}")?;
    writeln!(writer, "- direct edges: {direct_edges}")?;
    writeln!(writer, "- flip merged edges: {}", graph.flip_edges)?;
    writeln!(writer, "- illegal moves kept: {}", graph.illegal_moves)?;
    writeln!(writer)?;
    write_delta_report(&mut writer, &stats.abs_deltas)?;
    write_depth_report(&mut writer, &stats.depths)?;
    write_scc_report(&mut writer, stats)?;
    write_top_changes(&mut writer, &stats.changed_nodes)?;
    writer.flush()?;
    Ok(())
}

fn write_delta_report(writer: &mut dyn Write, deltas: &BTreeMap<i32, usize>) -> Result<()> {
    let total: usize = deltas.values().sum();
    writeln!(writer, "## Value deltas")?;
    writeln!(writer)?;
    if total == 0 {
        writeln!(writer, "- changed moves: 0")?;
        writeln!(writer)?;
        return Ok(());
    }
    writeln!(writer, "- changed moves: {total}")?;
    writeln!(writer, "- p50: {}", percentile(deltas, total, 50))?;
    writeln!(writer, "- p90: {}", percentile(deltas, total, 90))?;
    writeln!(writer, "- max: {}", deltas.keys().next_back().copied().unwrap_or(0))?;
    writeln!(writer)?;
    writeln!(writer, "| abs delta bucket | moves |")?;
    writeln!(writer, "|---|---:|")?;
    for (label, count) in delta_buckets(deltas) {
        writeln!(writer, "| {label} | {count} |")?;
    }
    writeln!(writer)?;
    Ok(())
}

/// 昇順に並べた `total` 個の値の `pct` パーセンタイル (並べた位置を切り上げる)。
fn percentile(counts: &BTreeMap<i32, usize>, total: usize, pct: usize) -> i32 {
    let idx = ((total - 1) * pct).div_ceil(100);
    let mut seen = 0;
    for (&value, &count) in counts {
        seen += count;
        if idx < seen {
            return value;
        }
    }
    0
}

fn delta_buckets(counts: &BTreeMap<i32, usize>) -> Vec<(&'static str, usize)> {
    let ranges = [
        ("1-49", 1, 49),
        ("50-99", 50, 99),
        ("100-199", 100, 199),
        ("200-499", 200, 499),
        ("500+", 500, i32::MAX),
    ];
    ranges
        .into_iter()
        .map(|(label, lo, hi)| (label, counts.range(lo..=hi).map(|(_, &count)| count).sum()))
        .collect()
}

fn write_depth_report(writer: &mut dyn Write, depths: &BTreeMap<usize, usize>) -> Result<()> {
    writeln!(writer, "## Propagation depth")?;
    writeln!(writer)?;
    writeln!(writer, "| depth | nodes |")?;
    writeln!(writer, "|---:|---:|")?;
    for (depth, count) in depths {
        writeln!(writer, "| {depth} | {count} |")?;
    }
    writeln!(writer)?;
    Ok(())
}

fn write_scc_report(writer: &mut dyn Write, stats: &PropagationStats) -> Result<()> {
    writeln!(writer, "## SCC")?;
    writeln!(writer)?;
    writeln!(writer, "- nontrivial SCCs: {}", stats.nontrivial_sccs)?;
    writeln!(writer, "- max SCC size: {}", stats.max_scc_size)?;
    writeln!(writer, "- draw-valued internal moves: {}", stats.draw_moves)?;
    writeln!(writer, "- check-loop core states: {}", stats.check_core_states)?;
    writeln!(
        writer,
        "- tie moves restricted to the guaranteed move: {}",
        stats.restricted_ties
    )?;
    writeln!(writer)?;
    Ok(())
}

fn write_top_changes(writer: &mut dyn Write, changes: &[NodeChange]) -> Result<()> {
    writeln!(writer, "## Top changed nodes")?;
    writeln!(writer)?;
    writeln!(writer, "| rank | old best | new best | sfen |")?;
    writeln!(writer, "|---:|---:|---:|---|")?;
    for (idx, change) in changes.iter().take(TOP_CHANGES).enumerate() {
        writeln!(
            writer,
            "| {} | {} | {} | `{}` |",
            idx + 1,
            change.old_best,
            change.new_best,
            change.sfen
        )?;
    }
    writeln!(writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_db::BookDb;

    #[test]
    fn review_quiet_cycle_with_check_branches_stays_draw() {
        let mut sfen = "5k3/9/6R2/9/9/9/6r2/9/5K3 b - 1".to_string();
        let mut input = format!("{BOOK_HEADER}\n");
        let mut quiet = Vec::new();
        for (mv, branch) in [
            ("3c5c", ["3c4c", "4a3a", "4c3c", "3a4a"]),
            ("3g5g", ["3g4g", "4i3i", "4g3g", "3i4i"]),
            ("5c3c", ["5c4c", "4a5a", "4c5c", "5a4a"]),
            ("5g3g", ["5g4g", "4i5i", "4g5g", "5i4i"]),
        ] {
            quiet.push((strip_ply(&sfen).to_string(), mv));
            input.push_str(&format!("sfen {sfen}\n{mv} none 0 10 1\n"));
            let mut check = sfen.clone();
            for checking_move in branch {
                input.push_str(&format!("sfen {check}\n{checking_move} none 0 10 1\n"));
                check = child_position_after_move(&check, checking_move).unwrap().to_sfen();
            }
            assert_eq!(strip_ply(&check), strip_ply(&sfen));
            sfen = child_position_after_move(&sfen, mv).unwrap().to_sfen();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.db");
        std::fs::write(&path, input).unwrap();
        let book = BookDb::read(&path).unwrap();
        let mut graph = read_graph(&path, BackpropOptions::default()).unwrap();
        assert_eq!(graph.illegal_moves, 0);
        propagate_values(&mut graph, DrawValues::default(), MergeMode::Replace).unwrap();
        for (key, mv) in quiet {
            let node = graph.find(&key).unwrap();
            let index = book.entries[&key]
                .moves
                .iter()
                .position(|m| m.move_usi.as_deref() == Some(mv))
                .unwrap();
            assert_eq!(values(&graph)[node][index], 0, "{mv}: {key}");
        }
    }

    #[test]
    fn branching_check_loop_is_solved_without_a_visit_limit() {
        // 王手をかける側 (3k の局面) が 2 通りの王手で合流しながら離脱のない循環を回す。
        let mut rows = vec![Vec::new(); 62];
        for level in 0..20 {
            let n = level * 3;
            rows[n] = vec![(0, Some(n + 1)), (0, Some(n + 2))];
            rows[n + 1] = vec![(0, Some(n + 3))];
            rows[n + 2] = vec![(0, Some(n + 3))];
        }
        rows[60] = vec![(0, Some(61))];
        rows[61] = vec![(0, Some(0))];
        let refs: Vec<_> = rows.iter().map(Vec::as_slice).collect();
        let checked: Vec<_> = (0..62).map(|n| n % 3 != 0).collect();
        let black: Vec<_> = (0..62).map(|n| n % 3 == 0).collect();
        let mut graph = rows_graph(&refs, &checked, &black);
        propagate_values(&mut graph, DrawValues::default(), MergeMode::Replace).unwrap();
        let mate = Value::MATE.raw();
        for (n, values) in values(&graph).into_iter().enumerate() {
            let expected = if n % 3 == 0 { -mate } else { mate };
            assert!(values.iter().all(|&value| value == expected), "{n}");
        }
    }

    #[test]
    fn review_tie_correction_stays_inside_mate_boundary() {
        let graph = solve_rows(&[&[(-32000, None), (0, Some(1))], &[(32000, None)]]).unwrap();
        assert_eq!(values(&graph)[0], [-32000, -32000]);
    }

    /// 行 `(値, 子)` の局面を並べたグラフ。局面 key は `<番号> <手番>` で、`black[i]` が先手番。
    fn rows_graph(rows: &[&[(i32, Option<usize>)]], checked: &[bool], black: &[bool]) -> Graph {
        let mut sfens = Texts::default();
        let mut node_flags = Vec::new();
        let mut row_start = vec![0];
        let (mut old, mut to) = (Vec::new(), Vec::new());
        for (i, rows) in rows.iter().enumerate() {
            sfens.push(&format!("{i} {}", if black[i] { "b" } else { "w" })).unwrap();
            let side = if black[i] { NODE_BLACK } else { 0 };
            node_flags.push(side | if checked[i] { NODE_CHECKED } else { 0 });
            for &(value, child) in rows.iter() {
                old.push(value);
                to.push(child.map_or(NONE, |child| child as u32));
            }
            row_start.push(old.len() as u32);
        }
        Graph {
            source: None,
            options: BackpropOptions::default(),
            sfens,
            node_flags,
            row_start,
            block_start: vec![0; rows.len() + 1],
            blocks: Vec::new(),
            hashes: Vec::new(),
            source_check: SourceCheck::default(),
            new: old.clone(),
            row_flags: vec![ROW_USABLE; old.len()],
            old,
            to,
            added: Vec::new(),
            flip_edges: 0,
            illegal_moves: 0,
        }
    }

    /// 偶数番が先手番、奇数番が後手番の局面を並べて逆伝播する。
    fn solve_rows_with(
        rows: &[&[(i32, Option<usize>)]],
        draws: DrawValues,
        merge: MergeMode,
    ) -> Result<Graph> {
        let black: Vec<_> = (0..rows.len()).map(|i| i % 2 == 0).collect();
        let mut graph = rows_graph(rows, &vec![false; rows.len()], &black);
        propagate_values(&mut graph, draws, merge)?;
        Ok(graph)
    }

    fn solve_rows(rows: &[&[(i32, Option<usize>)]]) -> Result<Graph> {
        solve_rows_with(rows, DrawValues::default(), MergeMode::Replace)
    }

    fn values(graph: &Graph) -> Vec<Vec<i32>> {
        (0..graph.len()).map(|v| graph.moves(v).map(|mv| mv.new).collect()).collect()
    }

    /// 書き出し値をそのまま与えたグラフに [`certify`] だけを適用する。偶数番が先手番。
    fn certify_rows(
        rows: &[&[(i32, Option<usize>)]],
        checked: &[bool],
        draws: DrawValues,
    ) -> Result<()> {
        let black: Vec<_> = (0..rows.len()).map(|i| i % 2 == 0).collect();
        let graph = rows_graph(rows, checked, &black);
        let violations = certify(
            &graph,
            BestParams {
                draws,
                merge: MergeMode::Replace,
                skip_unusable_moves: false,
            },
            &vec![None; rows.len()],
        );
        if violations.nodes.is_empty() {
            Ok(())
        } else {
            bail!("{}", violations.message(&graph))
        }
    }

    #[test]
    fn certify_rejects_positive_value_on_quiet_cycle() {
        let error = certify_rows(
            &[&[(50, Some(1))], &[(-50, Some(0))]],
            &[false; 2],
            DrawValues::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("千日手へ逃げられる 1 局面"), "{error}");
        certify_rows(&[&[(0, Some(1))], &[(0, Some(0))]], &[false; 2], DrawValues::default())
            .unwrap();
        // 相手が王手を続けるだけの循環なら、相手の反則負けなので勝ちを約束できる。
        let mate = Value::MATE.raw();
        certify_rows(
            &[&[(mate, Some(1))], &[(-mate, Some(0))]],
            &[true, false],
            DrawValues::default(),
        )
        .unwrap();
    }

    #[test]
    fn certify_rejects_own_perpetual_check_unless_already_mated() {
        let error = certify_rows(
            &[&[(-80, Some(1))], &[(80, Some(0))]],
            &[false, true],
            DrawValues::default(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("連続王手で負ける循環へ持ち込まれる 1 局面"),
            "{error}"
        );
        let mate = Value::MATE.raw();
        certify_rows(
            &[&[(-mate, Some(1))], &[(mate, Some(0))]],
            &[false, true],
            DrawValues::default(),
        )
        .unwrap();
    }

    #[test]
    fn certify_rejects_tied_move_into_unsafe_cycle() {
        // 同値の離脱があっても、補正されずに残った循環手を選べば千日手へ逃げられる。
        let rows: [&[_]; 2] = [&[(50, None), (50, Some(1))], &[(-50, Some(0))]];
        assert!(certify_rows(&rows, &[false; 2], DrawValues::default()).is_err());
        let rows: [&[_]; 2] = [&[(50, None), (49, Some(1))], &[(-50, Some(0))]];
        certify_rows(&rows, &[false; 2], DrawValues::default()).unwrap();
    }

    #[test]
    fn min_caps_only_own_moves_and_a_cycle_is_a_draw() {
        // 循環上の値 0 の局面で値の整合が切れていても、A の 50 は千日手にしかならない。
        // 先手 (A, C) の値は先手の手だけをラベルで抑え、後手 (B, D) の値は後手の手だけを抑えて求める。
        let rows: [&[_]; 4] = [
            &[(50, Some(1))],
            &[(-50, Some(2))],
            &[(0, Some(3))],
            &[(-50, Some(0))],
        ];
        let graph = solve_rows_with(&rows, DrawValues::default(), MergeMode::Min).unwrap();
        assert_eq!(values(&graph), [[0], [-50], [0], [-50]]);
        let graph = solve_rows(&rows).unwrap();
        assert_eq!(values(&graph), [[0], [0], [0], [0]]);
    }

    #[test]
    fn min_does_not_cap_opponent_moves_with_their_labels() {
        // 後手の 2 手目のラベル 20 は先手が book 外で咎める前提の値。先手は book どおりに指すので、
        // 先手から見た値では後手の手を抑えず、後手から見た値では後手自身の手を抑える。
        let rows: [&[_]; 3] = [
            &[(10, Some(1))],
            &[(10, None), (20, Some(2))],
            &[(-100, None)],
        ];
        let graph = solve_rows_with(&rows, DrawValues::default(), MergeMode::Min).unwrap();
        assert_eq!(values(&graph), [vec![-100], vec![10, 20], vec![-100]]);
    }

    #[test]
    fn draw_values_are_fixed_per_side() {
        // 先手が千日手を -70 とみると、後手は +30 で離脱せずに循環へ戻せば、先手に -50 の離脱を強いられる。
        let rows: [&[_]; 2] = [&[(-50, None), (0, Some(1))], &[(0, Some(0)), (30, None)]];
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        let graph = solve_rows_with(&rows, draws, MergeMode::Replace).unwrap();
        assert_eq!(values(&graph), [[-50, -51], [29, 30]]);
        // 両側とも 0 なら、後手が循環へ戻すと先手は千日手にできるので、後手は +30 で離脱する。
        let graph = solve_rows(&rows).unwrap();
        assert_eq!(values(&graph), [[-50, -30], [29, 30]]);
    }

    #[test]
    fn unsafe_tie_is_restricted_to_the_guaranteed_move() {
        // 同じ手数と見なした同値手が千日手の循環に入るなら、保証手 (葉) だけを残して循環手を -1 する。
        let mut graph = rows_graph(
            &[&[(50, None), (0, Some(1))], &[(0, Some(0))]],
            &[false; 2],
            &[true, false],
        );
        let results = [
            Some(NodeResult {
                value: 50,
                rows: vec![Some((50, Some(0))), Some((50, Some(0)))],
                guaranteed: Some(0),
                preferred: Some(0),
            }),
            Some(NodeResult {
                value: -50,
                rows: vec![Some((-50, Some(1)))],
                guaranteed: Some(0),
                preferred: Some(0),
            }),
        ];
        let params = BestParams {
            draws: DrawValues::default(),
            merge: MergeMode::Replace,
            skip_unusable_moves: false,
        };
        let results_of = |results: &[Option<NodeResult>; 2]| {
            let results = results.clone();
            move |_: &Graph, v: usize| results[v].clone()
        };
        let none = |_: &Graph, _: usize| None;
        assert_eq!(write_certified(&mut graph, params, &results_of(&results), &none).unwrap(), 1);
        assert_eq!(values(&graph), [vec![50, 49], vec![-50]]);
        // 保証手だけにしても破られる値はエラーにする。
        let mut broken = results.clone();
        broken[0].as_mut().unwrap().guaranteed = Some(1);
        broken[0].as_mut().unwrap().preferred = Some(1);
        assert!(write_certified(&mut graph, params, &results_of(&broken), &none).is_err());
    }

    #[test]
    fn flipped_orientation_that_needs_another_move_is_rejected() {
        // B と C 以降は反転キーで合流し、A から進むと C は後手番になる。先手は千日手を -70 とみて C で -50 の葉を選ぶが、
        // 千日手を 0 とみる後手には循環を続ける方が良いので、保存されている 1 行では両方の向きを表せない。
        let rows: [&[_]; 6] = [
            &[(0, Some(1))],
            &[(0, Some(2))],
            &[(-50, None), (0, Some(3))],
            &[(0, Some(4))],
            &[(0, Some(5))],
            &[(0, Some(2))],
        ];
        let black = [false, false, true, false, true, false];
        let mut graph = rows_graph(&rows, &[false; 6], &black);
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        let error = propagate_values(&mut graph, draws, MergeMode::Replace).unwrap_err();
        assert!(error.to_string().contains("反転キーで合流した"), "{error}");
        // 千日手の値が先後で同じなら、両方の向きは同じ問題なので通る。
        let mut graph = rows_graph(&rows, &[false; 6], &black);
        propagate_values(&mut graph, DrawValues::both(-70), MergeMode::Replace).unwrap();
    }

    #[test]
    fn flipped_orientation_prefers_a_move_that_keeps_both_values() {
        // X は先手番で保存され、A から反転キーで後手番としても届く。先手 (千日手 -70) には -70 の葉と循環が同値で、
        // 後手 (千日手 0) には循環の 0 が最善。両方の向きで値を保つ循環手を残し、葉を -1 する。
        let rows: [&[_]; 3] = [
            &[(0, Some(1))],
            &[(-70, None), (0, Some(2))],
            &[(0, Some(1))],
        ];
        let mut graph = rows_graph(&rows, &[false; 3], &[true, true, false]);
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        propagate_values(&mut graph, draws, MergeMode::Replace).unwrap();
        assert_eq!(values(&graph)[1], [-71, -70]);
    }

    #[test]
    fn draw_values_outside_the_engine_range_are_rejected() {
        for draw in [i32::MIN, -30_001, 30_001, i32::MAX] {
            let error =
                solve_rows_with(&[&[(0, None)]], DrawValues::both(draw), MergeMode::Replace)
                    .unwrap_err();
            assert!(error.to_string().contains("千日手の値は"), "{error}");
        }
        solve_rows_with(&[&[(0, None)]], DrawValues::both(-30_000), MergeMode::Replace).unwrap();
    }

    #[test]
    fn position_key_matches_sfen_keys_including_the_flipped_one() {
        for sfen in [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "4k4/9/4+R4/9/9/9/9/9/K8 w 2Pb3n 7",
            "ln5nl/6gk1/p3ppgp1/P5p1p/1NPp3P1/1P2SPP1P/2+p1P4/3KG2R1/L6NL b RB2SPbgsp 1",
        ] {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            let mut flipped = Position::new();
            flipped.set_sfen(&rshogi_book::flipped_key(sfen).unwrap()).unwrap();
            let [direct, flip] = PositionKey::pair(&pos);
            assert!(flip == PositionKey::pair(&flipped)[0], "{sfen}");
            assert!(direct != PositionKey::pair(&flipped)[0], "{sfen}");
            let mut same = Position::new();
            same.set_sfen(&format!("{} 99", strip_ply(sfen))).unwrap();
            assert!(direct == PositionKey::pair(&same)[0], "{sfen}");
        }
    }

    #[test]
    fn children_join_only_the_canonical_spelling_of_a_position() {
        let parent = "4k4/9/9/9/9/9/9/9/4K4 b GP 1";
        let child = child_position_after_move(parent, "5i5h").unwrap().to_sfen();
        let canonical = strip_ply(&child).to_string();
        let (board, rest) = canonical.split_once(' ').unwrap();
        let (side, hand) = rest.split_once(' ').unwrap();
        let reversed: String = hand.chars().rev().collect();
        assert_ne!(reversed, hand);
        let other = format!("{board} {side} {reversed}");
        let input = format!(
            "{BOOK_HEADER}\nsfen {parent}\n5i5h none 0 10 1\nsfen {canonical} 2\n5a4a none 30 10 1\nsfen {other} 2\n5a4a none 90 10 1\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.db");
        std::fs::write(&path, input).unwrap();
        let graph = read_graph(&path, BackpropOptions::default()).unwrap();
        let node = |key: &str| graph.find(key).unwrap();
        let edge = graph.moves(node(strip_ply(parent))).next().unwrap().edge.unwrap();
        assert_eq!(edge.to, node(&canonical));
    }

    #[test]
    fn mutual_continuous_check_is_rejected() {
        let black = [true, false];
        let mut graph = rows_graph(&[&[(0, Some(1))], &[(0, Some(0))]], &[true, true], &black);
        let error =
            propagate_values(&mut graph, DrawValues::default(), MergeMode::Replace).unwrap_err();
        assert!(error.to_string().contains("双方が王手をかけ続ける循環"), "{error}");
    }

    #[test]
    fn certify_compares_with_each_side_draw_value() {
        let rows: [&[_]; 2] = [&[(20, Some(1))], &[(-20, Some(0))]];
        let draws = |black, white| DrawValues { black, white };
        certify_rows(&rows, &[false; 2], draws(30, -10)).unwrap();
        // 先手は 20 を約束するが、千日手は先手に 10 しか与えない。
        assert!(certify_rows(&rows, &[false; 2], draws(10, -10)).is_err());
        // 後手は -20 を約束するが、千日手は後手に -10 を与えるので約束より良い。
        assert!(certify_rows(&rows, &[false; 2], draws(30, -30)).is_err());
    }

    #[test]
    fn losing_side_cannot_claim_draw_when_winner_can_exit() {
        let graph = solve_rows(&[&[(0, Some(1))], &[(0, Some(0)), (100, None)]]).unwrap();
        // 同値の遠回りを避ける。
        assert_eq!(values(&graph), [vec![-100], vec![99, 100]]);
    }

    #[test]
    fn distances_prefer_short_wins_and_long_losses_only_at_output() {
        for value in [-50, 0, 50] {
            let graph = solve_rows(&[&[(value, None), (0, Some(1))], &[(-value, None)]]).unwrap();
            let expected = if value >= 0 {
                [value, value - 1]
            } else {
                [value - 1, value]
            };
            assert_eq!(values(&graph)[0], expected);
        }
        let graph = solve_rows(&[&[(0, Some(1))], &[(0, Some(0))]]).unwrap();
        assert_eq!(values(&graph), [[0], [0]]);
    }

    #[test]
    fn perpetual_check_without_escape_is_loss_in_both_orientations() {
        let fixture = include_str!("../tests/fixtures/book_backprop_check_in.db");
        let pure: String = fixture
            .lines()
            .filter(|line| {
                !line.contains(" -200 ") && !line.contains(" -120 ") && !line.contains(" -80 ")
            })
            .map(|line| format!("{line}\n"))
            .collect();
        for flip in [false, true] {
            let input = if flip {
                pure.lines()
                    .map(|line| {
                        if let Some(sfen) = line.strip_prefix("sfen ") {
                            format!("sfen {}\n", rshogi_book::flipped_key(sfen).unwrap())
                        } else if line.starts_with('#') {
                            format!("{line}\n")
                        } else {
                            let (mv, rest) = line.split_once(' ').unwrap();
                            format!("{} {rest}\n", rshogi_book::flip_usi_move(mv).unwrap())
                        }
                    })
                    .collect()
            } else {
                pure.clone()
            };
            for merge in [MergeMode::Min, MergeMode::Replace] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("in.db");
                let out = dir.path().join("result.db");
                std::fs::write(&path, &input).unwrap();
                backprop_file(&path, &out, None, DrawValues::default(), merge).unwrap();
                for entry in BookDb::read(&out).unwrap().entries.values() {
                    let mut pos = Position::new();
                    pos.set_sfen(&entry.sfen).unwrap();
                    let rule_value = if pos.checkers().is_empty() {
                        -Value::MATE.raw()
                    } else {
                        Value::MATE.raw()
                    };
                    let expected = if merge == MergeMode::Min {
                        rule_value.min(0)
                    } else {
                        rule_value
                    };
                    assert_eq!(entry.moves[0].value, expected);
                }
            }
        }
    }

    #[test]
    fn unexplored_position_keeps_all_rows_even_with_searched_children() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f 3c3d 0 0 7\nsfen {child}\n3c3d none 50 12 3\n"
        );
        let output = backprop_text(
            &input,
            BackpropOptions {
                skip_unsearched_children: true,
                add_transposition_moves: true,
                ..BackpropOptions::default()
            },
        );
        assert!(output.contains("7g7f 3c3d 0 0 7\n"));
        assert!(!backprop_text(&input, BackpropOptions::default()).contains("7g7f 3c3d 0 0 7\n"));
    }

    #[test]
    fn tie_correction_keeps_unusable_and_skipped_child_rows_unchanged() {
        let searched = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let unsearched = child_position_after_move(START, "2g2f").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f none -50 9 7\n2g2f none -50 9 6\n5e5d none -50 9 5\nnone none -50 9 4\nsfen {searched}\n3c3d none 50 12 3\nsfen {unsearched}\n3c3d none 0 0 2\n"
        );
        for skip_unusable_moves in [false, true] {
            let output = backprop_text(
                &input,
                BackpropOptions {
                    skip_unsearched_children: true,
                    skip_unusable_moves,
                    ..BackpropOptions::default()
                },
            );
            for row in [
                "7g7f none -50 9 7",
                "2g2f none -50 9 6",
                "5e5d none -50 9 5",
                "none none -50 9 4",
            ] {
                assert!(output.contains(row), "{output}");
            }
        }
    }

    #[test]
    fn transposition_completion_is_opt_in_and_new_moves_have_no_min_label() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        for value in [-50, 50] {
            let input = format!(
                "{BOOK_HEADER}\nsfen {START}\n2g2f none -100 10 7\nsfen {child}\n3c3d none {value} 12 3\n"
            );
            assert!(!backprop_text(&input, BackpropOptions::default()).contains("7g7f none"));
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("in.db");
            let out = dir.path().join("out.db");
            std::fs::write(&path, &input).unwrap();
            let options = BackpropOptions {
                add_transposition_moves: true,
                ..BackpropOptions::default()
            };
            for merge in [MergeMode::Min, MergeMode::Replace] {
                backprop_file_with(&path, &out, None, DrawValues::default(), merge, options)
                    .unwrap();
                assert!(
                    std::fs::read_to_string(&out)
                        .unwrap()
                        .contains(&format!("7g7f none {} 1 0\n", -value))
                );
            }
        }
    }

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    fn check_child_propagation(child_rows: &str, flip: bool, unsearched: bool, best: i32) {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let child = if flip {
            rshogi_book::flipped_key(&child).unwrap()
        } else {
            child
        };
        let input =
            format!("{BOOK_HEADER}\nsfen {START}\n7g7f none 123 9 7\nsfen {child}\n{child_rows}");
        let dir = tempfile::tempdir().unwrap();
        let in_path = dir.path().join("in.db");
        let out_path = dir.path().join("out.db");
        std::fs::write(&in_path, &input).unwrap();
        for skip in [false, true] {
            let options = BackpropOptions {
                skip_unsearched_children: skip,
                ..BackpropOptions::default()
            };
            let graph = read_graph(&in_path, options).unwrap();
            let parent = graph.find(strip_ply(START)).unwrap();
            let excluded = skip && unsearched;
            let first = graph.moves(parent).next().unwrap();
            assert_eq!(first.edge.is_none(), excluded);
            assert_eq!(graph.flip_edges, usize::from(flip && !excluded));
            if let Some(edge) = first.edge {
                assert_eq!(edge.via_flip, flip);
            }
            for merge in [MergeMode::Replace, MergeMode::Min] {
                backprop_file_with(
                    &in_path,
                    &out_path,
                    None,
                    DrawValues::default(),
                    merge,
                    options,
                )
                .unwrap();
                let output = BookDb::read(&out_path).unwrap();
                let value = output.entries[strip_ply(START)].moves[0].value;
                let expected = if excluded {
                    123
                } else {
                    if merge == MergeMode::Min {
                        (-best).min(123)
                    } else {
                        -best
                    }
                };
                assert_eq!(value, expected, "skip={skip}, flip={flip}, merge={merge:?}");
            }
        }
    }

    #[test]
    fn skip_unsearched_children_preserves_parent_value() {
        check_child_propagation("3c3d none 0 0 3\n8c8d none 0 0 2\n", false, true, 0);
    }

    #[test]
    fn skip_unsearched_children_keeps_partly_searched_child() {
        check_child_propagation("3c3d none 50 10 3\n8c8d none 0 0 2\n", false, false, 50);
    }

    #[test]
    fn skip_unsearched_children_skips_flipped_child() {
        check_child_propagation("7g7f none 0 0 3\n2g2f none 0 0 2\n", true, true, 0);
    }

    #[test]
    fn skip_unsearched_children_keeps_empty_child() {
        check_child_propagation("", false, false, 0);
    }

    #[test]
    fn skip_unsearched_children_uses_depth_regardless_of_value() {
        check_child_propagation("3c3d none 50 0 3\n", false, true, 50);
    }

    fn backprop_text(input: &str, options: BackpropOptions) -> String {
        let dir = tempfile::tempdir().unwrap();
        let in_path = dir.path().join("in.db");
        let out_path = dir.path().join("out.db");
        std::fs::write(&in_path, input).unwrap();
        backprop_file_with(
            &in_path,
            &out_path,
            None,
            DrawValues::default(),
            MergeMode::Replace,
            options,
        )
        .unwrap();
        std::fs::read_to_string(out_path).unwrap()
    }

    #[test]
    fn skip_unusable_moves_excludes_illegal_and_none_rows_from_best() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        // 子局面の非合法手 5e5d と none 行は value 0。best に入ると 7g7f が 0 になる。
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f none 0 1 1\nsfen {child}\n3c3d none -50 10 3\n5e5d none 0 0 1\nnone none 0 0 1\n"
        );

        let default = backprop_text(&input, BackpropOptions::default());
        assert!(default.contains("7g7f none 0 1 1\n"));

        let skipped = backprop_text(
            &input,
            BackpropOptions {
                skip_unusable_moves: true,
                ..BackpropOptions::default()
            },
        );
        assert!(skipped.contains("7g7f none 50 1 1\n"));
        // 除外した行も値を変えずに書き出す。
        assert!(skipped.contains("5e5d none 0 0 1\n"));
        assert!(skipped.contains("none none 0 0 1\n"));
    }

    #[test]
    fn move_fields_are_written_back_as_in_the_input() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        // 7g7fjunk は 7g7f と読めて子局面へ伝播するが、書き出しは入力の欄のまま。
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7fjunk 3c3dX 0 1 9\nP*5e None 5 2 8\nlonger-than-eight resign 7 3 7\nNone none 1 4\nsfen {child}\n3c3d none 40 5 1\n"
        );
        let output = backprop_text(&input, BackpropOptions::default());
        for row in [
            "7g7fjunk 3c3dX -40 1 9\n",
            "P*5e none 5 2 8\n",
            "longer-than-eight none 7 3 7\n",
            "none none 1 4 1\n",
        ] {
            assert!(output.contains(row), "{output}");
        }
    }

    #[test]
    fn repeated_keys_are_joined_under_the_minimum_ply_line() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let input = format!(
            "2g2f none 99 9 9\n{BOOK_HEADER}\nsfen {} 5\n2g2f none 10 3 2\n// comment\n\nsfen {child}\n3c3d none 30 4 1\nsfen {START}\n7g7f none 0 6 2\n",
            strip_ply(START)
        );
        let output = backprop_text(&input, BackpropOptions::default());
        assert_eq!(
            output,
            format!(
                "{BOOK_HEADER}\nsfen {child}\n3c3d none 30 4 1\nsfen {START}\n2g2f none 10 3 2\n7g7f none -30 6 2\n"
            )
        );
    }

    #[test]
    fn output_can_replace_the_input_book() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f none 0 1 1\nsfen {child}\n3c3d none 30 4 1\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.db");
        std::fs::write(&path, &input).unwrap();
        backprop_file(&path, &path, None, DrawValues::default(), MergeMode::Replace).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            backprop_text(&input, BackpropOptions::default())
        );
    }

    #[test]
    fn canonical_moves_read_like_from_usi() {
        for sfen in [
            START,
            "ln5nl/6gk1/p3ppgp1/P5p1p/1NPp3P1/1P2SPP1P/2+p1P4/3KG2R1/L6NL b RB2SPbgsp 1",
        ] {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            let mut legal = MoveList::new();
            generate_legal_all(&pos, &mut legal);
            for &mv in legal.iter() {
                let usi = mv.to_usi();
                let fast = canonical_move(&usi).unwrap();
                assert_eq!(fast.raw(), Move::from_usi(&usi).unwrap().raw(), "{usi}");
                assert_eq!(fast.to_usi(), usi);
            }
        }
        for text in [
            "7g7f+x", "7g7fjunk", "P*5", "7G7f", "K*5e", "0a1b", "pass", "あa", "1a1a",
        ] {
            assert!(canonical_move(text).is_none(), "{text}");
        }
    }

    #[test]
    fn replaced_sfens_do_not_accumulate() {
        let mut texts = Texts::default();
        texts.push("k 1000000").unwrap();
        for ply in (1..1_000_000).rev() {
            texts.replace(0, &format!("k {ply}")).unwrap();
        }
        // 長い表記への差し替えも、使わない領域を詰め直して入力の大きさに収める。
        for zeros in 1..1000 {
            texts.replace(0, &format!("k {}1", "0".repeat(zeros))).unwrap();
        }
        assert_eq!(texts.get(0), format!("k {}1", "0".repeat(999)));
        assert!(texts.text.len() <= 2 * texts.get(0).len() + 16, "{}", texts.text.len());
    }

    #[test]
    fn temporary_output_does_not_overwrite_the_input() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f none 0 1 1\nsfen {child}\n3c3d none 30 4 1\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.db");
        let book = crate::common::io::partial_path(&out);
        std::fs::write(&book, &input).unwrap();
        backprop_file(&book, &out, None, DrawValues::default(), MergeMode::Replace).unwrap();
        assert_eq!(std::fs::read_to_string(&book).unwrap(), input);
        assert!(std::fs::read_to_string(&out).unwrap().contains("7g7f none -30 1 1\n"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn input_changed_before_writing_is_rejected() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f none 0 1 1\nsfen {child}\n3c3d none 30 4 1\n#fen {START}\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.db");
        let out = dir.path().join("out.db");
        // 同じ長さの指し手・ply の書き換え、ヘッダ行・コメント行の sfen 行への書き換えと、後ろへの局面の追加。
        let changed = [
            input.replace("7g7f none", "2g2f none"),
            input.replacen(" b - 1\n", " b - 3\n", 1),
            input.replacen("#YANE", "sfen ", 1),
            input.replace("#fen", "sfen"),
            format!("{input}sfen {START}\n2g2f none 0 1 1\n"),
        ];
        for changed in changed {
            assert_ne!(changed, input);
            std::fs::write(&path, &input).unwrap();
            let mut graph = read_graph(&path, BackpropOptions::default()).unwrap();
            propagate_values(&mut graph, DrawValues::default(), MergeMode::Replace).unwrap();
            std::fs::write(&path, changed).unwrap();
            let error = write_backprop_book(&graph, &out).unwrap_err();
            assert!(error.to_string().contains(CHANGED), "{error:#}");
            assert!(!out.exists());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn rows_appended_while_reading_are_rejected() {
        let input = format!("{BOOK_HEADER}\nsfen {START}\n7g7f none 0 1 1\n");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.db");
        std::fs::write(&path, &input).unwrap();
        let check = scan(&path).unwrap().check;
        let reader = LineReader::reopen(&path, &check).unwrap();
        std::fs::write(&path, format!("{input}sfen {START}\n2g2f none 100 1 1\n")).unwrap();
        let error = reader.finish(&path, &check).unwrap_err();
        assert!(error.to_string().contains(CHANGED), "{error:#}");
    }
}
