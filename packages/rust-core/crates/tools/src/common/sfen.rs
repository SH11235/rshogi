/// Normalize a Shogi SFEN string to the first 4 tokens:
/// board, side-to-move, hands, move count.
/// Returns None if SFEN is malformed.
pub fn normalize_4t(sfen: &str) -> Option<String> {
    let mut it = sfen.split_whitespace();
    let b = it.next()?;
    let s = it.next()?;
    let h = it.next()?;
    let m = it.next()?;
    Some(format!("{} {} {} {}", b, s, h, m))
}
