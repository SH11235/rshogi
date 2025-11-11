use engine_core::search::types::RootLine;
use engine_core::shogi::Position;
use engine_core::usi::{append_usi_score_and_bound, move_to_usi, score_view_from_internal};
use smallvec::SmallVec;

/// Emit a USI-compliant info line for a PV update (legacy bridge).
///
/// This adapter now consumes the richer `RootLine` produced by the core, so we
/// can surface MultiPV indices and optional timing/node metrics without
/// fabricating placeholder values.
pub fn emit_pv_line(line: &RootLine, multipv_enabled: bool) {
    let mut out = format!("info depth {}", line.depth);

    if let Some(ms) = line.time_ms {
        out.push_str(&format!(" time {}", ms));
    }

    if let Some(nodes) = line.nodes {
        out.push_str(&format!(" nodes {}", nodes));
        let nps = line
            .time_ms
            .filter(|ms| *ms > 0)
            .map(|ms| nodes.saturating_mul(1000) / ms.max(1))
            .unwrap_or(0);
        out.push_str(&format!(" nps {}", nps));
    }

    if multipv_enabled {
        out.push_str(&format!(" multipv {}", line.multipv_index.max(1)));
    }

    if let Some(sel) = line.seldepth {
        out.push_str(&format!(" seldepth {}", sel));
    }

    let score_view = score_view_from_internal(line.score_internal);
    append_usi_score_and_bound(&mut out, score_view, line.bound);

    if !line.pv.is_empty() {
        out.push_str(" pv ");
        out.push_str(&line.pv.iter().map(move_to_usi).collect::<Vec<_>>().join(" "));
    }

    println!("{}", out);
}

/// Sanitize PV against the given root position by applying moves sequentially.
///
/// - 不正な手（`is_legal_move == false`）に遭遇した時点で以降を切り捨てる。
/// - 先頭手が不正かつ `line.root_move` が合法な場合は、`root_move` のみを残す。
/// - 既に空PVなら、`root_move` が合法であれば 1 手だけ出力する。
pub fn sanitize_line_for_root(line: &RootLine, root_pos: &Position) -> RootLine {
    let mut out = line.clone();
    let mut sanitized: SmallVec<[engine_core::shogi::Move; 32]> = SmallVec::new();
    let mut pos = root_pos.clone();

    if out.pv.is_empty() {
        if pos.is_legal_move(out.root_move) {
            sanitized.push(out.root_move);
        }
        out.pv = sanitized;
        return out;
    }

    // First move must be legal from root; if not, try root_move as a minimal fallback
    if !pos.is_legal_move(out.pv[0]) {
        if pos.is_legal_move(out.root_move) {
            sanitized.push(out.root_move);
        }
        out.pv = sanitized;
        return out;
    }

    for &mv in out.pv.iter() {
        if !pos.is_legal_move(mv) {
            break;
        }
        let _undo = pos.do_move(mv);
        sanitized.push(mv);
    }

    out.pv = sanitized;
    out
}

/// Emit a PV info line after sanitizing the PV against `root_pos`.
pub fn emit_pv_line_sanitized(line: &RootLine, root_pos: &Position, multipv_enabled: bool) {
    let fixed = sanitize_line_for_root(line, root_pos);
    emit_pv_line(&fixed, multipv_enabled);
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::usi::{create_position, parse_usi_move};

    #[test]
    fn sanitize_trims_illegal_tail() {
        // startpos; PV: 7g7f, (illegal) 7g7f
        let pos = create_position(true, None, &[]).expect("startpos");
        let m1 = parse_usi_move("7g7f").expect("parse m1");

        let line = RootLine {
            multipv_index: 1,
            root_move: m1,
            score_internal: 0,
            score_cp: 0,
            bound: engine_core::search::types::NodeType::Exact,
            depth: 8,
            seldepth: Some(8),
            pv: {
                let mut v: SmallVec<[engine_core::shogi::Move; 32]> = SmallVec::new();
                v.push(m1);
                v.push(m1); // same move again -> illegal from child
                v
            },
            nodes: Some(1000),
            time_ms: Some(10),
            nps: Some(100000),
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        };

        let fixed = sanitize_line_for_root(&line, &pos);
        assert_eq!(fixed.pv.len(), 1, "illegal tail must be trimmed");
        assert_eq!(fixed.pv[0], m1);
    }
}
