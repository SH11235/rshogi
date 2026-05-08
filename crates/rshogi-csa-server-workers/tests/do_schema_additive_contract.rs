//! `SCHEMA_SQL` (`crates/rshogi-csa-server-workers/src/game_room.rs`) の
//! additive-only contract 検証。
//!
//! `SCHEMA_SQL` は GameRoom DO instance 構築時に `state.storage().sql().exec(...)`
//! で **毎回そのまま exec される** (`game_room.rs::DurableObject::new`)。
//! `wrangler rollback` で Worker code を巻き戻しても、各 DO instance に既に
//! 適用された SQLite schema は undo されない。したがって `SCHEMA_SQL` に
//! 直接書ける DDL は、**毎回 exec しても安全** (= 冪等) かつ非破壊的なものに
//! 限定する必要がある。
//!
//! 具体的には `CREATE TABLE IF NOT EXISTS <name> ( ... )` のみを許可する
//! (Issue #638 / `docs/csa-server/deployment.md` §3.4.1)。SQLite の
//! `ALTER TABLE ... ADD COLUMN` は同名 column が既に存在すると
//! `duplicate column name` で fail するため、再 exec で no-op にならない。
//! column 追加が必要になった場合は `SCHEMA_SQL` に直接入れず、
//! `PRAGMA table_info` 等で存在確認してから ADD COLUMN する **guarded
//! migration helper** を別途用意するルールにする (本 contract の対象外)。
//!
//! 本 test は `SCHEMA_SQL` を**意味解析せず**、limited な contract checker として:
//!
//! - 行コメント / ブロックコメント / quoted token (`'...'` / `"..."`) を除去して
//!   destructive keyword の誤検出を避ける
//! - `;` で statement 分割し、空 statement は skip
//! - 各 statement の先頭 keyword に基づいて allow / deny を判定
//! - destructive keyword (`DROP` / `TRUNCATE` / `RENAME` / `ALTER`) は単語境界付き
//!   regex 風の手書き判定で検出
//!
//! quoted token を除去するのは destructive keyword と同名の identifier が
//! 文字列リテラル内に登場した場合に false positive を起こさないためで、
//! 本 parser は SQL 構文の意味解析をしているわけではない。

use std::sync::LazyLock;

/// `src/game_room.rs` の `const SCHEMA_SQL: &str = r#"..."#;` の中身を抽出した文字列。
/// テスト 1 本ごとに file I/O + parse を繰り返さないため `LazyLock` で 1 回化する。
static SCHEMA_SQL: LazyLock<String> = LazyLock::new(load_schema_sql_from_source);

/// `src/game_room.rs` を file I/O で読み、`const SCHEMA_SQL: &str = r#"..."#;`
/// ブロックを抽出する。`SCHEMA_SQL` は private const なので production crate の
/// 公開面を広げず、test 側で source を直接読む方針を取る。
fn load_schema_sql_from_source() -> String {
    let source_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("game_room.rs");
    let raw = std::fs::read_to_string(&source_path).unwrap_or_else(|e| {
        panic!("failed to read {}: {e}", source_path.display());
    });

    let marker = "const SCHEMA_SQL: &str = r#\"";
    let start = raw.find(marker).unwrap_or_else(|| {
        panic!(
            "{}: could not locate `{}` marker; the SCHEMA_SQL declaration may have changed shape",
            source_path.display(),
            marker,
        );
    });
    let body_start = start + marker.len();
    let end = raw[body_start..].find("\"#;").unwrap_or_else(|| {
        panic!(
            "{}: could not locate closing `\"#;` for SCHEMA_SQL after byte offset {body_start}",
            source_path.display(),
        );
    });
    raw[body_start..body_start + end].to_owned()
}

/// SQL から `--` 行コメント / `/* ... */` ブロックコメント / `'...'` /
/// `"..."` quoted token を除去する。destructive keyword 検出の前段で
/// false positive を避けるための limited normalizer であり、SQL 構文を
/// 解釈しているわけではない (本 contract checker の責務範囲外)。
fn strip_comments_and_strings(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0usize;
    while i < bytes.len() {
        // ブロックコメント
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            out.push(' ');
            continue;
        }
        // 行コメント
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        // single-quote 文字列 (SQLite は `''` で escape)
        if bytes[i] == b'\'' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2; // escaped quote
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(' ');
            continue;
        }
        // double-quote identifier (SQLite は `""` で escape)
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(' ');
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// quoted token / コメント除去済みの文字列を `;` で statement 分割し、
/// 空白のみの要素は除外する。
fn split_statements(normalized: &str) -> Vec<String> {
    normalized
        .split(';')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 単語境界 (= 英数字・アンダースコア以外) で囲まれた `needle` を ascii
/// case-insensitive に含むかを判定する。`DROP` を `DROPPED` にマッチさせない
/// ために自前で境界判定を行う (regex crate を取り込まず std のみで完結させる)。
fn contains_word(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || n.len() > h.len() {
        return false;
    }
    fn is_word_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }
    let mut i = 0usize;
    while i + n.len() <= h.len() {
        let prev_ok = i == 0 || !is_word_byte(h[i - 1]);
        let next_ok = i + n.len() == h.len() || !is_word_byte(h[i + n.len()]);
        if prev_ok && next_ok && h[i..i + n.len()].eq_ignore_ascii_case(n) {
            return true;
        }
        i += 1;
    }
    false
}

