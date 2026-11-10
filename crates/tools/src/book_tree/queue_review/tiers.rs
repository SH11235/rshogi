//! 再採点の後に当てる段の表 (`--rerank-tiers`)。

use super::super::targets::{MAX_GROUP, MAX_PRIORITY};
use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeSet, path::Path};

const COLUMNS: [&str; 6] = [
    "name",
    "match",
    "class",
    "old_priority",
    "priority",
    "group",
];

/// priority の両端を含む範囲。端が無ければその側は制限しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    lo: Option<u32>,
    hi: Option<u32>,
}

impl Range {
    fn parse(text: &str) -> Result<Self> {
        let (lo, hi) = text.split_once("..").context("範囲は `lo..hi` の形が必要です")?;
        let bound = |s: &str| -> Result<Option<u32>> {
            if s.is_empty() {
                return Ok(None);
            }
            let value: u32 = s.parse().with_context(|| format!("priority が不正です: {s}"))?;
            ensure!(value <= MAX_PRIORITY, "priority は {MAX_PRIORITY} 以下が必要です: {value}");
            Ok(Some(value))
        };
        let range = Self {
            lo: bound(lo)?,
            hi: bound(hi)?,
        };
        ensure!(range.lo.is_some() || range.hi.is_some(), "範囲の端が両方ありません: {text}");
        if let (Some(lo), Some(hi)) = (range.lo, range.hi) {
            ensure!(lo <= hi, "範囲の下端が上端を超えています: {text}");
        }
        Ok(range)
    }

    fn contains(self, value: u32) -> bool {
        self.lo.is_none_or(|lo| lo <= value) && self.hi.is_none_or(|hi| value <= hi)
    }

