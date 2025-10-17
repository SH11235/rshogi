use crate::common::sfen::normalize_4t;

/// Mirror a Shogi SFEN horizontally (left-right).
/// Keeps side-to-move, hands, and ply intact.
pub fn mirror_horizontal(sfen: &str) -> Option<String> {
    let mut it = sfen.split_whitespace();
    let board = it.next()?;
    let stm = it.next()?;
    let hands = it.next()?;
    let ply = it.next()?;
    let rows: Vec<&str> = board.split('/').collect();
    if rows.len() != 9 {
        return None;
    }
    let mut out_rows = Vec::with_capacity(9);
    for r in rows {
        // expand row to 9 cells
        let mut cells: Vec<String> = Vec::with_capacity(9);
        let mut chars = r.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_ascii_digit() {
                let n = c.to_digit(10)? as usize;
                for _ in 0..n {
                    cells.push(String::from("1"));
                }
            } else if c == '+' {
                // promoted piece like +P, +r
                if let Some(pc) = chars.next() {
                    cells.push(format!("+{}", pc));
                } else {
                    return None;
                }
            } else {
                cells.push(c.to_string());
            }
        }
        if cells.len() != 9 {
            return None;
        }
        cells.reverse();
        // compress back
        let mut row_out = String::new();
        let mut run = 0usize;
        for s in cells {
            if s == "1" {
                run += 1;
            } else {
                if run > 0 {
                    row_out.push_str(&run.to_string());
                    run = 0;
                }
                row_out.push_str(&s);
            }
        }
        if run > 0 {
            row_out.push_str(&run.to_string());
        }
        out_rows.push(row_out);
    }
    Some(format!("{} {} {} {}", out_rows.join("/"), stm, hands, ply))
}

/// Canonicalize SFEN considering horizontal mirror symmetry.
/// Returns min(original, mirror) after 4-token normalization.
pub fn canonicalize_4t_with_mirror(sfen: &str) -> Option<String> {
    let a = normalize_4t(sfen)?;
    let b = mirror_horizontal(&a)?;
    Some(if a <= b { a } else { b })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mirror_initial_is_same() {
        // 初期局面は横対称ではない（角と飛の位置が非対称）ため、
        // 水平反転すると別の局面になるはず。
        let s = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let m = mirror_horizontal(s).unwrap();
        assert_ne!(normalize_4t(s), normalize_4t(&m));
    }

    #[test]
    fn test_mirror_rook_bishop_swapped() {
        // Place R and B asymmetrically and confirm mirror swaps them within a rank
        let s = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/2B4R1/LNSGKGSNL b - 1";
        let m = mirror_horizontal(s).unwrap();
        assert!(m.starts_with("lnsgkgsnl/1b5r1"));
    }
}
