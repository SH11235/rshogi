//! 探索内の入玉宣言勝ち判定（YO の search() と同じく probe_transposition で行う）。
use std::sync::Arc;

use crate::eval::EvalHash;
use crate::movegen::{MoveList, generate_legal_all_with_pass};
use crate::position::Position;
use crate::search::alpha_beta::{ProbeOutcome, SearchContext, SearchWorker};
use crate::search::types::NodeType;
use crate::search::{LimitsType, Search, SearchInfo, SearchTuneParams};
use crate::tt::TranspositionTable;
use crate::types::{EnteringKingRule, Move, Value};

/// 先手が今すぐ 27 点法で宣言できる局面。
const DECLARABLE: &str = "KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1";

/// 先手玉がトライ升 5一 に隣接し、5一 が空で敵の利きもない局面。
const TRY_READY: &str = "3K5/9/9/9/9/9/9/9/4k4 b 2r2b4g4s4n4l18p 1";

fn on_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

fn position(sfen: &str) -> Position {
    let mut pos = Position::new();
    pos.set_sfen(sfen).unwrap();
    pos
}

/// 空の置換表で probe_transposition を 1 回呼び、結果と置換表への書き込み有無を返す。
fn probe<const NT: u8>(
    sfen: &str,
    rule: EnteringKingRule,
    ply: i32,
    pv_node: bool,
    excluded_move: Move,
) -> (Option<Value>, bool) {
    let tt = Arc::new(TranspositionTable::new(1));
    let mut worker = SearchWorker::new(
        Arc::clone(&tt),
        Arc::new(EvalHash::new(1)),
        0,
        0,
        SearchTuneParams::default(),
    );
    worker.entering_king_rule = rule;
    worker.prepare_search(&LimitsType {
        depth: 1,
        ..Default::default()
    });
    let mut pos = position(sfen);
    let ctx = SearchContext {
        tt: &worker.tt,
        eval_hash: &worker.eval_hash,
        history: &worker.history,
        cont_history_sentinel: worker.cont_history_sentinel,
        generate_all_legal_moves: worker.generate_all_legal_moves,
        max_moves_to_draw: worker.max_moves_to_draw,
        thread_id: worker.thread_id,
        allow_tt_write: worker.allow_tt_write,
        tune_params: &worker.search_tune_params,
        reductions: &worker.reductions,
        draw_value_table: worker.draw_value_table,
        entering_king_rule: worker.entering_king_rule,
    };
    let in_check = pos.in_check();
    let outcome = super::super::eval_helpers::probe_transposition::<NT>(
        &mut worker.state,
        &ctx,
        &mut pos,
        4,
        Value::ZERO,
        ply,
        pv_node,
        in_check,
        excluded_move,
        !pv_node,
    );
    let value = match outcome {
        ProbeOutcome::Cutoff { value, tt_move, .. } => {
            // 宣言勝ちカットオフは ttMove を返さない（ヒストリ更新を起こさない）。
            assert_eq!(tt_move, Move::NONE);
            Some(value)
        }
        ProbeOutcome::Continue(_) => None,
    };
    (value, tt.probe(pos.key(), &pos).found)
}

#[test]
fn in_tree_declaration_cuts_off_without_tt_write() {
    on_large_stack(|| {
        for (pv_node, ply) in [(false, 3), (true, 6)] {
            let (value, written) = if pv_node {
                probe::<{ NodeType::PV as u8 }>(
                    DECLARABLE,
                    EnteringKingRule::Point27,
                    ply,
                    true,
                    Move::NONE,
                )
            } else {
                probe::<{ NodeType::NonPV as u8 }>(
                    DECLARABLE,
                    EnteringKingRule::Point27,
                    ply,
                    false,
                    Move::NONE,
                )
            };
            assert_eq!(value, Some(Value::mate_in(ply + 1)));
            // Move::WIN は置換表に書き出さない（YO準拠）。
            assert!(!written);
        }
    });
}

#[test]
fn in_tree_declaration_disabled_by_rule_and_root() {
    on_large_stack(|| {
        // ルールなしでは従来どおり探索を続ける。
        let (value, _) = probe::<{ NodeType::NonPV as u8 }>(
            DECLARABLE,
            EnteringKingRule::None,
            3,
            false,
            Move::NONE,
        );
        assert_eq!(value, None);
        // root は engine.rs 側で root_moves として扱うため、ここでは判定しない。
        let (value, _) = probe::<{ NodeType::Root as u8 }>(
            DECLARABLE,
            EnteringKingRule::Point27,
            0,
            true,
            Move::NONE,
        );
        assert_eq!(value, None);
    });
}

