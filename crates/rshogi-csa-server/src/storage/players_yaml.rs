//! Ruby shogi-server 互換の `players.yaml` 形式でレートを永続化する
//! [`RateStorage`](crate::port::RateStorage) 実装。
//!
//! ## 互換するフォーマット
//!
//! Ruby `YAML.dump` で書き出される `players.yaml` 形式（[design.md] 1110 行）に
//! 合わせ、トップレベルはプレイヤ名（String）→ レコード（Ruby Symbol キー）の
//! マップで構成する。Ruby の `Symbol` は YAML 上で `":name"` のようにコロン接頭辞
//! つき文字列で表現されるため、ここでも `:name`/`:rate`/`:win`/`:loss`/
//! `:last_game_id`/`:last_modified` をキーとして読み書きする:
//!
//! ```yaml
//! alice:
//!   :name: alice
//!   :rate: 2500
//!   :win: 100
//!   :loss: 50
//!   :last_game_id: 20260426-001
//!   :last_modified: 2026-04-26T12:34:56+00:00
//! bob:
//!   :name: bob
//!   :rate: 2400
//!   :win: 80
//!   :loss: 60
//!   :last_modified: 2026-04-26T12:34:56+00:00
//! ```
//!
//! 書き出しは document start (`---`) を出さず、Symbol キー（`:name` など）と
//! `:last_modified` を quote 無しの plain scalar として出力する。例は
//! `render_document` のテスト golden YAML と一致させる（実出力と差異が出たら
//! `render_document_emits_byte_stable_yaml_with_ruby_symbol_keys` が落ちる）。
//!
//! ## 読み込みと書き出しの実装
//!
//! - **読み込み**は `granit-parser`（pure Rust の YAML パーサ）のイベント列を
//!   `YamlReader` が直接読む。serde 経由の汎用デシリアライザ（`serde-saphyr`）は、
//!   quote 無しのキーを数値として解釈して重複を判定するので、`1_000` と `10_00` の
//!   ような別のプレイヤ名を同じキーとみなしてしまう。
//! - **書き出し**は `render_document` が自前で組み立てる。汎用のシリアライザは
//!   `:` 始まりの文字列とタイムスタンプ形の文字列を quote するため、上記の
//!   「quote 無し」の形式を保てない。
//!
//! 出力は、以前使っていた `serde_yaml` 0.9 の出力とバイト単位で一致させてある
//! （どの文字列を quote するかの規則は `push_scalar` を参照）。例外は次の 2 つ:
//!
//! - 改行文字（LF / CR / U+0085 / U+2028 / U+2029）を含む文字列は、block scalar や
//!   explicit key ではなく double quote + エスケープで書く。
//! - プレイヤ名と `:last_game_id` は、Ruby の YAML ローダーが文字列以外の型に読む形
//!   （`yes`、`1_000`、`:foo`、`2026-04-26` など）なら quote する。`serde_yaml` は
//!   YAML 1.2 の規則でしか quote しないので、こうした名前は Ruby 側で真偽値や数値に
//!   化けていた（`push_text` を参照）。`<<` という名前のキーには `!!str` タグを付ける。
//!
//! 読み込みは `serde_yaml` が書いたファイルをそのまま読める。ただし U+2028 / U+2029
//! を含む文字列だけは別で、`serde_yaml` はこれを改行として生のまま書いていたが、
//! YAML 1.2 のパーサは改行として扱わないため、元の文字列に戻らない。
//!
//! ## クリーンルーム方針
//!
//! Ruby shogi-server / mk_rate / mk_html のソースは参照せず、上記の公開ドキュメント
//! にある形式情報のみから実装する（OSS 互換ガイドラインに準拠）。CI も外部 Ruby
//! ランタイムや shogi-server リポジトリを引かない。
//!
//! ## レート値の責務分担
//!
//! `:rate` フィールドは Ruby `mk_rate` バッチが Glicko 系のアルゴリズムで計算する
//! 領域なので、本サーバ側では `record_game_outcome` で **触れない**。サーバが
//! 更新するのは `:win` / `:loss` / `:last_game_id` / `:last_modified` の 4 つだけで、
//! ロード時に取得した `:rate` をそのまま `save` 側に書き戻す。これにより `mk_rate`
//! と本サーバを同居させる運用でも、レート値を踏まないで wins/losses を加算できる。
//!
//! ## アトミック性
//!
//! - **ファイル書き込み**: tmpfile 書き込み + `rename(2)` の POSIX atomic で
//!   `players.yaml` の半端な状態を生まない。
//! - **read-modify-write**: 複数対局が同時に同一プレイヤのレコードを書き換える
//!   ケースを `disk_lock` (async Mutex) 配下で直列化し、`record_game_outcome` の
//!   内部で「キャッシュ更新 → 全件 snapshot → atomic write」を 1 critical section
//!   で完結する。`load` + `save` の 2 段呼び出しではなく `record_game_outcome` を
//!   経由する限り、wins/losses の lost-update は発生しない。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

use granit_parser::{Event, Parser, ScalarStyle, StrInput};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::StorageError;
use crate::port::{PlayerRateRecord, RateStorage};
use crate::types::{GameId, PlayerName};

