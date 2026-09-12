//! 公開 PV の合法な接頭列と、探索状態からの分離。
use super::*;

fn startpos() -> Position {
    let mut pos = Position::new();
    pos.set_hirate();
    pos
}

fn line(root: &Position, moves: &[&str]) -> Vec<Move> {
    let mut pos = root.clone();
    moves
        .iter()
        .map(|usi| {
            let mut legal = MoveList::new();
            generate_legal_all_with_pass(&pos, &mut legal);
            let mv = *legal.as_slice().iter().find(|m| m.to_usi() == *usi).unwrap();
            let check = pos.gives_check(mv);
            pos.do_move(mv, check);
            mv
        })
        .collect()
}

#[test]
fn public_pv_preserves_legal_nonpromotion() {
    let mut root = Position::new();
    root.set_sfen("4k4/9/9/4P4/9/9/9/9/4K4 b - 1").unwrap();
    let mv = root.to_move(Move::from_usi("5d5c").unwrap()).unwrap();
    assert!(root.pseudo_legal(mv));
    assert!(root.is_legal(mv));
    assert_eq!(legal_pv_prefix_len(&root, &[mv], EnteringKingRule::None), 1);
}

#[test]
fn public_pv_truncates_stale_and_malformed_moves() {
    let root = startpos();
    let original_sfen = root.to_sfen();
    let original_key = root.key();
    let mut pv = line(&root, &["7g7f", "2c2d", "1g1f", "1a1b", "2g2f"]);
    assert_eq!(legal_pv_prefix_len(&root, &pv, EnteringKingRule::None), 5);
    // 移動済みの香を再び動かす列。最初の不正手より前が保持されることを確認する。
    pv.push(pv[3]);
    let original_pv = pv.clone();
    assert_eq!(legal_pv_prefix_len(&root, &pv, EnteringKingRule::None), 5);
    assert_eq!(root.to_sfen(), original_sfen);
    assert_eq!(root.key(), original_key);
    assert_eq!(pv, original_pv);
    for invalid in [
        Move::NONE,
        Move::WIN,
        Move::PASS,
        Move::from_usi("3a2b").unwrap(),
    ] {
        assert_eq!(legal_pv_prefix_len(&root, &[invalid], EnteringKingRule::None), 0);
        assert_eq!(legal_pv_prefix_len(&root, &[pv[0], invalid], EnteringKingRule::None), 1);
    }
    assert_eq!(legal_pv_prefix_len(&root, &[], EnteringKingRule::None), 0);
}

#[test]
fn public_pv_preserves_declaration_and_stops_after_it() {
    let mut root = Position::new();
    root.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
        .unwrap();
    assert_eq!(root.declaration_win(EnteringKingRule::Point27), Move::WIN);
    assert_eq!(
        legal_pv_prefix_len(&root, &[Move::WIN, Move::NONE], EnteringKingRule::Point27),
        1
    );
    assert_eq!(legal_pv_prefix_len(&root, &[Move::WIN], EnteringKingRule::None), 0);
}

#[test]
#[cfg(not(feature = "search-no-pass-rules"))]
fn public_pv_checks_pass_rights() {
    let mut root = startpos();
    root.enable_pass_rights(1, 1);
    assert_eq!(
        legal_pv_prefix_len(&root, &[Move::PASS, Move::PASS, Move::PASS], EnteringKingRule::None),
        2
    );
    assert_eq!(root.pass_rights(crate::types::Color::Black), 1);
    assert_eq!(root.pass_rights(crate::types::Color::White), 1);
}

