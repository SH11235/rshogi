//! Common test utilities for search modules

#[cfg(test)]
pub mod test_helpers {
    use crate::movegen::generator::MoveGenImpl;
    use crate::shogi::{Move, Position};
    use crate::usi::move_to_usi;

    /// Helper function to get a legal move from USI notation
    /// This ensures the move has proper piece type information from the move generator
    pub fn legal_usi(pos: &Position, usi: &str) -> Move {
        let mut gen = MoveGenImpl::new(pos);
        let moves = gen.generate_all();
        *moves
            .as_slice()
            .iter()
            .find(|m| move_to_usi(m) == usi)
            .unwrap_or_else(|| panic!("USI move {} is not legal in the position", usi))
    }
}
