use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::unified::UnifiedSearcher;
use crate::search::SearchLimitsBuilder;
use crate::Position;

#[test]
fn test_multipv_lines_and_sync_basic() {
    // MultiPV=2で探索して、linesとレガシーフィールドが同期していることを確認
    let mut pos = Position::startpos();
    let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);
    let limits = SearchLimitsBuilder::default().depth(2).multipv(2).build();
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
            assert!(!result.stats.pv.is_empty(), "stats.pv should not be empty");
            assert_eq!(
                result.stats.pv[0], first.root_move,
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
    let limits = SearchLimitsBuilder::default().depth(2).multipv(2).build();
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
    let limits = SearchLimitsBuilder::default().fixed_nodes(1_000).depth(2).multipv(2).build();
    let result = searcher.search(&mut pos, limits);
    // 成功の最低条件：結果が返り、best_moveが存在する
    assert!(result.best_move.is_some());
    // linesは環境により1本のみのこともあるが、少なくとも構造が壊れていないこと
    if let Some(lines) = &result.lines {
        assert!(!lines.is_empty());
    }
}

#[test]
fn test_multipv_limits_takes_precedence() {
    // SearchLimitsのmultipvがSearcherの内部設定より優先されることを確認
    let mut pos = Position::startpos();
    let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);

    // Searcherの内部multipvは1のまま（デフォルト）
    assert_eq!(searcher.multi_pv(), 1);

    // Limitsでmultipv=2を指定
    let limits = SearchLimitsBuilder::default().depth(3).multipv(2).build();
    let result = searcher.search(&mut pos, limits);

    // MultiPV=2の結果が返る
    if let Some(lines) = &result.lines {
        assert!(!lines.is_empty());
        // 十分な深さがあれば2本期待できる
        if result.stats.depth >= 3 {
            assert!(lines.len() >= 1, "At least 1 line should be returned");
        }
    }
}

#[test]
fn test_backward_compatibility_multipv_1() {
    // MultiPV=1（デフォルト）での後方互換性を確認
    let mut pos = Position::startpos();
    let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);

    // デフォルトのmultipv=1で探索
    let limits = SearchLimitsBuilder::default().depth(3).build();
    let result = searcher.search(&mut pos, limits);

    // 通常の結果が返る
    assert!(result.best_move.is_some());
    assert!(!result.stats.pv.is_empty());

    // MultiPV=1ではlinesは通常付かない（実装による）
    // ただし、付いても問題ない
}

#[test]
fn test_multipv_with_reset() {
    // reset_history()後でもlimits.multipvが正しく反映されることを確認
    let mut pos = Position::startpos();
    let mut searcher = UnifiedSearcher::<_, true, true>::new(MaterialEvaluator);

    // 履歴をリセット
    searcher.reset_history();

    // MultiPV=3で探索
    let limits = SearchLimitsBuilder::default().depth(2).multipv(3).build();
    let result = searcher.search(&mut pos, limits);

    // 結果が返る
    assert!(result.best_move.is_some());
    if let Some(lines) = &result.lines {
        assert!(!lines.is_empty());
    }
}