/// `StdMutex<HashMap<...>>` を poisoning に強くロックする小ヘルパ。
///
/// `current_thread + LocalSet` 運用では他スレッドが panic を伝播させる経路は
/// 実質存在しないが、`poison`化は契約違反ではなく単に「ロック中にどこかで
/// panic が起きた」副作用に過ぎない。CLAUDE.md の「panic は契約違反のみ」
/// 方針に合わせ、poisoning は `into_inner` でデータをそのまま借りる。
fn lock_cache<T>(m: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Ruby shogi-server 互換 `players.yaml` をレートストレージとして使う実装。
///
/// 起動時に `load_from_file` でファイル全体を in-memory `HashMap` に取り込み、
/// `load` は cache lookup のみで応答する（disk read を発生させない）。
/// `save` / `record_game_outcome` は cache を更新したあと、全件 snapshot を
/// atomic write で `players.yaml` に書き戻す。
///
/// ファイルが存在しない場合は空のマップから始め、最初の `save` で生成する。
#[derive(Debug)]
pub struct PlayersYamlRateStorage {
    path: PathBuf,
    cache: StdMutex<HashMap<String, PlayerRateRecord>>,
    /// disk write を直列化する async lock。`save` / `record_game_outcome` の
    /// critical section 全体（cache 更新 → snapshot → atomic write）を覆う。
    disk_lock: AsyncMutex<()>,
}

impl PlayersYamlRateStorage {
    /// 既存の `players.yaml` を読み込んでレートストレージを構築する。
    ///
    /// ファイルが存在しない場合は空マップを返す（初回起動の運用シナリオ）。
    /// ファイルが空文字列・空白のみの場合も同様に空マップとして扱う。
    /// パース失敗は [`StorageError::Malformed`] として `Err` を返す。
    ///
    /// **fail-fast**: ファイル自体が `NotFound` でも、親ディレクトリが存在しない
    /// 場合は設定ミス（例: `--players-yaml /nonexistent-dir/players.yaml`）として
    /// 起動時に `StorageError::Io` を返す。これを許容すると、サーバが accept ループ
    /// に入ったあと初回終局時に毎回書き込みが失敗する形で運用障害が遅延顕在化
    /// するため、最初の `record_game_outcome` を待たずに起動段階で検知する。
    pub async fn load_from_file(path: PathBuf) -> Result<Self, StorageError> {
        let map = match fs::read_to_string(&path).await {
            Ok(text) if text.trim().is_empty() => HashMap::new(),
            Ok(text) => parse_document(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ensure_parent_dir_exists(&path).await?;
                HashMap::new()
            }
            Err(e) => {
                return Err(StorageError::Io(format!("read {}: {}", path.display(), e)));
            }
        };
        Ok(Self {
            path,
            cache: StdMutex::new(map),
            disk_lock: AsyncMutex::new(()),
        })
    }

    /// `players.toml` 由来の `PlayerRateRecord` で、まだ YAML 上に未登録のプレイヤ
    /// レコードを補填する。disk への書き戻しは行わず、cache を更新するのみ。
    ///
    /// **契約**: YAML 既存レコードは保護される（同名キーが既にあれば渡された
    /// `PlayerRateRecord` は捨てる）。これにより YAML 側で運用中の `:rate` /
    /// `:win` / `:loss` を上書きせず、TOML 側の値は「YAML 移行時の既存プレイヤ
    /// 補填」目的にのみ使われる。
    ///
    /// `players.toml` で定義された全プレイヤを LOGIN 経路で受け付けるための
    /// 起動時補填用。最初に終局して `record_game_outcome` が走った時点で
    /// `players.yaml` に書き戻される。
    pub fn ensure_default_records<I>(&self, defaults: I)
    where
        I: IntoIterator<Item = PlayerRateRecord>,
    {
        let mut cache = lock_cache(&self.cache);
        for rec in defaults {
            cache.entry(rec.name.as_str().to_owned()).or_insert(rec);
        }
    }

    fn snapshot(&self) -> HashMap<String, PlayerRateRecord> {
        lock_cache(&self.cache).clone()
    }

    async fn flush_to_disk(
        &self,
        snapshot: &HashMap<String, PlayerRateRecord>,
    ) -> Result<(), StorageError> {
        atomic_write_yaml(&self.path, &render_document(snapshot)).await
    }
}

impl RateStorage for PlayersYamlRateStorage {
    fn load(
        &self,
        name: &PlayerName,
    ) -> impl std::future::Future<Output = Result<Option<PlayerRateRecord>, StorageError>> {
        let result = lock_cache(&self.cache).get(name.as_str()).cloned();
        async move { Ok(result) }
    }

    fn save(
        &self,
        record: &PlayerRateRecord,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> {
        let key = record.name.as_str().to_owned();
        let value = record.clone();
        async move {
            let _guard = self.disk_lock.lock().await;
            {
                let mut cache = lock_cache(&self.cache);
                cache.insert(key, value);
            }
            let snapshot = self.snapshot();
            self.flush_to_disk(&snapshot).await
        }
    }

    fn list_all(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<PlayerRateRecord>, StorageError>> {
        let snapshot: Vec<PlayerRateRecord> = lock_cache(&self.cache).values().cloned().collect();
        async move { Ok(snapshot) }
    }

    fn record_game_outcome(
        &self,
        black: &PlayerName,
        white: &PlayerName,
        winner: Option<&PlayerName>,
        game_id: &GameId,
        now_iso: &str,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> {
        debug_assert_ne!(
            black, white,
            "record_game_outcome: black and white must be distinct players",
        );
        let black_str = black.as_str().to_owned();
        let white_str = white.as_str().to_owned();
        // self-play 防護: 同名を渡した場合は二重加算を防ぐため `for` ループに
        // 入る前に同一性チェックする。trait 既定実装と同じ契約を保つ。
        let same_player = black_str == white_str;
        let winner_str = winner.map(|w| w.as_str().to_owned());
        let game_id_owned = game_id.clone();
        let now_owned = now_iso.to_owned();
        async move {
            if same_player {
                return Ok(());
            }
            let _guard = self.disk_lock.lock().await;
            {
                let mut cache = lock_cache(&self.cache);
                for key in [&black_str, &white_str] {
                    if let Some(rec) = cache.get_mut(key) {
                        match winner_str.as_deref() {
                            Some(w) if w == key => rec.wins = rec.wins.saturating_add(1),
                            Some(_) => rec.losses = rec.losses.saturating_add(1),
                            None => {}
                        }
                        rec.last_game_id = Some(game_id_owned.clone());
                        rec.last_modified = now_owned.clone();
                    }
                }
            }
            let snapshot = self.snapshot();
            self.flush_to_disk(&snapshot).await
        }
    }
}

fn parse_document(text: &str) -> Result<HashMap<String, PlayerRateRecord>, StorageError> {
    YamlReader::new(text)
        .read_players()
        .map_err(|e| StorageError::Malformed(format!("players.yaml: {e}")))
}

/// 読み込んだ scalar 1 つ。
#[derive(Clone)]
struct Scalar {
    text: String,
    /// quote もタグも無い `~` / `null` / 空（YAML の null）か。
    is_null: bool,
}

/// YAML の node の先頭。mapping / sequence は中身をまだ読んでいない状態を表す。
enum Node {
    Scalar(Scalar),
    MappingStart,
    SequenceStart,
}

/// `players.yaml` を YAML パーサのイベント列から直接読む。
///
/// serde 経由の汎用デシリアライザを使わないのは、プレイヤ名を **書かれた文字列の
/// まま** 区別するため。汎用デシリアライザは quote 無しのキーを数値として解釈して
/// 重複を判定するので、`1_000` と `10_00` のような別の名前を同じキーとみなし、片方の
/// レコードを捨ててしまう（`serde_yaml` はこうした名前を quote 無しで書いていた）。
///
/// ここでは scalar をすべて文字列として受け取り、型は項目ごとに決める:
///
/// - 同じプレイヤ名・同じ項目が重複していたら後のものを採る。
/// - `<<` も通常のキーとして読む（merge key として展開しない）。
/// - anchor / alias（`&1` / `*1`）は scalar に付いたものだけ解決する。Ruby の
///   `YAML.dump` は同じ文字列オブジェクトを 2 回書くときにこれを使う。
/// - 知らない項目は値ごと読み飛ばす。
struct YamlReader<'a> {
    parser: Parser<'a, StrInput<'a>>,
    anchors: HashMap<usize, Scalar>,
    /// 直近に読んだイベントの行番号（エラーメッセージ用）。
    line: usize,
}

impl<'a> YamlReader<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            parser: Parser::new_from_str(text),
            anchors: HashMap::new(),
            line: 0,
        }
    }

    fn error(&self, message: impl std::fmt::Display) -> String {
        format!("{message} at line {}", self.line)
    }

    fn next_event(&mut self) -> Result<Event<'a>, String> {
        loop {
            match self.parser.next() {
                Some(Ok((Event::Comment(..), _))) => {}
                Some(Ok((event, span))) => {
                    self.line = span.start.line();
                    return Ok(event);
                }
                Some(Err(e)) => return Err(e.to_string()),
                None => return Err(self.error("unexpected end of input")),
            }
        }
    }

    /// 次の node を読む。mapping / sequence の終わりに達していたら `None`。
    fn next_node(&mut self) -> Result<Option<Node>, String> {
        match self.next_event()? {
            Event::Scalar(text, style, anchor, tag) => {
                let is_null = style == ScalarStyle::Plain
                    && tag.is_none()
                    && matches!(&*text, "" | "~" | "null" | "Null" | "NULL");
                let scalar = Scalar {
                    text: text.into_owned(),
                    is_null,
                };
                if anchor != 0 {
                    self.anchors.insert(anchor, scalar.clone());
                }
                Ok(Some(Node::Scalar(scalar)))
            }
            Event::Alias(anchor) => match self.anchors.get(&anchor) {
                Some(scalar) => Ok(Some(Node::Scalar(scalar.clone()))),
                None => Err(self.error("alias to a mapping or sequence is not supported")),
            },
            Event::MappingStart(..) => Ok(Some(Node::MappingStart)),
            Event::SequenceStart(..) => Ok(Some(Node::SequenceStart)),
            Event::MappingEnd | Event::SequenceEnd => Ok(None),
            other => Err(self.error(format!("unexpected YAML event {other:?}"))),
        }
    }

    /// `node` が mapping / sequence の先頭なら、対応する終わりまで読み飛ばす。
    fn skip(&mut self, node: Node) -> Result<(), String> {
        let mut depth = usize::from(!matches!(node, Node::Scalar(_)));
        while depth > 0 {
            match self.next_event()? {
                Event::MappingStart(..) | Event::SequenceStart(..) => depth += 1,
                Event::MappingEnd | Event::SequenceEnd => depth -= 1,
                _ => {}
            }
        }
        Ok(())
    }

    fn scalar(&self, node: Node, field: &str) -> Result<Scalar, String> {
        match node {
            Node::Scalar(scalar) => Ok(scalar),
            Node::MappingStart | Node::SequenceStart => {
                Err(self.error(format!("`{field}` must be a scalar")))
            }
        }
    }

    fn number<T: std::str::FromStr>(&self, node: Node, field: &str) -> Result<T, String> {
        let scalar = self.scalar(node, field)?;
        scalar
            .text
            .parse()
            .map_err(|_| self.error(format!("invalid value {:?} for `{field}`", scalar.text)))
    }

    fn read_players(mut self) -> Result<HashMap<String, PlayerRateRecord>, String> {
        let mut players = HashMap::new();
        if !matches!(self.next_event()?, Event::StreamStart) {
            return Err(self.error("expected the start of a YAML stream"));
        }
        match self.next_event()? {
            // 空のファイル・コメントだけのファイル。
            Event::StreamEnd => return Ok(players),
            Event::DocumentStart(..) => {}
            other => return Err(self.error(format!("unexpected YAML event {other:?}"))),
        }
        match self.next_node()? {
            // `---` だけの文書。
            Some(Node::Scalar(scalar)) if scalar.is_null => {}
            Some(Node::MappingStart) => {
                while let Some(key) = self.next_node()? {
                    let name = self.scalar(key, "player name")?.text;
                    let Some(Node::MappingStart) = self.next_node()? else {
                        return Err(
                            self.error(format!("record of player {name:?} must be a mapping"))
                        );
                    };
                    let record = self.read_record(&name)?;
                    players.insert(name, record);
                }
            }
            _ => return Err(self.error("expected a mapping of players at the top level")),
        }
        match (self.next_event()?, self.next_event()?) {
            (Event::DocumentEnd, Event::StreamEnd) => Ok(players),
            _ => Err(self.error("expected a single YAML document")),
        }
    }

    /// 1 プレイヤ分のレコード（`:name` / `:rate` / ... の mapping）を、先頭を読んだ
    /// 直後の状態から読む。`:last_game_id` は無くてもよく、YAML の null なら `None`。
    fn read_record(&mut self, player: &str) -> Result<PlayerRateRecord, String> {
        let (mut name, mut rate, mut wins, mut losses) = (None, None, None, None);
        let (mut last_game_id, mut last_modified) = (None, None);
        while let Some(key) = self.next_node()? {
            let key = self.scalar(key, "field name")?.text;
            let Some(value) = self.next_node()? else {
                return Err(self.error(format!("`{key}` has no value")));
            };
            match key.as_str() {
                ":name" => name = Some(self.scalar(value, ":name")?.text),
                ":rate" => rate = Some(self.number(value, ":rate")?),
                ":win" => wins = Some(self.number(value, ":win")?),
                ":loss" => losses = Some(self.number(value, ":loss")?),
                ":last_game_id" => {
                    let scalar = self.scalar(value, ":last_game_id")?;
                    last_game_id = (!scalar.is_null).then_some(scalar.text);
                }
                ":last_modified" => {
                    last_modified = Some(self.scalar(value, ":last_modified")?.text);
                }
                _ => self.skip(value)?,
            }
        }
        let missing =
            |field: &str| self.error(format!("player {player:?}: missing field `{field}`"));
        Ok(PlayerRateRecord {
            name: PlayerName::new(name.ok_or_else(|| missing(":name"))?),
            rate: rate.ok_or_else(|| missing(":rate"))?,
            wins: wins.ok_or_else(|| missing(":win"))?,
            losses: losses.ok_or_else(|| missing(":loss"))?,
            last_game_id: last_game_id.map(GameId::new),
            last_modified: last_modified.ok_or_else(|| missing(":last_modified"))?,
        })
    }
}

/// `key:` の形（implicit key）で書くキーの最大 byte 数。
///
/// これを超えるキーは `? key` の形（explicit key）で書く。YAML の implicit key には
/// 1024 文字までという制限があり、長いキーをそのまま `key:` と書くと読み戻せない。
/// 境界の 128 は `serde_yaml`（libyaml）の出力に合わせた値。
const MAX_IMPLICIT_KEY_BYTES: usize = 128;