#[test]
fn public_pv_callback_does_not_change_search_state() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for multi_pv in [1, 4] {
                let root = startpos();
                let mut control = Search::new(1);
                let limits = LimitsType {
                    nodes: 8000,
                    multi_pv,
                    ..Default::default()
                };
                let plain = control.go(&mut root.clone(), limits.clone(), None::<fn(&SearchInfo)>);
                let mut search = Search::new(1);
                let mut infos = Vec::new();
                let result = search.go(
                    &mut root.clone(),
                    limits,
                    Some(|info: &SearchInfo| {
                        assert_eq!(
                            legal_pv_prefix_len(&root, &info.pv, EnteringKingRule::None),
                            info.pv.len()
                        );
                        infos.push(info.clone());
                    }),
                );
                assert_eq!(
                    (result.best_move, result.score, result.depth, result.nodes),
                    (plain.best_move, plain.score, plain.depth, plain.nodes)
                );
                assert_eq!(result.pv, plain.pv);
                assert_eq!(result.pv, infos.last().unwrap().pv);
                assert_eq!(result.ponder_move, result.pv.get(1).copied().unwrap_or(Move::NONE));
                let state = &search.worker.as_ref().unwrap().state;
                let before = &control.worker.as_ref().unwrap().state;
                assert_eq!(state.previous_pv, before.previous_pv);
                let snapshot = |state: &super::super::alpha_beta::SearchState| {
                    state.root_moves.iter().map(|rm| (rm.score, rm.pv.clone())).collect::<Vec<_>>()
                };
                assert_eq!(snapshot(state), snapshot(before));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[ignore = "release で公開 PV 検証の単体コストを測定"]
fn public_pv_validation_cost() {
    for history in [0, 512] {
        let mut root = startpos();
        let cycle = line(&root, &["5i5h", "5a5b", "5h5i", "5b5a"]);
        for _ in 0..history / 4 {
            for &mv in &cycle {
                let check = root.gives_check(mv);
                root.do_move(mv, check);
            }
        }
        let pv = line(
            &root,
            &[
                "7g7f", "8c8d", "2g2f", "3c3d", "2f2e", "8d8e", "2e2d", "2c2d", "2h2d", "8e8f",
                "8g8f", "8b8f",
            ],
        );
        for len in [0, 1, 6, 12] {
            for validate in [false, true] {
                let start = std::time::Instant::now();
                for _ in 0..20_000 {
                    let input = std::hint::black_box(&pv[..len]);
                    let mut output = input.to_vec();
                    if validate {
                        output.truncate(legal_pv_prefix_len(
                            std::hint::black_box(&root),
                            input,
                            EnteringKingRule::None,
                        ));
                    }
                    std::hint::black_box(output);
                }
                println!(
                    "PV_COST,{history},{len},{validate},{}",
                    start.elapsed().as_nanos() / 20_000
                );
            }
        }
    }
}

/// 公開直前に不正手を差し込んでも外へ出ないこと。検証呼び出しを外すと失敗する。
#[test]
fn public_pv_validation_applies_on_callback_and_result() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let root = startpos();
            // 後手の手なので、先手番の公開 PV では常に不正。
            let corrupt = Move::from_usi("3a2b").unwrap();
            for with_callback in [false, true] {
                let mut search = Search::new(1);
                search.corrupt_public_pv = Some(corrupt);
                let limits = LimitsType {
                    nodes: 8000,
                    ..Default::default()
                };
                let mut infos = Vec::new();
                let result = if with_callback {
                    search.go(
                        &mut root.clone(),
                        limits,
                        Some(|info: &SearchInfo| infos.push(info.clone())),
                    )
                } else {
                    search.go(&mut root.clone(), limits, None::<fn(&SearchInfo)>)
                };
                assert!(!result.pv.contains(&corrupt));
                assert_ne!(result.ponder_move, corrupt);
                assert_eq!(
                    legal_pv_prefix_len(&root, &result.pv, EnteringKingRule::None),
                    result.pv.len()
                );
                assert_eq!(with_callback, !infos.is_empty());
                for info in &infos {
                    assert!(!info.pv.contains(&corrupt));
                    assert_eq!(
                        legal_pv_prefix_len(&root, &info.pv, EnteringKingRule::None),
                        info.pv.len()
                    );
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

/// 宣言可能でも WIN は ponder に選ばない。
#[test]
fn public_pv_never_ponders_declaration() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let mut root = Position::new();
            root.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
                .unwrap();
            let mut search = Search::new(1);
            search.set_entering_king_rule(EnteringKingRule::Point27);
            search.corrupt_public_pv = Some(Move::WIN);
            let result = search.go(
                &mut root,
                LimitsType {
                    depth: 1,
                    ..Default::default()
                },
                None::<fn(&SearchInfo)>,
            );
            assert_ne!(result.ponder_move, Move::WIN);
            if result.pv.last() == Some(&Move::WIN) {
                assert_eq!(result.ponder_move, Move::NONE);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn public_pv_terminal_result_and_no_callback() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for declaration in [false, true] {
                let mut root = Position::new();
                root.set_sfen(if declaration {
                    "KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1"
                } else {
                    "K8/8r/9/9/9/9/9/9/1r6k b - 1"
                })
                .unwrap();
                let mut search = Search::new(1);
                search.set_entering_king_rule(EnteringKingRule::Point27);
                let result = search.go(
                    &mut root,
                    LimitsType {
                        depth: 1,
                        ..Default::default()
                    },
                    None::<fn(&SearchInfo)>,
                );
                assert_eq!(result.pv, if declaration { vec![Move::WIN] } else { vec![] });
                assert_eq!(result.ponder_move, Move::NONE);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
