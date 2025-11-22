use crate::movegen::{error::MoveGenError, MoveGenerator};
use crate::search::SearchLimits;
use crate::shogi::Move;
use crate::Position;
use std::collections::HashSet;

/// Build root move list based on SearchLimits (searchmoves / generate_all_legal_moves).
///
/// - `limits.root_moves` が既に設定されている場合はそれを優先して返す。
/// - `searchmoves` が指定されている場合は、その順序で合法手にフィルタし、空になったら全合法手でフォールバック。
/// - `searchmoves` 未指定の場合は全合法手を返す。
pub fn build_root_moves(pos: &Position, limits: &SearchLimits) -> Result<Vec<Move>, MoveGenError> {
    if let Some(prebuilt) = limits.root_moves.as_ref() {
        if !prebuilt.is_empty() {
            return Ok((**prebuilt).clone());
        }
        // 明示的に空が渡された場合はフォールバックとして合法手を再生成する
    }

    let generator = MoveGenerator::new();

    // LEGAL_ALL 相当の分岐は現状同一の実装。将来的に pseudo-legal 網羅が必要ならここで分ける。
    let all_moves = generator.generate_all_with_mode(pos, limits.generate_all_legal_moves)?;
    let legal = all_moves.as_slice();

    if let Some(searchmoves) = limits.searchmoves.as_ref() {
        let mut seen = HashSet::new();
        let mut filtered = Vec::with_capacity(legal.len());
        for &mv in searchmoves.iter() {
            if legal.contains(&mv) && seen.insert(mv) {
                filtered.push(mv);
            }
        }
        if filtered.is_empty() {
            return Ok(legal.to_vec());
        }
        return Ok(filtered);
    }

    Ok(legal.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchLimitsBuilder;
    use crate::usi::{move_to_usi, parse_usi_move};
    use std::sync::Arc;

    #[test]
    fn respects_searchmoves_order_and_filters_illegal() {
        let pos = Position::startpos();
        let legal = MoveGenerator::new().generate_all(&pos).expect("legal moves");
        let pick = |usi: &str| -> Move {
            legal
                .as_slice()
                .iter()
                .copied()
                .find(|m| move_to_usi(m) == usi)
                .unwrap_or_else(|| panic!("legal move not found for usi={}", usi))
        };
        let mv1 = pick("7g7f");
        let mv2 = pick("2g2f");
        let limits = SearchLimitsBuilder::default().searchmoves(vec![mv1, mv2]).build();

        let root_moves = build_root_moves(&pos, &limits).expect("build root moves");

        assert_eq!(root_moves, vec![mv1, mv2]);
    }

    #[test]
    fn falls_back_to_all_legal_when_searchmoves_miss() {
        let pos = Position::startpos();
        let invalid = parse_usi_move("7a7b").unwrap(); // 先手番で合法ではない
        let limits = SearchLimitsBuilder::default().searchmoves(vec![invalid]).build();

        let expected = MoveGenerator::new()
            .generate_all(&pos)
            .expect("legal moves")
            .as_slice()
            .to_vec();
        let root_moves = build_root_moves(&pos, &limits).expect("build root moves");

        assert_eq!(root_moves, expected);
    }

    #[test]
    fn uses_prebuilt_root_moves_when_provided() {
        let pos = Position::startpos();
        let mv = parse_usi_move("7g7f").unwrap();
        let prebuilt = Arc::new(vec![mv]);
        let limits = SearchLimitsBuilder::default().root_moves(Arc::clone(&prebuilt)).build();

        let root_moves = build_root_moves(&pos, &limits).expect("build root moves");
        assert_eq!(root_moves, vec![mv]);
    }

    #[test]
    fn generate_all_legal_moves_flag_routes_through_builder() {
        // フラグを true にしても合法手列挙が取得できることを確認（擬似合法未実装だが経路確認用）。
        let pos = Position::startpos();
        let limits = SearchLimitsBuilder::default().generate_all_legal_moves(true).build();

        let via_flag = build_root_moves(&pos, &limits).expect("build with flag");
        let via_default = MoveGenerator::new().generate_all(&pos).expect("legal moves");

        assert_eq!(via_flag, via_default.as_slice());
    }
}
