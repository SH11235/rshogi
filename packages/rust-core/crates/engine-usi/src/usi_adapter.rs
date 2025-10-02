use engine_core::search::types::RootLine;
use engine_core::usi::{append_usi_score_and_bound, move_to_usi, score_view_from_internal};

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
        if let Some(nps) = line.nps {
            out.push_str(&format!(" nps {}", nps));
        } else if let Some(ms) = line.time_ms {
            let denom = ms.max(1);
            let nps = nodes.saturating_mul(1000) / denom;
            out.push_str(&format!(" nps {}", nps));
        }
    } else if let Some(nps) = line.nps {
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