/// 全レコードを `players.yaml` の文字列に組み立てる。
///
/// `last_game_id` が `None` のレコードは `:last_game_id` 行ごと出力しない（`null`
/// リテラルや空値を吐かない）。Ruby `YAML.dump` で `Hash.delete(:last_game_id)` 済みの
/// 未対局レコードと等価な見え方になる。
fn render_document(records: &HashMap<String, PlayerRateRecord>) -> String {
    if records.is_empty() {
        return "{}\n".to_owned();
    }
    // `BTreeMap` でキーをソートして書き出すことで、同一データから出力 byte 列が
    // 一致する（運用での diff 比較・自動レビューが安定する）。
    let sorted: BTreeMap<&str, &PlayerRateRecord> =
        records.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let mut out = String::new();
    for (key, record) in sorted {
        if key == "<<" {
            // Ruby は `<<` というキーを、quote してあっても merge key として扱い、値の
            // mapping を親に展開する。文字列だと明示するタグを付けたときだけ通常のキーに
            // なる。
            out.push_str("!!str <<:\n  :name: ");
        } else if key.len() <= MAX_IMPLICIT_KEY_BYTES {
            push_text(&mut out, key);
            out.push_str(":\n  :name: ");
        } else {
            out.push_str("? ");
            push_text(&mut out, key);
            out.push_str("\n: :name: ");
        }
        push_text(&mut out, record.name.as_str());
        out.push_str(&format!(
            "\n  :rate: {}\n  :win: {}\n  :loss: {}\n",
            record.rate, record.wins, record.losses
        ));
        if let Some(game_id) = &record.last_game_id {
            out.push_str("  :last_game_id: ");
            push_text(&mut out, game_id.as_str());
            out.push('\n');
        }
        // `:last_modified` は時刻として読まれてよい（Ruby 側では Time になる）ので、
        // `push_text` ではなく `push_scalar` で quote 無しのまま書く。
        out.push_str("  :last_modified: ");
        push_scalar(&mut out, &record.last_modified);
        out.push('\n');
    }
    out
}

/// プレイヤ名や game id のように、読み手に必ず文字列として読まれるべき値を書く。
///
/// [`push_scalar`] の規則に加えて、Ruby の YAML ローダーが文字列以外の型に読む形
/// （[`ruby_may_read_as_non_string`]）も single quote する。quote してあれば Ruby は
/// 常に String として読む。
fn push_text(out: &mut String, s: &str) {
    if is_plain(s) && ruby_may_read_as_non_string(s) {
        push_single_quoted(out, s);
    } else {
        push_scalar(out, s);
    }
}

/// 文字列 1 つを YAML の scalar として `out` に書く。
///
/// quote の要否は `serde_yaml` 0.9（libyaml）の出力と同じ規則で決める:
///
/// 1. エスケープが要る文字（制御文字・改行・BOM など）を含むなら double quote。
/// 2. 空文字列、YAML 1.2 で null / 真偽値 / 数値として読まれる文字列、plain で書くと
///    構文上の意味が変わる文字列は single quote。
/// 3. それ以外は quote 無し（plain）。`:name` のような Symbol 形や
///    `2026-04-26T12:34:56+00:00` のような時刻はここに入る。
fn push_scalar(out: &mut String, s: &str) {
    if s.chars().any(needs_escape) {
        out.push('"');
        for c in s.chars() {
            push_double_quoted_char(out, c);
        }
        out.push('"');
    } else if is_plain(s) {
        out.push_str(s);
    } else {
        push_single_quoted(out, s);
    }
}

/// [`push_scalar`] が quote 無しで書く文字列か。
fn is_plain(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(needs_escape) && !resolves_to_non_string(s) && plain_allowed(s)
}

fn push_single_quoted(out: &mut String, s: &str) {
    out.push('\'');
    out.push_str(&s.replace('\'', "''"));
    out.push('\'');
}

/// quote 無しで書くと、Ruby の YAML ローダー（Psych）が文字列以外の型に読みうる形か。
///
/// Ruby は YAML 1.1 寄りの規則で型を推測するので、YAML 1.2 では文字列でしかない形も
/// 別の型になる。Psych の規則そのものではなく、それを含む広めの判定にしてある
/// （Psych の版による差を吸収するため。余分に quote しても、読まれる値は変わらない）:
///
/// - 真偽値・nil: `yes` / `no` / `on` / `off` / `true` / `false` / `null` など
///   （大文字小文字を問わない）
/// - Symbol: `:` 始まり
/// - 日付・時刻: `2026-04-26` で始まる形
/// - 数値: `1_000` / `1,000`（区切りつき）、`1:30`（60 進）、`.5`、値域を超える 16 進
///   表記など。数字か `.`（または符号）で始まり、数字を含み、数値に現れる文字だけで
///   できているもの
fn ruby_may_read_as_non_string(s: &str) -> bool {
    const KEYWORDS: [&str; 9] = ["y", "n", "yes", "no", "on", "off", "true", "false", "null"];
    if KEYWORDS.iter().any(|k| s.eq_ignore_ascii_case(k)) || s.starts_with(':') {
        return true;
    }
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if unsigned.eq_ignore_ascii_case(".inf") || unsigned.eq_ignore_ascii_case(".nan") {
        return true;
    }
    starts_with_date(unsigned) || {
        let mut prev = None;
        // 符号つきなら区切りで始まる形（`+,5` / `-_29`）も対象にする。古い Psych
        // （Ruby 2.3 で確認）はこれを整数として読む。
        let signed = unsigned.len() < s.len();
        (signed || unsigned.starts_with(|c: char| c.is_ascii_digit() || c == '.'))
            && unsigned.contains(|c: char| c.is_ascii_digit())
            && unsigned.chars().all(|c| {
                // 符号は指数の直後（`1e+5`）にだけ現れる。`20260426-001` のような
                // game id はここで数値から外れる。
                let ok = c.is_ascii_hexdigit()
                    || matches!(c, '_' | ',' | '.' | ':' | 'x')
                    || (matches!(c, '+' | '-') && matches!(prev, Some('e' | 'E')));
                prev = Some(c);
                ok
            })
    }
}

/// `YYYY-M-D`（月・日は 1〜2 桁）で始まるか。
fn starts_with_date(s: &str) -> bool {
    let mut parts = s.splitn(3, '-');
    let (Some(year), Some(month), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let all_digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
    year.len() == 4
        && all_digits(year)
        && matches!(month.len(), 1 | 2)
        && all_digits(month)
        && rest.starts_with(|c: char| c.is_ascii_digit())
}

/// double quote の中でエスケープして書く必要がある文字か。
///
/// そのまま書けるのは ASCII の印字可能文字と、U+00A0 以上の文字のうち改行扱いの
/// U+2028 / U+2029、BOM (U+FEFF)、非文字 U+FFFE / U+FFFF を除いたもの。
fn needs_escape(c: char) -> bool {
    !matches!(
        c,
        ' '..='~'
            | '\u{a0}'..='\u{2027}'
            | '\u{202a}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fefe}'
            | '\u{ff00}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}'
    )
}

fn push_double_quoted_char(out: &mut String, c: char) {
    match c {
        '\0' => out.push_str("\\0"),
        '\u{7}' => out.push_str("\\a"),
        '\u{8}' => out.push_str("\\b"),
        '\t' => out.push_str("\\t"),
        '\n' => out.push_str("\\n"),
        '\u{b}' => out.push_str("\\v"),
        '\u{c}' => out.push_str("\\f"),
        '\r' => out.push_str("\\r"),
        '\u{1b}' => out.push_str("\\e"),
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\u{85}' => out.push_str("\\N"),
        '\u{2028}' => out.push_str("\\L"),
        '\u{2029}' => out.push_str("\\P"),
        c if !needs_escape(c) => out.push(c),
        c if c <= '\u{ff}' => out.push_str(&format!("\\x{:02X}", u32::from(c))),
        c => out.push_str(&format!("\\u{:04X}", u32::from(c))),
    }
}

/// quote 無しで書くと、YAML 1.2 の読み手に文字列以外（null / 真偽値 / 数値）として
/// 読まれる文字列か。`001` のような「0 始まりの数字列」も quote する。
fn resolves_to_non_string(s: &str) -> bool {
    if matches!(
        s,
        "null" | "Null" | "NULL" | "~" | "true" | "True" | "TRUE" | "false" | "False" | "FALSE"
    ) {
        return true;
    }
    let (negative, unsigned) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if unsigned.starts_with(['+', '-']) {
        return false;
    }
    if unsigned.len() > 1
        && unsigned.starts_with('0')
        && unsigned[1..].bytes().all(|b| b.is_ascii_digit())
    {
        return true;
    }
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = unsigned.strip_prefix(prefix) {
            if digits.starts_with(['+', '-']) {
                return false;
            }
            let is_int = if negative {
                i128::from_str_radix(&format!("-{digits}"), radix).is_ok()
            } else {
                u128::from_str_radix(digits, radix).is_ok()
            };
            if is_int {
                return true;
            }
        }
    }
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") || matches!(s, ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    let float_text = if negative { s } else { unsigned };
    float_text.parse::<f64>().is_ok_and(f64::is_finite)
}

/// quote 無し（plain）で書いても、YAML の構文上ほかの意味に取られない文字列か。
///
/// 呼び出し側で空文字列と `needs_escape` な文字（タブ・改行を含む）は除外済みなので、
/// 空白は ASCII スペースだけを考えればよい。
fn plain_allowed(s: &str) -> bool {
    if s.starts_with(' ') || s.ends_with(' ') || s.starts_with("---") || s.starts_with("...") {
        return false;
    }
    let mut prev = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let followed_by_blank = matches!(chars.peek(), None | Some(' '));
        let is_indicator = match prev {
            // 先頭の文字: 単独で indicator になる記号と、直後が空白（または末尾）の
            // ときだけ indicator になる `?` `:` `-`。
            None => {
                "#,[]{}&*!|>'\"%@`".contains(c)
                    || (matches!(c, '?' | ':' | '-') && followed_by_blank)
            }
            // 2 文字目以降: `: `（キーと値の区切り）と ` #`（コメント開始）。
            Some(p) => (c == ':' && followed_by_blank) || (c == '#' && p == ' '),
        };
        if is_indicator {
            return false;
        }
        prev = Some(c);
    }
    true
}