    fn clamp(self, value: u32) -> u32 {
        let value = self.lo.map_or(value, |lo| value.max(lo));
        self.hi.map_or(value, |hi| value.min(hi))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Fixed(u32),
    Clamp(Range),
    /// 受付の今の priority のまま (再採点の値も使わない)。
    Keep,
    /// 再採点の値のまま。
    Rerank,
}

#[derive(Debug)]
struct Tier {
    name: String,
    patterns: Vec<String>,
    classes: Option<BTreeSet<char>>,
    old: Option<Range>,
    action: Action,
    group: Option<String>,
}

/// 段の表の行を上から順に照合し、最初に当たった行を使う。
#[derive(Debug)]
pub(super) struct Tiers(Vec<Tier>);

/// `*` だけを任意の文字列として扱う、両端を固定した照合。
fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<_> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

impl Tiers {
    pub(super) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("段の表を読めません: {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("段の表が不正です: {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        let mut lines = text
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'));
        let (_, header) = lines.next().context("見出しの行がありません")?;
        ensure!(header.split('\t').eq(COLUMNS), "見出しは `{}` が必要です", COLUMNS.join("\\t"));
        let mut tiers = Vec::new();
        let mut names = BTreeSet::new();
        for (n, line) in lines {
            let tier = Self::parse_row(line).with_context(|| format!("{} 行目", n + 1))?;
            ensure!(names.insert(tier.name.clone()), "{} 行目: name が重複しています", n + 1);
            tiers.push(tier);
        }
        ensure!(!tiers.is_empty(), "段の行がありません");
        Ok(Self(tiers))
    }

    fn parse_row(line: &str) -> Result<Tier> {
        let fields: Vec<_> = line.split('\t').collect();
        ensure!(fields.len() == COLUMNS.len(), "欄の数は {} が必要です", COLUMNS.len());
        let name = fields[0].trim();
        ensure!(!name.is_empty(), "name が空です");
        let patterns: Vec<_> = fields[1]
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
        ensure!(!patterns.is_empty(), "match が空です");
        let classes = match fields[2].trim() {
            "*" => None,
            text => {
                let set: BTreeSet<_> = text.chars().collect();
                ensure!(
                    !set.is_empty() && set.iter().all(|c| matches!(c, 'A'..='D')),
                    "class は A〜D の並びか * が必要です: {text}"
                );
                Some(set)
            }
        };
        let old = match fields[3].trim() {
            "*" => None,
            text => Some(Range::parse(text).context("old_priority")?),
        };
        let action = match fields[4].trim() {
            "keep" => Action::Keep,
            "rerank" => Action::Rerank,
            text => {
                if let Some(value) = text.strip_prefix('=') {
                    let value: u32 =
                        value.parse().with_context(|| format!("priority が不正です: {text}"))?;
                    ensure!(
                        value <= MAX_PRIORITY,
                        "priority は {MAX_PRIORITY} 以下が必要です: {value}"
                    );
                    Action::Fixed(value)
                } else if text.contains("..") {
                    Action::Clamp(Range::parse(text).context("priority")?)
                } else {
                    bail!("priority は `=N`・`lo..hi`・keep・rerank のどれかが必要です: {text}");
                }
            }
        };
        let group = match fields[5].trim() {
            "" => None,
            text => {
                ensure!(
                    text.len() <= MAX_GROUP && !text.chars().any(char::is_control),
                    "group は制御文字を含まない 1〜{MAX_GROUP} バイトが必要です: {text:?}"
                );
                Some(text.to_owned())
            }
        };
        Ok(Tier {
            name: name.to_owned(),
            patterns,
            classes,
            old,
            action,
            group,
        })
    }

    /// 最初に当たった段の名前・当てた priority・group を返す。
    /// `old` は受付の今の priority、`rerank` は再採点 (と group 揃え) の後の priority。
    pub(super) fn apply(
        &self,
        id: &str,
        class: char,
        old: u32,
        rerank: u32,
    ) -> Option<(&str, u32, Option<&str>)> {
        let tier = self.0.iter().find(|t| {
            t.patterns.iter().any(|p| glob(p, id))
                && t.classes.as_ref().is_none_or(|c| c.contains(&class))
                && t.old.is_none_or(|r| r.contains(old))
        })?;
        let priority = match tier.action {
            Action::Fixed(value) => value,
            Action::Clamp(range) => range.clamp(rerank),
            Action::Keep => old,
            Action::Rerank => rerank,
        };
        Some((&tier.name, priority, tier.group.as_deref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_anchors_both_ends() {
        for (pattern, text, expected) in [
            ("x58i8*", "x58i8_3_ab", true),
            ("x58i8*", "ax58i8", false),
            ("*bridge*", "x51_bridge_w", true),
            ("*bridge*", "x51_brid", false),
            ("*", "", true),
            ("x57kf", "x57kf", true),
            ("x57kf", "x57kf_1", false),
            ("a*b*c", "abc", true),
            ("a*b*c", "a_c_b", false),
            ("ab*ba", "aba", false),
        ] {
            assert_eq!(glob(pattern, text), expected, "{pattern} {text}");
        }
    }

    #[test]
    fn first_matching_row_and_actions() {
        let tiers = Tiers::parse(
            "# comment\nname\tmatch\tclass\told_priority\tpriority\tgroup\n\
             keep\tx23top*\t*\t*\tkeep\t\n\
             prop\t*refut*,x49br*\t*\t..8\t=2\tprop\n\
             seed\tx58i8*\tABC\t*\t=3\t\n\
             seed-d\tx58i8*\tD\t*\t13..60\t\n\
             small\t*\t*\t..8\t..4\t\n\
             rest\t*\t*\t*\trerank\t\n",
        )
        .unwrap();
        assert_eq!(tiers.apply("x23top_1", 'A', 2, 40), Some(("keep", 2, None)));
        assert_eq!(tiers.apply("x49br_1", 'D', 8, 8), Some(("prop", 2, Some("prop"))));
        assert_eq!(tiers.apply("x5_refut_w", 'D', 9, 9), Some(("rest", 9, None)));
        assert_eq!(tiers.apply("x58i8_1", 'B', 50, 40), Some(("seed", 3, None)));
        assert_eq!(tiers.apply("x58i8_1", 'D', 4, 4), Some(("seed-d", 13, None)));
        assert_eq!(tiers.apply("x58i8_1", 'D', 100, 70), Some(("seed-d", 60, None)));
        assert_eq!(tiers.apply("x1", 'A', 8, 7), Some(("small", 4, None)));
        let narrow = Tiers::parse(
            "name\tmatch\tclass\told_priority\tpriority\tgroup\nx\tx*\tA\t*\tkeep\t\n",
        )
        .unwrap();
        assert_eq!(narrow.apply("x1", 'B', 1, 1), None);
        assert_eq!(narrow.apply("y1", 'A', 1, 1), None);
    }

    #[test]
    fn rejects_malformed_rows() {
        let header = "name\tmatch\tclass\told_priority\tpriority\tgroup\n";
        for row in [
            "a\tx*\t*\t*\t5\t",
            "a\tx*\tE\t*\t=5\t",
            "a\tx*\t*\t9..8\t=5\t",
            "a\tx*\t*\t..\t=5\t",
            "a\t\t*\t*\t=5\t",
            "a\tx*\t*\t*\t=5",
            "a\tx*\t*\t*\t=1000001\t",
            "a\tx*\t*\t*\t=1\t\na\ty*\t*\t*\t=1\t",
        ] {
            assert!(Tiers::parse(&format!("{header}{row}\n")).is_err(), "{row}");
        }
        assert!(Tiers::parse("name\tmatch\n").is_err());
        assert!(Tiers::parse(header).is_err());
    }
}
