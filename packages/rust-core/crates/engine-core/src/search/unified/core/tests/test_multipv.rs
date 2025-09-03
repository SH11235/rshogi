use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::unified::UnifiedSearcher;
use crate::search::SearchLimitsBuilder;
use crate::Position;

#[test]
fn test_multipv_lines_and_sync_basic() {
    // MultiPV=2で探索して、linesとレガシーフィールドが同期していることを確認
    let mut pos = Position::startpos();
    let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);
    searcher.set_multi_pv(2);

    let limits = SearchLimitsBuilder::default().depth(2).build();
    let result = searcher.search(&mut pos, limits);

    // linesが付与され、少なくとも1本は存在
    if let Some(lines) = &result.lines {
        assert!(!lines.is_empty(), "lines should not be empty when MultiPV>1");
        // best_moveとlines[0]のroot_moveが一致
        if let Some(first) = lines.first() {
            assert_eq!(
                result.best_move,
                Some(first.root_move),
                "best_move should equal lines[0].root_move"
            );
            // pv同期（少なくとも先頭手は一致）
            assert!(
                !result.stats.pv.is_empty(),
                "stats.pv should not be empty"
            );
            assert_eq!(
                result.stats.pv[0],
                first.root_move,
                "stats.pv[0] should equal lines[0].root_move"
            );
        }
    } else {
        // 環境依存でlinesが付かないケースは失敗とする（MultiPV=2をセットしているため）
        panic!("result.lines should be Some when MultiPV=2");
    }
}

#[test]
fn test_multipv_ordering_nonincreasing_scores() {
    // MultiPV=2で探索し、lines[0].score >= lines[1].score を緩く検証（降順）
    let mut pos = Position::startpos();
    let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);
    searcher.set_multi_pv(2);

    let limits = SearchLimitsBuilder::default().depth(2).build();
    let result = searcher.search(&mut pos, limits);
    if let Some(lines) = &result.lines {
        if lines.len() >= 2 {
            assert!(
                lines[0].score_cp >= lines[1].score_cp,
                "lines should be ordered by non-increasing score"
            );
        }
    }
}

#[test]
fn test_budget_guard_does_not_panic() {
    // ごく小さいノード予算でもMultiPV=2が返ること（パニックしないこと）を確認
    let mut pos = Position::startpos();
    let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);
    searcher.set_multi_pv(2);

    let limits = SearchLimitsBuilder::default()
        .fixed_nodes(1_000)
        .depth(2)
        .build();
    let result = searcher.search(&mut pos, limits);
    // 成功の最低条件：結果が返り、best_moveが存在する
    assert!(result.best_move.is_some());
    // linesは環境により1本のみのこともあるが、少なくとも構造が壊れていないこと
    if let Some(lines) = &result.lines {
        assert!(!lines.is_empty());
    }
}

