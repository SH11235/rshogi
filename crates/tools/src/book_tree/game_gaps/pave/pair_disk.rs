use super::*;
use crate::tournament_pairs::ResultPairs;

#[derive(Deserialize)]
struct PairMeta {
    pair_index: Option<u32>,
    pair_slot: Option<u32>,
    #[serde(default)]
    attempt: u32,
    #[serde(default)]
    error: Option<bool>,
}

pub(super) fn visit(
    path: &Path,
    name: &str,
    mut visit: impl FnMut(Game, &str) -> Result<()>,
) -> Result<()> {
    let mut pairs = Sorter::new();
    let mut events = Sorter::new();
    let mut seq = 0u32;
    seeds::visit_json(path, |offset, value| {
        if let Some("move" | "result") = value["type"].as_str() {
            let id: u32 = serde_json::from_value(value["game_id"].clone())?;
            let text = serde_json::to_string(&value)?;
            events.push(vec![format!("{id:010}"), format!("{offset:020}")], text.clone())?;
            if value["type"] == "result" {
                let meta: PairMeta = serde_json::from_value(value.clone())?;
                let _: seeds::GameResult = serde_json::from_value(value)?;
                pairs.push(
                    vec![
                        format!("{:010}", meta.pair_index.unwrap_or(seq / 2)),
                        format!("{:010}", meta.attempt),
                        format!("{offset:020}"),
                    ],
                    serde_json::to_string(&(meta.pair_slot.unwrap_or(seq % 2).min(1), text))?,
                )?;
                seq = seq.checked_add(1).context("result の件数が大きすぎます")?;
            }
        }
        Ok(())
    })?;
    let mut pairs = pairs.finish()?;
    let mut selected = Sorter::new();
    let mut current = None;
    let mut pending = ResultPairs::default();
    let mut incomplete = 0;
    let mut in_progress = 0;
    while let Some(row) = read_record(&mut pairs)? {
        let key = (row.key[0].parse::<u32>()?, row.key[1].parse::<u32>()?);
        if current != Some(key) {
            incomplete += pending.incomplete();
            pending = ResultPairs::default();
            current = Some(key);
        }
        let (slot, text): (usize, String) = serde_json::from_str(&row.value)?;
        if let Some(pair) = pending.push(path, key, slot, text) {
            let metas: Vec<PairMeta> = pair
                .iter()
                .map(|s| serde_json::from_str(s))
                .collect::<std::result::Result<_, _>>()?;
            if metas.iter().any(|m| m.error.unwrap_or(false)) {
                continue;
            }
            for text in pair {
                let result: seeds::GameResult = serde_json::from_str(&text)?;
                if result.outcome == "in_progress" {
                    in_progress += 1;
                    continue;
                }
                ensure!(
                    matches!(result.outcome.as_str(), "black_win" | "white_win" | "draw"),
                    "不正な outcome: {}",
                    result.outcome
                );
                selected.push(vec![format!("{:010}", result.game_id)], text)?;
            }
        }
    }
    incomplete += pending.incomplete();
    if incomplete > 0 {
        eprintln!("{}: 未完了の {incomplete} ペアを除外", path.display());
    }
    if in_progress > 0 {
        println!("警告: {}: outcome=in_progress の {in_progress} 局を種から除外", path.display());
    }
    let mut selected = selected.finish()?;
    let mut events = events.finish()?;
    let mut event = read_record(&mut events)?;
    let mut previous = None;
    while let Some(row) = read_record(&mut selected)? {
        let result: seeds::GameResult = serde_json::from_str(&row.value)?;
        ensure!(previous != Some(result.game_id), "game_id が重複しています: {}", result.game_id);
        previous = Some(result.game_id);
        while event.as_ref().is_some_and(|e| e.key[0] < row.key[0]) {
            event = read_record(&mut events)?;
        }
        let mut moves = Vec::<seeds::Move>::new();
        let mut ended = false;
        while event.as_ref().is_some_and(|e| e.key[0] == row.key[0]) {
            let e = event.take().context("棋譜の行がありません")?;
            let value: serde_json::Value = serde_json::from_str(&e.value)?;
            if value["type"] == "result" {
                ended = true;
            } else {
                ensure!(!ended, "result 後に手があります: game_id={}", result.game_id);
                let mv: seeds::Move = serde_json::from_value(value)?;
                if let Some(last) = moves.last() {
                    ensure!(
                        last.ply.checked_add(1) == Some(mv.ply),
                        "手順が不連続: game_id={}",
                        result.game_id
                    );
                }
                moves.push(mv);
            }
            event = read_record(&mut events)?;
        }
        visit(pair_game(name, None, &result, &moves)?, &result.outcome)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disk_pairs_match_shared_reader_across_spills_retries_and_legacy_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("games.jsonl");
        for legacy in [false, true] {
            let mut file = File::create(&path).unwrap();
            for slot in 0..2 {
                for pair in (0..1100).rev() {
                    let mut result =
                        json!({"type":"result", "game_id":pair * 2 + slot, "outcome":"white_win"});
                    if !legacy {
                        result["pair_index"] = json!(pair);
                        result["pair_slot"] = json!(slot);
                        result["error"] = json!(pair % 5 == 0);
                    }
                    writeln!(file, "{result}").unwrap();
                    if !legacy {
                        writeln!(file, "{result}").unwrap();
                        result["attempt"] = json!(1);
                        result["error"] = json!(true);
                        writeln!(file, "{result}").unwrap();
                    }
                }
            }
            drop(file);
            let mut expected = Vec::new();
            visit_pair(&path, "games", None, |game, outcome| {
                expected.push(serde_json::to_string(&(game, outcome))?);
                Ok(())
            })
            .unwrap();
            let mut actual = Vec::new();
            visit(&path, "games", |game, outcome| {
                actual.push(serde_json::to_string(&(game, outcome))?);
                Ok(())
            })
            .unwrap();
            expected.sort();
            actual.sort();
            assert_eq!(actual, expected);
        }
    }
}