#[test]
fn in_tree_declaration_respects_excluded_move() {
    on_large_stack(|| {
        // 点数法の宣言 (Move::WIN) は ttMove になりえないので、除外手があっても詰みを返す。
        let unrelated = position(DECLARABLE).to_move(Move::from_usi("4c4b").unwrap()).unwrap();
        let (value, _) = probe::<{ NodeType::NonPV as u8 }>(
            DECLARABLE,
            EnteringKingRule::Point27,
            2,
            false,
            unrelated,
        );
        assert_eq!(value, Some(Value::mate_in(3)));

        // トライルールの宣言手は通常の玉移動なので、それ自体が除外手なら採用しない。
        let try_move = position(TRY_READY).declaration_win(EnteringKingRule::TryRule);
        assert!(try_move.is_normal());
        let (value, _) = probe::<{ NodeType::NonPV as u8 }>(
            TRY_READY,
            EnteringKingRule::TryRule,
            2,
            false,
            Move::NONE,
        );
        assert_eq!(value, Some(Value::mate_in(3)));
        let (value, _) = probe::<{ NodeType::NonPV as u8 }>(
            TRY_READY,
            EnteringKingRule::TryRule,
            2,
            false,
            try_move,
        );
        assert_eq!(value, None);
    });
}

fn search(
    sfen: &str,
    rule: EnteringKingRule,
    depth: i32,
) -> (Position, crate::search::SearchResult) {
    let mut root = position(sfen);
    let mut search = Search::new(16);
    search.set_entering_king_rule(rule);
    let result = search.go(
        &mut root,
        LimitsType {
            depth,
            ..Default::default()
        },
        None::<fn(&SearchInfo)>,
    );
    (root, result)
}

/// 公開 PV が全て合法手で構成され、ponder も合法であることを確認する。
fn assert_pv_legal(root: &Position, result: &crate::search::SearchResult) {
    assert!(!result.pv.is_empty());
    assert!(!result.pv.contains(&Move::WIN));
    let mut pos = root.clone();
    for &mv in &result.pv {
        let mut legal = MoveList::new();
        generate_legal_all_with_pass(&pos, &mut legal);
        assert!(legal.as_slice().contains(&mv), "illegal pv move {}", mv.to_usi());
        let check = pos.gives_check(mv);
        pos.do_move(mv, check);
    }
    if result.ponder_move != Move::NONE {
        assert_eq!(result.ponder_move, result.pv[1]);
    }
}

#[test]
fn in_tree_declaration_sees_opponent_declaration_after_our_move() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        // 相手（後手）が入玉済みで、先手がどう指しても後手が宣言可能になる局面。
        // 先手は今は宣言できない（17 点）。
        let sfen = "LN2K4/LN1GG1SS1/1PPPPPP2/9/9/9/3pppppp/7ss/6ggk b 3P2r2b 1";
        let root = position(sfen);
        assert_eq!(root.declaration_win(EnteringKingRule::Point27), Move::NONE);

        let (root, result) = search(sfen, EnteringKingRule::Point27, 5);
        assert!(
            result.score.is_loss(),
            "相手の宣言勝ちが見えていない: score={}",
            result.score.raw()
        );
        assert_pv_legal(&root, &result);

        // ルールなしでは宣言勝ちが存在しないので、負けとは評価しない。
        let (_, result) = search(sfen, EnteringKingRule::None, 5);
        assert!(!result.score.is_loss(), "score={}", result.score.raw());
    });
}

#[test]
fn in_tree_declaration_finds_own_declaration_in_two() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        // 先手は敵陣の駒が 9 枚で今は宣言できないが、大駒を敵陣に打つと 10 枚 29 点になり、
        // 後手のどの応手の後でも宣言できる。
        // 先手の盤上に歩を置かないのは、静止探索が歩の成りを bestMove として置換表に残すと
        // 非 PV ノードでは宣言判定が省かれる（YO と同じ `!ttMove || PvNode` 条件）ため。
        let sfen = "KGG6/+S+S7/+P+P+P+P+P4/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1";
        let root = position(sfen);
        assert_eq!(root.declaration_win(EnteringKingRule::Point27), Move::NONE);

        let (root, result) = search(sfen, EnteringKingRule::Point27, 5);
        assert!(
            result.score.is_win(),
            "2 手後の宣言勝ちが見えていない: score={}",
            result.score.raw()
        );
        // 宣言勝ちは 3 手目（ply 2）で成立するので、それより遠い詰みにはならない。
        assert!(result.score >= Value::mate_in(3), "score={}", result.score.raw());
        assert_pv_legal(&root, &result);
    });
}