/// `path` の親ディレクトリが存在することを検証する（[`load_from_file`] の
/// fail-fast 用）。`path` が単純なファイル名（親が空 / `Path::new(".")` 相当）
/// の場合はカレントディレクトリが暗黙的な親なので存在チェックは省略する。
///
/// `tokio::fs::try_exists` は対象が存在しないときに `Ok(false)`、stat 自体が
/// permission denied 等で失敗した場合に `Err` を返す。後者は親が存在しない
/// 設定ミスとは別軸の運用障害（権限不足）なので、エラーメッセージにパスと
/// 原因の両方を残して `Io` で上に伝える。
async fn ensure_parent_dir_exists(path: &Path) -> Result<(), StorageError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return Ok(()),
    };
    let exists = fs::try_exists(parent).await.map_err(|e| {
        StorageError::Io(format!(
            "stat parent dir {} for {}: {}",
            parent.display(),
            path.display(),
            e
        ))
    })?;
    if !exists {
        return Err(StorageError::Io(format!(
            "players.yaml parent directory does not exist: {} (for {})",
            parent.display(),
            path.display()
        )));
    }
    Ok(())
}

async fn atomic_write_yaml(path: &Path, contents: &str) -> Result<(), StorageError> {
    // `rename(2)` は同一ファイルシステム上で atomic なので tmpfile は隣接ディレクトリ
    // に作る。tmpfile 名衝突防止の構造:
    //
    //   - **異プロセス間**: `pid` で一意化する（PID が同時刻に再利用されるケースは
    //     OS が pid 再利用を防ぐため実質ゼロ）。
    //   - **同プロセス内**: `static AtomicU64` の `seq` が strict monotonic に増え、
    //     同 pid 内では絶対衝突しない。
    //   - `nanos` (subsec) は grep / 障害解析用の時刻ヒントで、衝突防止の根拠では
    //     ない。`pid + seq` で衝突しないので不要にも見えるが、tmp 残骸が事後解析で
    //     見つかった際に「いつ生成されたか」が分かるよう残す。
    //
    // 加えて `create_new(true)` を使うので、運悪く衝突した場合は `AlreadyExists` Err で
    // 検出可能。攻撃者・他テナントが先回りして同名 symlink を張った場合も `create_new`
    // が follow せず `EEXIST` で落ちる。
    //
    // dotfile 接頭辞は ls で隠して "中間ファイル" を示す慣習。
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .ok_or_else(|| {
            StorageError::Io(format!("players.yaml path has no file name: {}", path.display()))
        })?
        .to_string_lossy();
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{stem}.rshogi-tmp.{pid}.{nanos}.{seq}"));

    // tmpfile が rename 完走前に Err 経路へ落ちた場合の残骸を確実に掃除するための
    // RAII ガード。`renamed = true` を立てると Drop で残骸削除をスキップする。
    struct TmpCleanup<'a> {
        tmp: &'a Path,
        renamed: bool,
    }
    impl Drop for TmpCleanup<'_> {
        fn drop(&mut self) {
            if !self.renamed {
                let _ = std::fs::remove_file(self.tmp);
            }
        }
    }
    let mut cleanup = TmpCleanup {
        tmp: &tmp,
        renamed: false,
    };

    // unix では既存 `players.yaml` の mode を継承して tmp を開く（`chmod 0600` 運用で
    // 人手で締めた permission が rename で 0644 に緩む事故を防ぐ）。Windows / その他
    // 非 unix では mode 概念がないので素直に `create_new` のみで開く。
    let mut open_options = fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    // unix では `tokio::fs::OpenOptions::mode` が直接生えており trait import 不要。
    // `Permissions::mode` 側のみ `std::os::unix::fs::PermissionsExt` を要する。
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt as _;
        open_options.mode(meta.permissions().mode() & 0o7777);
    }
    let mut file = open_options
        .open(&tmp)
        .await
        .map_err(|e| StorageError::Io(format!("create {}: {}", tmp.display(), e)))?;
    file.write_all(contents.as_bytes())
        .await
        .map_err(|e| StorageError::Io(format!("write {}: {}", tmp.display(), e)))?;
    file.sync_all()
        .await
        .map_err(|e| StorageError::Io(format!("sync {}: {}", tmp.display(), e)))?;
    drop(file);
    fs::rename(&tmp, path).await.map_err(|e| {
        StorageError::Io(format!("rename {} -> {}: {}", tmp.display(), path.display(), e))
    })?;
    cleanup.renamed = true;
    drop(cleanup);

    // unix では `rename(2)` のディレクトリエントリ更新は親 inode の fsync を行うまで
    // 永続化が保証されない（ext4 / xfs の crash recovery で rename 結果が古い状態に
    // 戻るケースが知られている）。`tokio::fs::OpenOptions::open(dir).sync_all()` で
    // 親ディレクトリを fsync してクラッシュ耐性を確保する。
    //
    // Windows / 非 unix ではディレクトリ open のセマンティクス自体が異なる
    // (`CreateFileW` は `FILE_FLAG_BACKUP_SEMANTICS` 必須等) ため、ディレクトリ fsync
    // は no-op として扱う。本サーバは VPS Linux 運用が一次ターゲットで、Windows は
    // 開発・テスト経路の便宜上のサポート。
    #[cfg(unix)]
    {
        let dir_handle = fs::OpenOptions::new()
            .read(true)
            .open(dir)
            .await
            .map_err(|e| StorageError::Io(format!("open dir {}: {}", dir.display(), e)))?;
        dir_handle
            .sync_all()
            .await
            .map_err(|e| StorageError::Io(format!("fsync dir {}: {}", dir.display(), e)))?;
    }
    #[cfg(not(unix))]
    {
        // 非 unix では no-op。`dir` 引数を未使用警告から逃すため明示的に消費する。
        let _ = dir;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn rec(name: &str, rate: i32, wins: u32, losses: u32) -> PlayerRateRecord {
        PlayerRateRecord {
            name: PlayerName::new(name),
            rate,
            wins,
            losses,
            last_game_id: Some(GameId::new("20260426-001")),
            last_modified: "2026-04-26T12:34:56+00:00".to_owned(),
        }
    }

    #[test]
    fn render_document_emits_byte_stable_yaml_with_ruby_symbol_keys() {
        let mut records = HashMap::new();
        records.insert("alice".to_owned(), rec("alice", 2500, 100, 50));
        records.insert("bob".to_owned(), rec("bob", 2400, 80, 60));

        let yaml = render_document(&records);
        // Ruby `YAML.dump` のキー順は内部 Hash 挿入順だが、ここでは BTreeMap で
        // 名前昇順に正規化する。アルファベット順で alice → bob が確定。
        // `:name`/`:rate`/`:win`/`:loss`/`:last_game_id`/`:last_modified` の
        // Ruby Symbol キーが quote 無しで出力されることを byte 比較で固定する。
        // 先頭の `:` は直後が空白でなければ quote 不要なので、Ruby `YAML.dump` と
        // 同様にコロン接頭辞のみで bare key として出力される。
        // diff 検証や grep で扱いやすくする上でも quote 無しの形を期待する。
        let expected = concat!(
            "alice:\n",
            "  :name: alice\n",
            "  :rate: 2500\n",
            "  :win: 100\n",
            "  :loss: 50\n",
            "  :last_game_id: 20260426-001\n",
            "  :last_modified: 2026-04-26T12:34:56+00:00\n",
            "bob:\n",
            "  :name: bob\n",
            "  :rate: 2400\n",
            "  :win: 80\n",
            "  :loss: 60\n",
            "  :last_game_id: 20260426-001\n",
            "  :last_modified: 2026-04-26T12:34:56+00:00\n",
        );
        assert_eq!(yaml, expected);
    }

    /// `last_game_id` が `None` のレコードは `:last_game_id` 行ごと出力から
    /// 除外される。Ruby 側で `if rec[:last_game_id]`
    /// truthiness チェックする実装でも、`has_key?(:last_game_id)` で存在判定する
    /// 実装でも、どちらにも互換な見え方になる。`null` リテラルは Ruby `YAML.dump`
    /// が出さない形なので避ける。
    #[test]
    fn render_document_omits_last_game_id_when_none() {
        let mut records = HashMap::new();
        records.insert(
            "alice".to_owned(),
            PlayerRateRecord {
                name: PlayerName::new("alice"),
                rate: 1500,
                wins: 0,
                losses: 0,
                last_game_id: None,
                last_modified: "2026-04-26T00:00:00+00:00".to_owned(),
            },
        );
        let yaml = render_document(&records);
        let expected = concat!(
            "alice:\n",
            "  :name: alice\n",
            "  :rate: 1500\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :last_modified: 2026-04-26T00:00:00+00:00\n",
        );
        assert_eq!(yaml, expected);
        // round-trip しても同じ shape を保つ（None で deserialize される）。
        let parsed = parse_document(&yaml).unwrap();
        assert_eq!(parsed, records);
    }

    #[test]
    fn parse_document_round_trips_ruby_symbol_keys() {
        let mut records = HashMap::new();
        records.insert("alice".to_owned(), rec("alice", 2500, 100, 50));
        records.insert("bob".to_owned(), rec("bob", 2400, 80, 60));

        let yaml = render_document(&records);
        let parsed = parse_document(&yaml).unwrap();
        assert_eq!(parsed, records);
    }

    #[test]
    fn parse_document_accepts_optional_last_game_id_omitted() {
        // Ruby YAML で `:last_game_id:` 行ごと省略するケース（新規プレイヤで
        // 一度も対局していない状態）を許容する。
        let yaml = "alice:\n  ':name': alice\n  ':rate': 1500\n  ':win': 0\n  ':loss': 0\n  ':last_modified': '2026-04-26T00:00:00+00:00'\n";
        let parsed = parse_document(yaml).unwrap();
        assert_eq!(parsed.len(), 1);
        let r = parsed.get("alice").unwrap();
        assert!(r.last_game_id.is_none());
        assert_eq!(r.rate, 1500);
        assert_eq!(r.wins, 0);
        assert_eq!(r.losses, 0);
    }

    #[test]
    fn parse_document_rejects_malformed_yaml() {
        let err = parse_document(":not a mapping").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("players.yaml"), "expected error to mention players.yaml: {msg}");
    }

    #[test]
    fn parse_document_treats_empty_input_as_empty_map() {
        // 上位 `load_from_file` は trim 済みの空文字列を直接 `HashMap::new()` に
        // 落とす経路だが、`parse_document` 単体でも空 mapping `{}` は受理する。
        let parsed = parse_document("{}").unwrap();
        assert!(parsed.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_from_file_returns_empty_when_file_missing() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        assert!(storage.list_all().await.unwrap().is_empty());
    }

    /// **P2 回帰固定**: ファイル本体が無くても、親ディレクトリが存在しない
    /// 場合は起動段階で `StorageError::Io` を返して fail-fast する。これを
    /// 許容すると初回終局時の atomic write が必ず失敗する形で運用障害が遅延
    /// 顕在化するため、accept ループに入る前に検知する契約を固定する。
    #[tokio::test(flavor = "current_thread")]
    async fn load_from_file_fails_fast_when_parent_dir_missing() {
        let dir = tempdir();
        // 親ディレクトリ自体が存在しないパスを指す（typo / mount 漏れの設定ミス）。
        let nonexistent_parent = dir.path().join("does-not-exist");
        let path = nonexistent_parent.join("players.yaml");
        let err = PlayersYamlRateStorage::load_from_file(path).await.unwrap_err();
        let msg = format!("{err}");
        assert!(matches!(err, StorageError::Io(_)), "expected Io error, got: {msg}");
        assert!(
            msg.contains("parent directory"),
            "error message should mention parent directory: {msg}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_writes_atomic_yaml_and_round_trips() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        storage.save(&rec("alice", 2500, 100, 50)).await.unwrap();
        storage.save(&rec("bob", 2400, 80, 60)).await.unwrap();

        // Reload from disk and confirm same records exist.
        let reloaded = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        let mut names: Vec<String> = reloaded
            .list_all()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name.as_str().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["alice".to_owned(), "bob".to_owned()]);
    }

    /// `ensure_default_records` 経由で初期レコードを差し込むテストヘルパ。
    /// 旧 API（`names + initial_rate + now_iso`）の呼び出し記述を集約し、
    /// 新 API（`IntoIterator<Item = PlayerRateRecord>`）への移行で
    /// 各テストの差分を最小化する。
    fn seed_default_records(
        storage: &PlayersYamlRateStorage,
        names: &[&str],
        initial_rate: i32,
        now_iso: &str,
    ) {
        let defaults = names.iter().map(|n| PlayerRateRecord {
            name: PlayerName::new(*n),
            rate: initial_rate,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: now_iso.to_owned(),
        });
        storage.ensure_default_records(defaults);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_increments_winner_and_loser_atomically() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        seed_default_records(&storage, &["alice", "bob"], 1500, "2026-04-26T00:00:00+00:00");

        let alice = PlayerName::new("alice");
        let bob = PlayerName::new("bob");
        let game_id = GameId::new("20260426-001");
        storage
            .record_game_outcome(&alice, &bob, Some(&alice), &game_id, "2026-04-26T12:34:56+00:00")
            .await
            .unwrap();

        let alice_rec = storage.load(&alice).await.unwrap().unwrap();
        let bob_rec = storage.load(&bob).await.unwrap().unwrap();
        assert_eq!(alice_rec.wins, 1);
        assert_eq!(alice_rec.losses, 0);
        assert_eq!(bob_rec.wins, 0);
        assert_eq!(bob_rec.losses, 1);
        assert_eq!(alice_rec.last_game_id.as_ref().map(|g| g.as_str()), Some("20260426-001"));
        assert_eq!(bob_rec.last_modified, "2026-04-26T12:34:56+00:00");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_draw_updates_last_fields_only() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        seed_default_records(&storage, &["alice", "bob"], 1500, "2026-04-26T00:00:00+00:00");

        let alice = PlayerName::new("alice");
        let bob = PlayerName::new("bob");
        let game_id = GameId::new("20260426-002");
        storage
            .record_game_outcome(&alice, &bob, None, &game_id, "2026-04-26T13:00:00+00:00")
            .await
            .unwrap();

        let alice_rec = storage.load(&alice).await.unwrap().unwrap();
        let bob_rec = storage.load(&bob).await.unwrap().unwrap();
        // 千日手・最大手数では wins/losses は据置。last_* のみ更新される。
        assert_eq!(alice_rec.wins, 0);
        assert_eq!(alice_rec.losses, 0);
        assert_eq!(bob_rec.wins, 0);
        assert_eq!(bob_rec.losses, 0);
        assert_eq!(alice_rec.last_modified, "2026-04-26T13:00:00+00:00");
        assert_eq!(bob_rec.last_modified, "2026-04-26T13:00:00+00:00");
        assert_eq!(alice_rec.last_game_id.as_ref().map(|g| g.as_str()), Some("20260426-002"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_does_not_mutate_rate_value() {
        // `:rate` は外部バッチ（mk_rate）の責務。本サーバの終局処理では
        // 触れないことを契約として固定する（同居運用で踏まないために重要）。
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        storage.save(&rec("alice", 2500, 0, 0)).await.unwrap();
        storage.save(&rec("bob", 2400, 0, 0)).await.unwrap();

        let alice = PlayerName::new("alice");
        let bob = PlayerName::new("bob");
        let game_id = GameId::new("20260426-003");
        storage
            .record_game_outcome(&alice, &bob, Some(&alice), &game_id, "2026-04-26T14:00:00+00:00")
            .await
            .unwrap();

        let alice_rec = storage.load(&alice).await.unwrap().unwrap();
        let bob_rec = storage.load(&bob).await.unwrap().unwrap();
        assert_eq!(alice_rec.rate, 2500, "rate must be preserved verbatim");
        assert_eq!(bob_rec.rate, 2400, "rate must be preserved verbatim");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_recovers_from_corrupted_file_with_explicit_error() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        // 不正な YAML を直接書く（mid-write クラッシュ等のシミュレーション）。
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b":invalid: [unterminated").unwrap();
        drop(f);

        let err = PlayersYamlRateStorage::load_from_file(path).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            matches!(err, StorageError::Malformed(_)),
            "expected Malformed error, got: {msg}"
        );
    }

    /// `record_game_outcome` の契約: `black == white` が誤って渡されても
    /// `wins`/`losses` の二重加算が発生しない（self-play 防護）。debug ビルドでは
    /// `debug_assert_ne!` が落とすが、release ビルドでも `Ok(())` で早期 return
    /// して整合性を保つ。
    #[cfg(not(debug_assertions))]
    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_rejects_self_play_in_release_build() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        storage.save(&rec("alice", 1500, 5, 3)).await.unwrap();

        let alice = PlayerName::new("alice");
        let game_id = GameId::new("20260426-self");
        // 同名を black / white 両方に渡しても 2 重加算しない。
        storage
            .record_game_outcome(
                &alice,
                &alice,
                Some(&alice),
                &game_id,
                "2026-04-26T16:00:00+00:00",
            )
            .await
            .unwrap();
        let r = storage.load(&alice).await.unwrap().unwrap();
        // wins/losses は据置（save 時の値そのまま）。
        assert_eq!(r.wins, 5);
        assert_eq!(r.losses, 3);
    }

    /// 日本語ハンドル名 / 数字始まりハンドル名など quote を要する可能性がある
    /// 文字列で round-trip する契約を固定する。Ruby 互換 YAML として、
    /// UTF-8 名前の対局者を扱える運用を保証する。
    ///
    /// (1) round-trip で意味的同値を保つ
    /// (2) 日本語ハンドルは bare key として出力される（`田中太郎:` の出現を pin）
    /// (3) 数字 only ハンドル `12345` は YAML 1.2 で integer scalar に解釈されうる
    ///     ため string quote を付ける。`'12345':` の出現を pin する
    ///     （quote の有無は仕様変更で破壊的変化があるため byte 列で固定する）。
    #[test]
    fn render_and_parse_round_trip_handles_unicode_and_numeric_names() {
        let mut records = HashMap::new();
        records.insert("田中太郎".to_owned(), rec("田中太郎", 1500, 0, 0));
        records.insert("12345".to_owned(), rec("12345", 1700, 10, 5));
        let yaml = render_document(&records);
        let parsed = parse_document(&yaml).unwrap();
        assert_eq!(parsed, records, "Unicode / numeric handles must round-trip");

        // (2): 日本語ハンドルは bare で出力される。
        assert!(yaml.contains("田中太郎:\n"), "non-ASCII handle must be unquoted, got:\n{yaml}",);
        // (3): 数字 only ハンドルは integer 解釈を防ぐため quote される。
        // 現行の出力は single quote（`push_scalar_matches_serde_yaml_quoting` で固定）。
        // ここでは「quote されていること」だけを見るので double quote も許容する。
        assert!(
            yaml.contains("'12345':\n") || yaml.contains("\"12345\":\n"),
            "numeric handle must be string-quoted to avoid YAML integer coercion, got:\n{yaml}",
        );
    }

    /// quote の規則を、置き換え前の `serde_yaml` 0.9.34 の実出力で固定する。
    ///
    /// 期待値は `serde_yaml::to_string` に同じ文字列を渡して得たもの（手書きではない）。
    /// Ruby の `mk_rate` と同じファイルを共有するので、出力形式を変えないことが要件。
    #[test]
    fn push_scalar_matches_serde_yaml_quoting() {
        let cases = [
            // quote 無し
            ("alice", "alice"),
            ("田中太郎", "田中太郎"),
            ("player_1", "player_1"),
            ("a-b", "a-b"),
            ("Alice.Bob", "Alice.Bob"),
            ("2026-04-26T12:34:56+00:00", "2026-04-26T12:34:56+00:00"),
            ("20260426-001", "20260426-001"),
            ("1_000", "1_000"),
            ("yes", "yes"),
            ("on", "on"),
            ("a b", "a b"),
            ("a:b", "a:b"),
            (":sym", ":sym"),
            ("-x", "-x"),
            ("?x", "?x"),
            ("x#y", "x#y"),
            ("a'b", "a'b"),
            ("a\"b", "a\"b"),
            ("a,b", "a,b"),
            ("<<", "<<"),
            ("=", "="),
            ("a\\b", "a\\b"),
            ("a\u{a0}b", "a\u{a0}b"),
            ("😀", "😀"),
            ("é", "é"),
            // null / 真偽値 / 数値として読まれる文字列は single quote
            ("12345", "'12345'"),
            ("001", "'001'"),
            ("-1", "'-1'"),
            ("+1", "'+1'"),
            ("1e5", "'1e5'"),
            ("1.5", "'1.5'"),
            (".5", "'.5'"),
            ("0x1F", "'0x1F'"),
            ("0o17", "'0o17'"),
            ("0b11", "'0b11'"),
            (".inf", "'.inf'"),
            (".NaN", "'.NaN'"),
            ("true", "'true'"),
            ("True", "'True'"),
            ("FALSE", "'FALSE'"),
            ("null", "'null'"),
            ("~", "'~'"),
            ("", "''"),
            // plain で書くと構文上の意味が変わる文字列は single quote
            ("a: b", "'a: b'"),
            ("x:", "'x:'"),
            (":", "':'"),
            ("-", "'-'"),
            ("- x", "'- x'"),
            ("? x", "'? x'"),
            ("#x", "'#x'"),
            ("x #y", "'x #y'"),
            (" lead", "' lead'"),
            ("trail ", "'trail '"),
            ("'a", "'''a'"),
            ("\"a", "'\"a'"),
            ("@x", "'@x'"),
            ("[x", "'[x'"),
            ("{x", "'{x'"),
            ("*x", "'*x'"),
            ("&x", "'&x'"),
            ("!x", "'!x'"),
            ("|x", "'|x'"),
            (">x", "'>x'"),
            ("%x", "'%x'"),
            ("`x", "'`x'"),
            ("---", "'---'"),
            ("--- a", "'--- a'"),
            ("...", "'...'"),
            // エスケープが要る文字を含む文字列は double quote
            ("a\tb", "\"a\\tb\""),
            ("a\\\tb", "\"a\\\\\\tb\""),
            ("\u{feff}x", "\"\\uFEFFx\""),
            ("\u{7f}", "\"\\x7F\""),
            ("\u{1b}[0m", "\"\\e[0m\""),
            ("\0", "\"\\0\""),
            ("\u{7}", "\"\\a\""),
            ("\u{8}", "\"\\b\""),
            ("\u{b}", "\"\\v\""),
            ("\u{c}", "\"\\f\""),
            ("a\rb", "\"a\\rb\""),
            ("\u{85}", "\"\\N\""),
            ("\u{80}", "\"\\x80\""),
            ("\u{9f}", "\"\\x9F\""),
            ("\u{fffe}", "\"\\uFFFE\""),
            ("\u{ffff}", "\"\\uFFFF\""),
            ("a\"\tb", "\"a\\\"\\tb\""),
            ("a'\tb", "\"a'\\tb\""),
            // エスケープ不要な範囲の両端
            ("\u{a0}", "\u{a0}"),
            ("\u{2027}", "\u{2027}"),
            ("\u{202a}", "\u{202a}"),
            ("\u{d7ff}", "\u{d7ff}"),
            ("\u{e000}", "\u{e000}"),
            ("\u{fefe}", "\u{fefe}"),
            ("\u{ff00}", "\u{ff00}"),
            ("\u{fffd}", "\u{fffd}"),
            ("\u{10000}", "\u{10000}"),
            ("\u{10ffff}", "\u{10ffff}"),
            // null / 真偽値: 綴りが完全一致するものだけ quote
            ("false", "'false'"),
            ("False", "'False'"),
            ("TRUE", "'TRUE'"),
            ("Null", "'Null'"),
            ("NULL", "'NULL'"),
            ("tRUE", "tRUE"),
            ("nULL", "nULL"),
            ("null1", "null1"),
            ("truex", "truex"),
            ("~a", "~a"),
            ("True ", "'True '"),
            ("y", "y"),
            ("n", "n"),
            ("no", "no"),
            ("off", "off"),
            // 数値: 符号・0 始まり・指数
            ("0", "'0'"),
            ("-0", "'-0'"),
            ("+0", "'+0'"),
            ("00", "'00'"),
            ("-001", "'-001'"),
            ("+001", "'+001'"),
            ("1.", "'1.'"),
            ("1e+5", "'1e+5'"),
            ("1E5", "'1E5'"),
            ("1e", "1e"),
            ("e5", "e5"),
            ("1.5.5", "1.5.5"),
            ("+-1", "+-1"),
            ("-+1", "-+1"),
            ("--1", "--1"),
            ("++1", "++1"),
            ("1,000", "1,000"),
            ("1:30", "1:30"),
            ("2026-04-26", "2026-04-26"),
            // 数値: 基数つき（接頭辞は小文字のみ、桁が不正なら文字列）
            ("-0x1F", "'-0x1F'"),
            ("+0x1F", "'+0x1F'"),
            ("-0o17", "'-0o17'"),
            ("-0b11", "'-0b11'"),
            ("0x", "0x"),
            ("0xZZ", "0xZZ"),
            ("0X1F", "0X1F"),
            ("0x-1", "0x-1"),
            ("0x+1", "0x+1"),
            ("-0x-1", "-0x-1"),
            ("0o8", "0o8"),
            ("0b2", "0b2"),
            // 数値: 無限大・NaN の綴り
            (".Inf", "'.Inf'"),
            (".INF", "'.INF'"),
            ("-.inf", "'-.inf'"),
            ("+.inf", "'+.inf'"),
            ("-.Inf", "'-.Inf'"),
            (".nan", "'.nan'"),
            (".NAN", "'.NAN'"),
            ("-.nan", "-.nan"),
            ("+.nan", "+.nan"),
            (".Nan", ".Nan"),
            // 数値: 値域の境界。`serde_yaml` は u128 / i128 / 有限の f64 に収まるものだけを
            // 数値とみなして quote していたので、収まらないものは quote 無しになる。
            (
                "340282366920938463463374607431768211455",
                "'340282366920938463463374607431768211455'",
            ),
            (
                "340282366920938463463374607431768211456",
                "'340282366920938463463374607431768211456'",
            ),
            ("0xffffffffffffffffffffffffffffffff", "'0xffffffffffffffffffffffffffffffff'"),
            ("0x1ffffffffffffffffffffffffffffffff", "0x1ffffffffffffffffffffffffffffffff"),
            ("-0x80000000000000000000000000000000", "'-0x80000000000000000000000000000000'"),
            ("-0x80000000000000000000000000000001", "-0x80000000000000000000000000000001"),
            ("1e999", "1e999"),
            ("-1e999", "-1e999"),
            ("inf", "inf"),
            ("-inf", "-inf"),
            ("nan", "nan"),
            ("NaN", "NaN"),
            ("infinity", "infinity"),
            // indicator の位置による違い
            (",x", "',x'"),
            ("]x", "']x'"),
            ("}x", "'}x'"),
            ("?", "'?'"),
            ("#", "'#'"),
            ("a?", "a?"),
            ("a-", "a-"),
            ("a:", "'a:'"),
            ("a :b", "a :b"),
            ("a# b", "a# b"),
            ("a #", "'a #'"),
            ("x: y: z", "'x: y: z'"),
            ("...x", "'...x'"),
            ("---x", "'---x'"),
            ("..", ".."),
            ("--", "--"),
            ("-- -", "-- -"),
            ("a  b", "a  b"),
            (" ", "' '"),
            ("''", "''''''"),
            ("it's", "it's"),
            ("a.b@example.com", "a.b@example.com"),
            (
                "wdoor+floodgate-300-10F+alice+bob+20260426123456",
                "wdoor+floodgate-300-10F+alice+bob+20260426123456",
            ),
            ("2026-04-26 12:34:56 +09:00", "2026-04-26 12:34:56 +09:00"),
        ];
        for (input, expected) in cases {
            let mut out = String::new();
            push_scalar(&mut out, input);
            assert_eq!(out, expected, "input: {input:?}");
        }
    }

    /// 改行文字を含む文字列は double quote + エスケープで書く。
    ///
    /// ここだけは `serde_yaml` の出力（block scalar、U+2028 / U+2029 は生のまま）と
    /// 意図して変えているので、新しい形を固定し、読み戻せることを確認する。
    #[test]
    fn push_scalar_escapes_line_breaks() {
        let cases = [
            ("a\nb", "\"a\\nb\""),
            ("a\n", "\"a\\n\""),
            ("\r", "\"\\r\""),
            ("a\u{2028}b", "\"a\\Lb\""),
            ("a\u{2029}b", "\"a\\Pb\""),
        ];
        for (input, expected) in cases {
            let mut out = String::new();
            push_scalar(&mut out, input);
            assert_eq!(out, expected, "input: {input:?}");

            let mut records = HashMap::new();
            let mut record = rec(input, 1500, 0, 0);
            record.last_game_id = Some(GameId::new(input));
            record.last_modified = input.to_owned();
            records.insert(input.to_owned(), record);
            assert_eq!(
                parse_document(&render_document(&records)).unwrap(),
                records,
                "input: {input:?}"
            );
        }
    }

    /// キーを `key:` と `? key` のどちらで書くかの境界（128 byte）を固定する。
    ///
    /// 期待値は `serde_yaml` 0.9.34 の実出力に合わせてある。長さは文字数ではなく
    /// byte 数で数え、quote が要るキーは `? 'key'` の形になる。
    #[test]
    fn render_document_switches_to_explicit_key_above_128_bytes() {
        let cases = [
            ("k".repeat(128), false, false),
            ("k".repeat(129), true, false),
            // 44 文字 / 128 byte
            ("あ".repeat(42) + "kk", false, false),
            // 43 文字 / 129 byte
            ("あ".repeat(43), true, false),
            ("1".repeat(129), true, true),
        ];
        for (name, explicit, quoted) in cases {
            let mut records = HashMap::new();
            let mut record = rec(&name, 1500, 0, 0);
            record.last_game_id = None;
            records.insert(name.clone(), record);

            let scalar = if quoted {
                format!("'{name}'")
            } else {
                name.clone()
            };
            let head = if explicit {
                format!("? {scalar}\n: :name: {scalar}\n")
            } else {
                format!("{scalar}:\n  :name: {scalar}\n")
            };
            let expected = format!(
                "{head}  :rate: 1500\n  :win: 0\n  :loss: 0\n  :last_modified: 2026-04-26T12:34:56+00:00\n"
            );
            let yaml = render_document(&records);
            assert_eq!(yaml, expected, "key length: {}", name.len());
            assert_eq!(parse_document(&yaml).unwrap(), records, "key length: {}", name.len());
        }
    }

    /// quote 無しで書かれた数値・真偽値の形の値も、文字列の項目には文字列として読む
    /// （手書きのファイルや、名前を quote しない書き手への備え）。`:last_game_id` の
    /// `~` は未対局（`None`）として読む。
    #[test]
    fn parse_document_reads_unquoted_scalars_as_strings() {
        let yaml = concat!(
            "12345:\n",
            "  :name: 12345\n",
            "  :rate: 1500\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :last_game_id: 20260426\n",
            "  :last_modified: 2026-04-26\n",
            "true:\n",
            "  :name: true\n",
            "  :rate: -20\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :last_game_id: ~\n",
            "  :last_modified: 1.5\n",
        );
        let parsed = parse_document(yaml).unwrap();
        let numeric = parsed.get("12345").unwrap();
        assert_eq!(numeric.name.as_str(), "12345");
        assert_eq!(numeric.last_game_id.as_ref().map(|g| g.as_str()), Some("20260426"));
        assert_eq!(numeric.last_modified, "2026-04-26");
        let boolean = parsed.get("true").unwrap();
        assert_eq!(boolean.name.as_str(), "true");
        assert_eq!(boolean.rate, -20);
        assert_eq!(boolean.last_game_id, None);
        assert_eq!(boolean.last_modified, "1.5");
    }

    /// 置き換え前の実装（`serde_yaml` 0.9.34）が書いた `players.yaml` を読める。
    ///
    /// fixture は旧実装の実出力そのまま。数字だけの名前の quote、空文字列、`<<` という
    /// 名前、改行入りの名前（explicit key + block scalar）、129 byte の名前（explicit
    /// key）、空白区切りの時刻を含む。読んだ結果を書き戻すと、`<<` と改行入りの
    /// レコード以外は旧実装と同じバイト列になることも確認する。
    #[test]
    fn parse_document_reads_file_written_by_serde_yaml() {
        let long = "n".repeat(129);
        let numeric_and_merge = concat!(
            "'12345':\n",
            "  :name: '12345'\n",
            "  :rate: 1500\n",
            "  :win: 3\n",
            "  :loss: 4\n",
            "  :last_game_id: '20260426'\n",
            "  :last_modified: 2026-04-26T12:34:56+00:00\n",
            "<<:\n",
            "  :name: <<\n",
            "  :rate: 1500\n",
            "  :win: 3\n",
            "  :loss: 4\n",
            "  :last_modified: ''\n",
        );
        let multiline = concat!(
            "? |-\n",
            "  a\n",
            "  b\n",
            ": :name: |-\n",
            "    a\n",
            "    b\n",
            "  :rate: 1500\n",
            "  :win: 3\n",
            "  :loss: 4\n",
            "  :last_game_id: |\n",
            "    x\n",
            "    y\n",
            "  :last_modified: 2026-04-26T00:00:00+00:00\n",
        );
        let rest = format!(
            concat!(
                "'a b: c':\n",
                "  :name: 'a b: c'\n",
                "  :rate: 1500\n",
                "  :win: 3\n",
                "  :loss: 4\n",
                "  :last_modified: 2026-04-26 12:34:56 +09:00\n",
                "alice:\n",
                "  :name: alice\n",
                "  :rate: 1500\n",
                "  :win: 3\n",
                "  :loss: 4\n",
                "  :last_game_id: 20260426-001\n",
                "  :last_modified: 2026-04-26T12:34:56+00:00\n",
                "? {long}\n",
                ": :name: {long}\n",
                "  :rate: 1500\n",
                "  :win: 3\n",
                "  :loss: 4\n",
                "  :last_modified: 2026-04-26T00:00:00+00:00\n",
                "田中太郎:\n",
                "  :name: 田中太郎\n",
                "  :rate: 1500\n",
                "  :win: 3\n",
                "  :loss: 4\n",
                "  :last_modified: 2026-04-26T00:00:00+00:00\n",
            ),
            long = long
        );
        let written_by_serde_yaml = format!("{numeric_and_merge}{multiline}{rest}");

        let mut expected = HashMap::new();
        for (name, game_id, last_modified) in [
            ("12345", Some("20260426"), "2026-04-26T12:34:56+00:00"),
            ("<<", None, ""),
            ("a\nb", Some("x\ny\n"), "2026-04-26T00:00:00+00:00"),
            ("a b: c", None, "2026-04-26 12:34:56 +09:00"),
            ("alice", Some("20260426-001"), "2026-04-26T12:34:56+00:00"),
            (long.as_str(), None, "2026-04-26T00:00:00+00:00"),
            ("田中太郎", None, "2026-04-26T00:00:00+00:00"),
        ] {
            expected.insert(
                name.to_owned(),
                PlayerRateRecord {
                    name: PlayerName::new(name),
                    rate: 1500,
                    wins: 3,
                    losses: 4,
                    last_game_id: game_id.map(GameId::new),
                    last_modified: last_modified.to_owned(),
                },
            );
        }
        let mut parsed = parse_document(&written_by_serde_yaml).unwrap();
        assert_eq!(parsed, expected);

        // 書き戻すと、`<<` という名前（`!!str` タグを付ける）と改行入りのレコード
        // （double quote + エスケープ）だけ旧実装と形が違う。どちらも意図した差。
        let rendered_multiline = concat!(
            "\"a\\nb\":\n",
            "  :name: \"a\\nb\"\n",
            "  :rate: 1500\n",
            "  :win: 3\n",
            "  :loss: 4\n",
            "  :last_game_id: \"x\\ny\\n\"\n",
            "  :last_modified: 2026-04-26T00:00:00+00:00\n",
        );
        assert_eq!(
            render_document(&parsed),
            format!(
                "{}{rendered_multiline}{rest}",
                numeric_and_merge.replace("<<:\n", "!!str <<:\n")
            )
        );
        assert_eq!(parse_document(&render_document(&parsed)).unwrap(), expected);

        // 残りは旧実装の出力とバイト一致する。
        parsed.remove("a\nb");
        parsed.remove("<<");
        assert_eq!(
            render_document(&parsed),
            format!("{}{rest}", numeric_and_merge.split("<<:\n").next().unwrap())
        );
    }

    /// プレイヤ名と game id は、Ruby の YAML ローダーが文字列以外の型に読む形なら
    /// quote する（`serde_yaml` は quote していなかった）。それ以外は quote しない。
    #[test]
    fn push_text_quotes_what_ruby_reads_as_non_string() {
        let cases = [
            // 真偽値・nil（大文字小文字を問わない）
            ("yes", "'yes'"),
            ("Yes", "'Yes'"),
            ("NO", "'NO'"),
            ("on", "'on'"),
            ("oFF", "'oFF'"),
            ("y", "'y'"),
            ("N", "'N'"),
            ("tRUE", "'tRUE'"),
            ("nULL", "'nULL'"),
            // Symbol
            (":foo", "':foo'"),
            // 日付・時刻
            ("2026-04-26", "'2026-04-26'"),
            ("2026-4-6", "'2026-4-6'"),
            ("2026-04-26T12:34:56+00:00", "'2026-04-26T12:34:56+00:00'"),
            ("2026-04-26 12:34:56 +09:00", "'2026-04-26 12:34:56 +09:00'"),
            // 数値: 区切りつき・60 進・値域を超える 16 進・綴り違いの無限大 / NaN
            ("1_000", "'1_000'"),
            ("1,000", "'1,000'"),
            ("-1_000", "'-1_000'"),
            // 符号 + 区切り始まり（Ruby 2.3 が整数として読む）
            ("+,5", "'+,5'"),
            ("-_29", "'-_29'"),
            ("1_000.5", "'1_000.5'"),
            ("1:30", "'1:30'"),
            ("1:30:45.5", "'1:30:45.5'"),
            ("0_7", "'0_7'"),
            ("0x_1F", "'0x_1F'"),
            ("0x1ffffffffffffffffffffffffffffffff", "'0x1ffffffffffffffffffffffffffffffff'"),
            (".iNf", "'.iNf'"),
            ("-.INF", "'-.INF'"),
            (".nAn", "'.nAn'"),
            // `push_scalar` の規則ですでに quote されるものはそのまま
            ("12345", "'12345'"),
            ("true", "'true'"),
            ("", "''"),
            ("a\tb", "\"a\\tb\""),
            // 文字列としてしか読まれないものは quote しない
            ("alice", "alice"),
            ("田中太郎", "田中太郎"),
            ("player_1", "player_1"),
            ("yesman", "yesman"),
            ("20260426-001", "20260426-001"),
            ("2ch_bot", "2ch_bot"),
            ("3gatsu", "3gatsu"),
            ("x1_000", "x1_000"),
            ("_29", "_29"),
            ("-x", "-x"),
            ("+a", "+a"),
            ("a:b", "a:b"),
            ("<<", "<<"),
            (
                "wdoor+floodgate-300-10F+alice+bob+20260426123456",
                "wdoor+floodgate-300-10F+alice+bob+20260426123456",
            ),
        ];
        for (input, expected) in cases {
            let mut out = String::new();
            push_text(&mut out, input);
            assert_eq!(out, expected, "input: {input:?}");
        }
    }

    /// プレイヤ名と `:last_game_id` には Ruby 向けの quote を掛け、`:last_modified` の
    /// 時刻は quote 無しのまま書く。`<<` という名前は `!!str` タグつきのキーにする。
    #[test]
    fn render_document_quotes_names_but_not_last_modified() {
        let mut records = HashMap::new();
        for (name, game_id) in [("yes", "2026-04-26"), ("<<", "1_000")] {
            let mut record = rec(name, 1500, 0, 0);
            record.last_game_id = Some(GameId::new(game_id));
            records.insert(name.to_owned(), record);
        }
        let yaml = render_document(&records);
        let expected = concat!(
            "!!str <<:\n",
            "  :name: <<\n",
            "  :rate: 1500\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :last_game_id: '1_000'\n",
            "  :last_modified: 2026-04-26T12:34:56+00:00\n",
            "'yes':\n",
            "  :name: 'yes'\n",
            "  :rate: 1500\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :last_game_id: '2026-04-26'\n",
            "  :last_modified: 2026-04-26T12:34:56+00:00\n",
        );
        assert_eq!(yaml, expected);
        assert_eq!(parse_document(&yaml).unwrap(), records);
    }

    /// Ruby の `YAML.dump` が書く形（`---` 始まり、未知のキーあり）を読める。
    ///
    /// fixture は手書き（このテストは Ruby を実行しない）。
    #[test]
    fn parse_document_reads_document_with_start_marker_and_unknown_keys() {
        let yaml = concat!(
            "---\n",
            "alice:\n",
            "  :name: alice\n",
            "  :rate: 2500\n",
            "  :win: 100\n",
            "  :loss: 50\n",
            "  :rating_group: 0\n",
            "  :last_modified: 2026-04-26 12:34:56.000000000 +09:00\n",
        );
        let parsed = parse_document(yaml).unwrap();
        let alice = parsed.get("alice").unwrap();
        assert_eq!(alice.rate, 2500);
        assert_eq!(alice.last_game_id, None);
        assert_eq!(alice.last_modified, "2026-04-26 12:34:56.000000000 +09:00");
    }

    /// 同じプレイヤ名が重複していたら後のレコードを採る。
    #[test]
    fn parse_document_keeps_last_record_for_duplicate_name() {
        let record = |rate: i32| {
            format!(
                "alice:\n  :name: alice\n  :rate: {rate}\n  :win: 0\n  :loss: 0\n  :last_modified: t\n"
            )
        };
        let parsed = parse_document(&format!("{}{}", record(1500), record(1600))).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get("alice").unwrap().rate, 1600);
    }

    /// 数値として読むと同じ値になる名前も、書かれた文字列のまま別のプレイヤとして読む。
    ///
    /// fixture は `serde_yaml` 0.9.34 の実出力。旧実装はこれらを quote 無しで書いて
    /// いたので、キーを数値として解釈するデシリアライザだと片方のレコードが消える。
    #[test]
    fn parse_document_keeps_names_that_are_equal_as_numbers() {
        let names = ["+1_0", "0X10", "0x_10", "10_00", "1_0", "1_000"];
        let written_by_serde_yaml: String = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                format!(
                    "{name}:\n  :name: {name}\n  :rate: {}\n  :win: 0\n  :loss: 0\n  :last_modified: t\n",
                    1500 + i
                )
            })
            .collect();
        let parsed = parse_document(&written_by_serde_yaml).unwrap();
        assert_eq!(parsed.len(), names.len());
        for (i, name) in names.iter().enumerate() {
            let record = parsed.get(*name).unwrap();
            assert_eq!(record.name.as_str(), *name);
            assert_eq!(record.rate, 1500 + i as i32);
        }
        // 書き戻すとすべて quote され、読み直しても同じ内容になる。
        assert_eq!(parse_document(&render_document(&parsed)).unwrap(), parsed);
    }

    /// Ruby の `YAML.dump` は同じ文字列オブジェクトを 2 回書くとき anchor / alias を
    /// 使う（対局した 2 人の `:last_game_id` など）。alias 側も同じ文字列として読む。
    #[test]
    fn parse_document_resolves_scalar_aliases() {
        let yaml = concat!(
            "---\n",
            "alice:\n",
            "  :name: alice\n",
            "  :rate: 1500\n",
            "  :win: 1\n",
            "  :loss: 0\n",
            "  :last_game_id: &1 20260426-001\n",
            "  :last_modified: 2026-04-26 12:34:56 +09:00\n",
            "bob:\n",
            "  :name: bob\n",
            "  :rate: 1500\n",
            "  :win: 0\n",
            "  :loss: 1\n",
            "  :last_game_id: *1\n",
            "  :last_modified: 2026-04-26 12:34:56 +09:00\n",
        );
        let parsed = parse_document(yaml).unwrap();
        for name in ["alice", "bob"] {
            let record = parsed.get(name).unwrap();
            assert_eq!(record.last_game_id.as_ref().map(|g| g.as_str()), Some("20260426-001"));
        }
    }

    /// 知らない項目は、値が mapping や sequence でも読み飛ばす。同じ項目が重複して
    /// いたら後のものを採る。
    #[test]
    fn parse_document_skips_unknown_fields_and_keeps_last_duplicate_field() {
        let yaml = concat!(
            "alice:\n",
            "  :name: alice\n",
            "  :history:\n",
            "    - {:game: g1, :result: win}\n",
            "    - [1, 2, [3]]\n",
            "  :rate: 1500\n",
            "  :rate: 1600\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :extra: {a: {b: c}}\n",
            "  :last_modified: t\n",
            "bob:\n",
            "  :name: bob\n",
            "  :rate: 1400\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :last_modified: t\n",
        );
        let parsed = parse_document(yaml).unwrap();
        assert_eq!(parsed.get("alice").unwrap().rate, 1600);
        assert_eq!(parsed.get("bob").unwrap().rate, 1400);
    }

    /// 形が合わないファイルは、理由と行番号つきの `Malformed` で拒否する。
    #[test]
    fn parse_document_rejects_unexpected_shapes() {
        let record = "  :name: a\n  :rate: 1\n  :win: 0\n  :loss: 0\n  :last_modified: t\n";
        let cases = [
            // 必須項目が無い
            (
                "alice:\n  :name: alice\n  :rate: 1\n  :win: 0\n  :loss: 0\n",
                "missing field `:last_modified`",
            ),
            // 数値でない
            ("alice:\n  :name: alice\n  :rate: abc\n", "invalid value \"abc\" for `:rate`"),
            ("alice:\n  :name: alice\n  :win: -1\n", "invalid value \"-1\" for `:win`"),
            // レコードが mapping でない
            ("alice: 1500\n", "must be a mapping"),
            ("- alice\n", "expected a mapping of players"),
            // 文字列の項目が mapping
            ("alice:\n  :name: {a: b}\n", "`:name` must be a scalar"),
        ];
        for (yaml, expected) in cases {
            let message = parse_document(yaml).unwrap_err().to_string();
            assert!(message.contains(expected), "yaml: {yaml:?}, message: {message}");
            assert!(message.contains("line"), "yaml: {yaml:?}, message: {message}");
        }
        // 2 つ目の文書がある
        let two_documents = format!("a:\n{record}---\nb:\n{record}");
        let message = parse_document(&two_documents).unwrap_err().to_string();
        assert!(message.contains("single YAML document"), "message: {message}");
    }

    /// 空のファイル・コメントだけ・`---` だけの文書は、プレイヤ 0 人として読む。
    #[test]
    fn parse_document_treats_blank_documents_as_empty() {
        for yaml in ["", "# no players yet\n", "---\n", "--- {}\n", "--- ~\n"] {
            assert!(parse_document(yaml).unwrap().is_empty(), "yaml: {yaml:?}");
        }
    }

    /// レコードが 1 件も無いときは空の mapping を書き、読み戻せる。
    #[test]
    fn render_document_emits_empty_mapping_for_no_records() {
        let yaml = render_document(&HashMap::new());
        assert_eq!(yaml, "{}\n");
        assert!(parse_document(&yaml).unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ensure_default_records_does_not_overwrite_existing() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        storage.save(&rec("alice", 2500, 100, 50)).await.unwrap();

        // alice は既に rate=2500/wins=100 で記録済み。ensure_default_records が
        // これを TOML 由来の既定値で上書きしないことを確認。
        seed_default_records(&storage, &["alice", "bob"], 1500, "2026-04-26T15:00:00+00:00");
        let alice_rec = storage.load(&PlayerName::new("alice")).await.unwrap().unwrap();
        assert_eq!(alice_rec.rate, 2500);
        assert_eq!(alice_rec.wins, 100);
        let bob_rec = storage.load(&PlayerName::new("bob")).await.unwrap().unwrap();
        assert_eq!(bob_rec.rate, 1500);
        assert_eq!(bob_rec.wins, 0);
    }

    /// **P1 回帰固定**: TOML 由来の `PlayerRateRecord`（rate / wins / losses が
    /// 1500 以外）が YAML 未登録プレイヤの補填値として実際に使われることを検証する。
    /// 旧実装は `into_keys()` で名前のみ抽出して固定値 `1500` で補填していたため
    /// TOML 側の運用値を黙って失う bug があった。新 API では `PlayerRateRecord`
    /// 全体を受け取るため、未登録分は TOML 値そのままで in-memory 登録される。
    #[tokio::test(flavor = "current_thread")]
    async fn ensure_default_records_uses_toml_rate_wins_losses_for_unseen_players() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        // YAML には何もない状態。TOML 側で carol が rate=1800/wins=12/losses=7
        // と運用されているケースを模倣する。
        let toml_seed = vec![PlayerRateRecord {
            name: PlayerName::new("carol"),
            rate: 1800,
            wins: 12,
            losses: 7,
            last_game_id: None,
            last_modified: "2026-04-26T15:00:00+00:00".to_owned(),
        }];
        storage.ensure_default_records(toml_seed);
        let carol = storage.load(&PlayerName::new("carol")).await.unwrap().unwrap();
        assert_eq!(carol.rate, 1800, "TOML rate must be honored for unseen players");
        assert_eq!(carol.wins, 12, "TOML wins must be honored for unseen players");
        assert_eq!(carol.losses, 7, "TOML losses must be honored for unseen players");
    }

    /// `tempfile` クレートを使わずに済ませるため、テスト専用の薄い RAII
    /// ディレクトリを定義する。`Drop` で再帰削除する。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            // テスト失敗時のクリーンアップ漏れは許容（系列番号衝突は確率的に低い）
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn tempdir() -> TempDir {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        // counter を使って同 pid 内のテスト間衝突を避ける
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("rshogi-players-yaml-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("create tempdir");
        TempDir { path }
    }
}