/// 1 statement を contract に照らして検査する。違反があれば `Err(reason)` を返す。
///
/// `SCHEMA_SQL` は DO instance 構築時に毎回そのまま exec されるため、
/// 許可するのは **冪等な** `CREATE TABLE IF NOT EXISTS` のみ。
/// `ALTER TABLE ADD COLUMN` を含むそれ以外の DDL は `SCHEMA_SQL` 直結では
/// 不可で、guarded migration helper 側に分離する必要がある (本 test の責務外)。
fn check_statement(stmt: &str) -> Result<(), String> {
    // 1. destructive / re-exec で fail する keyword を単語境界判定で先に弾く。
    // `ALTER` も SCHEMA_SQL の文脈では fail (re-exec で duplicate column / 型違反)。
    for bad in ["DROP", "TRUNCATE", "RENAME", "ALTER"] {
        if contains_word(stmt, bad) {
            return Err(format!(
                "keyword `{bad}` is not allowed in SCHEMA_SQL: it is either destructive \
                 or non-idempotent on re-exec (SCHEMA_SQL runs on every DO instance build). \
                 Use a guarded migration helper instead. \
                 See docs/csa-server/deployment.md §3.4.1. statement: `{stmt}`"
            ));
        }
    }

    // 2. 先頭 token を取り出す。識別子境界 = ascii 英数字 + `_`。
    let first_token = stmt
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find(|tok| !tok.is_empty())
        .unwrap_or("");
    let head = first_token.to_ascii_uppercase();

    // 3. 先頭が `CREATE` 以外 → fail (許可するのは CREATE TABLE IF NOT EXISTS のみ)
    if head != "CREATE" {
        return Err(format!(
            "unsupported statement head `{first_token}`; only \
             `CREATE TABLE IF NOT EXISTS ...` is allowed in SCHEMA_SQL. \
             See docs/csa-server/deployment.md §3.4.1. statement: `{stmt}`"
        ));
    }

    // 4. CREATE TABLE のみ許可。CREATE INDEX / CREATE TRIGGER 等は未許可。
    let mut tokens = stmt
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|tok| !tok.is_empty());
    let _ = tokens.next(); // CREATE
    let second = tokens.next().unwrap_or("").to_ascii_uppercase();
    if second != "TABLE" {
        return Err(format!(
            "only `CREATE TABLE` is allowed (got `CREATE {second}`); \
             see docs/csa-server/deployment.md §3.4.1. statement: `{stmt}`"
        ));
    }

    // 5. `IF NOT EXISTS` を必須化 (再 exec で no-op になる契約)。
    if !contains_word(stmt, "IF") || !contains_word(stmt, "NOT") || !contains_word(stmt, "EXISTS") {
        return Err(format!(
            "`CREATE TABLE` must include `IF NOT EXISTS` for idempotent re-exec; \
             see docs/csa-server/deployment.md §3.4.1. statement: `{stmt}`"
        ));
    }
    Ok(())
}

