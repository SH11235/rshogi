//! Regression test suite with fixed positions and expected node counts
//!
//! This suite helps detect performance regressions by comparing node counts
//! at fixed depths for a set of standard positions.

use crate::{
    evaluation::evaluate::MaterialEvaluator,
    search::{unified::UnifiedSearcher, SearchLimits},
    shogi::Position,
    usi::parse_sfen,
};

/// Position for regression testing
#[derive(Debug)]
struct TestPosition {
    name: &'static str,
    sfen: &'static str,
    depth: u8,
    // Expected node count with ±5% tolerance
    expected_nodes: u64,
}

const REGRESSION_POSITIONS: &[TestPosition] = &[
    TestPosition {
        name: "Initial position",
        sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        depth: 3,
        expected_nodes: 0, // To be filled after baseline run
    },
    TestPosition {
        name: "Mid-game position",
        sfen: "ln1g1g1nl/1r1s1k3/1pp1ppp1p/p2p3p1/9/P1P1P3P/1P1PSP1P1/1BK1G2R1/LN1G3NL b BSP 1",
        depth: 3,
        expected_nodes: 0,
    },
    TestPosition {
        name: "Endgame position",
        sfen: "8l/4g1k2/4ppn2/8p/9/8P/4PP3/4GK3/5G2L b RBSNrbsnl3p 1",
        depth: 3,
        expected_nodes: 0,
    },
    TestPosition {
        name: "King in check",
        sfen: "lnsgkg1nl/6r2/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1",
        depth: 3,
        expected_nodes: 0,
    },
    TestPosition {
        name: "Many captures available",
        sfen: "ln1gkg1nl/1r1s3b1/pppppp1pp/6p2/9/2P4P1/PP1PPPP1P/1BS5R/LN1GKG1NL b P 1",
        depth: 3,
        expected_nodes: 0,
    },
];

/// Run regression suite and return results
pub fn run_regression_suite() -> Vec<(String, u64)> {
    let mut results = Vec::new();

    for pos in REGRESSION_POSITIONS {
        let position = match parse_sfen(pos.sfen) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to parse position '{}': {}", pos.name, e);
                continue;
            }
        };

        let nodes = measure_nodes(position, pos.depth);
        results.push((pos.name.to_string(), nodes));
    }

    results
}

/// Measure node count for a position at given depth
fn measure_nodes(mut position: Position, depth: u8) -> u64 {
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
    let limits = SearchLimits::builder().depth(depth).build();

    // Run search
    let result = searcher.search(&mut position, limits);
    result.stats.nodes
}

/// Print baseline results for updating the test
pub fn print_baseline() {
    println!("=== Regression Suite Baseline ===");
    println!("Update REGRESSION_POSITIONS with these values:");
    println!();

    for (i, pos) in REGRESSION_POSITIONS.iter().enumerate() {
        let position = parse_sfen(pos.sfen).unwrap();
        let nodes = measure_nodes(position, pos.depth);

        println!("    TestPosition {{");
        println!("        name: \"{}\",", pos.name);
        println!("        sfen: \"{}\",", pos.sfen);
        println!("        depth: {},", pos.depth);
        println!("        expected_nodes: {},", nodes);
        println!(
            "    }}{}",
            if i < REGRESSION_POSITIONS.len() - 1 {
                ","
            } else {
                ""
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_regression_suite() {
        let results = run_regression_suite();

        for (name, nodes) in results {
            println!("{}: {} nodes", name, nodes);

            // Find corresponding expected value
            if let Some(pos) = REGRESSION_POSITIONS.iter().find(|p| p.name == name) {
                if pos.expected_nodes > 0 {
                    // Allow ±5% deviation
                    let min_nodes = (pos.expected_nodes as f64 * 0.95) as u64;
                    let max_nodes = (pos.expected_nodes as f64 * 1.05) as u64;

                    assert!(
                        nodes >= min_nodes && nodes <= max_nodes,
                        "{}: Node count {} is outside expected range [{}, {}]",
                        name,
                        nodes,
                        min_nodes,
                        max_nodes
                    );
                }
            }
        }
    }

    #[test]
    #[ignore] // Run with --ignored to generate baseline
    fn generate_baseline() {
        print_baseline();
    }
}