/// `SCHEMA_SQL` 全体に対する contract checker。違反があれば `Err(Vec<String>)`。
fn check_schema_sql(sql: &str) -> Result<(), Vec<String>> {
    let normalized = strip_comments_and_strings(sql);
    let statements = split_statements(&normalized);
    let mut errors = Vec::new();
    for stmt in &statements {
        if let Err(e) = check_statement(stmt) {
            errors.push(e);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// --- 実 SCHEMA_SQL に対する contract assert ----------------------------------

#[test]
fn schema_sql_satisfies_additive_only_contract() {
    let sql = SCHEMA_SQL.as_str();
    if let Err(errors) = check_schema_sql(sql) {
        panic!(
            "SCHEMA_SQL violates additive-only contract \
             (docs/csa-server/deployment.md §3.4.1):\n  - {}\n\nSCHEMA_SQL:\n{}",
            errors.join("\n  - "),
            sql,
        );
    }
}

#[test]
fn schema_sql_extracts_at_least_one_statement() {
    // marker 抽出失敗が「全 statement 0 件 = 自動 pass」になる退化を防ぐ。
    let normalized = strip_comments_and_strings(SCHEMA_SQL.as_str());
    let statements = split_statements(&normalized);
    assert!(
        !statements.is_empty(),
        "SCHEMA_SQL extraction yielded 0 statements; source-file marker may have drifted"
    );
}

// --- contract checker 自身の self-test (negative cases) ---------------------

#[test]
fn rejects_create_table_without_if_not_exists() {
    let bad = "CREATE TABLE moves (ply INTEGER);";
    let err = check_schema_sql(bad).expect_err("must reject CREATE TABLE without IF NOT EXISTS");
    assert!(
        err.iter().any(|m| m.contains("IF NOT EXISTS")),
        "expected IF NOT EXISTS error, got: {err:?}"
    );
}

#[test]
fn rejects_drop_table() {
    let bad = "DROP TABLE moves;";
    let err = check_schema_sql(bad).expect_err("must reject DROP TABLE");
    // `DROP` は head 不一致 (`DROP` は CREATE/ALTER 外) で拒否されるか、
    // destructive keyword 検出で拒否されるか、いずれかの経路で fail すれば良い。
    assert!(
        err.iter().any(|m| m.contains("DROP") || m.contains("unsupported")),
        "expected DROP rejection, got: {err:?}"
    );
}

#[test]
fn rejects_alter_table_drop_column() {
    let bad = "ALTER TABLE moves DROP COLUMN ply;";
    let err = check_schema_sql(bad).expect_err("must reject ALTER TABLE DROP COLUMN");
    assert!(err.iter().any(|m| m.contains("DROP")), "expected DROP detection, got: {err:?}");
}

#[test]
fn rejects_alter_table_rename() {
    let bad = "ALTER TABLE moves RENAME TO moves_v2;";
    let err = check_schema_sql(bad).expect_err("must reject ALTER TABLE RENAME");
    assert!(
        err.iter().any(|m| m.contains("RENAME")),
        "expected RENAME detection, got: {err:?}"
    );
}

#[test]
fn rejects_create_index() {
    let bad = "CREATE INDEX IF NOT EXISTS moves_color_idx ON moves(color);";
    let err = check_schema_sql(bad).expect_err("must reject CREATE INDEX (currently out of scope)");
    assert!(
        err.iter().any(|m| m.contains("CREATE TABLE")),
        "expected `CREATE TABLE`-only enforcement, got: {err:?}"
    );
}

#[test]
fn rejects_alter_table_add_column_in_schema_sql() {
    // SQLite の `ALTER TABLE ADD COLUMN` は同名 column 存在時に
    // duplicate column error になり、`SCHEMA_SQL` の毎回再 exec 契約と
    // 衝突するため、`SCHEMA_SQL` 直結では許可しない。
    let bad = "ALTER TABLE moves ADD COLUMN comment TEXT;";
    let err = check_schema_sql(bad)
        .expect_err("must reject ALTER TABLE ADD COLUMN inside SCHEMA_SQL (non-idempotent)");
    assert!(
        err.iter().any(|m| m.contains("ALTER")),
        "expected ALTER rejection, got: {err:?}"
    );
}

#[test]
fn accepts_create_table_if_not_exists() {
    let good = "CREATE TABLE IF NOT EXISTS moves (ply INTEGER PRIMARY KEY, color TEXT NOT NULL);";
    check_schema_sql(good).expect("must accept CREATE TABLE IF NOT EXISTS");
}

#[test]
fn ignores_destructive_keyword_in_string_literal() {
    // 'DROP' は文字列リテラル内なので contract 違反として検出されない契約。
    // (column default 値などで実用上発生しうる)
    let good = "CREATE TABLE IF NOT EXISTS moves (ply INTEGER, note TEXT DEFAULT 'no DROP here');";
    check_schema_sql(good).expect("string-literal DROP must not trigger destructive detection");
}

#[test]
fn ignores_destructive_keyword_in_line_comment() {
    let good = "-- DROP TABLE noted here\nCREATE TABLE IF NOT EXISTS moves (ply INTEGER);";
    check_schema_sql(good).expect("line-comment DROP must not trigger destructive detection");
}

#[test]
fn ignores_destructive_keyword_in_block_comment() {
    let good = "/* DROP TABLE noted */ CREATE TABLE IF NOT EXISTS moves (ply INTEGER);";
    check_schema_sql(good).expect("block-comment DROP must not trigger destructive detection");
}

#[test]
fn contains_word_does_not_match_substring() {
    // `DROP` が `DROPPED` にマッチしないことの sanity check。
    assert!(!contains_word("CREATE TABLE DROPPED_NOTES (x INT);", "DROP"));
    assert!(contains_word("DROP TABLE x;", "DROP"));
}
