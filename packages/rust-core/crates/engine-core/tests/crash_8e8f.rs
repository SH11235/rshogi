use engine_core::usi::{move_to_usi, parse_usi_move, parse_usi_square};
use engine_core::{
    movegen::MoveGen,
    shogi::{Color, MoveList},
    Position,
};

#[test]
fn test_crash_8e8f_reproduction() {
    // 問題の局面を再現
    let moves_str = "7i6h 3c3d 2g2f 4a3b 4g4f 8c8d 5g5f 8d8e 8g8f 8e8f";

    let mut pos = Position::startpos();
    let moves: Vec<&str> = moves_str.split_whitespace().collect();
    let mut move_gen = MoveGen::new();

    println!("再現シーケンス開始...");

    for (i, move_str) in moves.iter().enumerate() {
        println!("手 {}: {}", i + 1, move_str);

        // USI形式の手をパース
        let _usi_move = parse_usi_move(move_str).expect("USI手のパース失敗");

        // 合法手を生成
        let mut moves_list = MoveList::new();
        move_gen.generate_all(&pos, &mut moves_list);

        // USI手に対応するMoveを探す
        let m = moves_list
            .as_slice()
            .iter()
            .find(|&mv| {
                let usi_str = move_to_usi(mv);
                usi_str == *move_str
            })
            .unwrap_or_else(|| {
                println!("エラー: '{move_str}' に対応する合法手が見つかりません");
                println!("生成された合法手:");
                for (idx, mv) in moves_list.as_slice().iter().enumerate() {
                    println!("  {}: {}", idx, move_to_usi(mv));
                }
                panic!("合法手が見つかりません");
            });

        // 手を適用する前の状態を確認
        println!("  適用前: 手番={:?}", pos.side_to_move);
        println!("  手: {m:?}");
        println!(
            "  手の詳細: from={:?}, to={:?}",
            if m.is_drop() { None } else { m.from() },
            m.to()
        );

        // 最後の手（8e8f）の前に詳細チェック
        if i == moves.len() - 1 {
            println!("\n=== クラッシュ直前の局面 ===");
            println!("手番: {:?}", pos.side_to_move);
            let sq_8f = parse_usi_square("8f").unwrap();
            let sq_8e = parse_usi_square("8e").unwrap();
            println!("8f の駒: {:?}", pos.board.piece_on(sq_8f));
            println!("8e の駒: {:?}", pos.board.piece_on(sq_8e));

            // ビットボードの状態も確認
            println!("\n局面の整合性チェック:");
            let bb_black = pos.board.occupied_bb[Color::Black as usize];
            let bb_white = pos.board.occupied_bb[Color::White as usize];
            let overlap = bb_black & bb_white;
            println!(
                "黒白の重複: {}",
                if overlap.is_empty() {
                    "なし"
                } else {
                    "あり！"
                }
            );
        }

        // 手を適用
        pos.do_move(*m);

        // 適用後の状態を確認
        println!("  適用後: 成功、新しい手番={:?}", pos.side_to_move);
    }

    println!("\n=== 最終局面での合法手生成 ===");
    // ここでクラッシュする可能性
    let mut moves_list = MoveList::new();
    move_gen.generate_all(&pos, &mut moves_list);
    println!("合法手数: {}", moves_list.len());
}

#[test]
fn test_direct_crash_position() {
    use std::panic;

    // パニックをキャッチして詳細を表示
    let result = panic::catch_unwind(|| {
        let mut pos = Position::startpos();
        let mut move_gen = MoveGen::new();

        // 問題の手順を再度実行（これが最も確実）
        let moves_str = "7i6h 3c3d 2g2f 4a3b 4g4f 8c8d 5g5f 8d8e 8g8f";

        for move_str in moves_str.split_whitespace() {
            let _usi_move = parse_usi_move(move_str).unwrap();
            let mut moves_list = MoveList::new();
            move_gen.generate_all(&pos, &mut moves_list);
            let m = moves_list
                .as_slice()
                .iter()
                .find(|&mv| {
                    let usi_str = move_to_usi(mv);
                    usi_str == move_str
                })
                .expect("合法手が見つかりません");
            pos.do_move(*m);
        }

        // 8e8f の手を適用
        let sq_8e = parse_usi_square("8e").unwrap();
        let sq_8f = parse_usi_square("8f").unwrap();

        println!("8e8f 適用前の状態:");
        println!("  8e: {:?}", pos.board.piece_on(sq_8e));
        println!("  8f: {:?}", pos.board.piece_on(sq_8f));

        // 8e8fの手を探す
        let mut moves_list = MoveList::new();
        move_gen.generate_all(&pos, &mut moves_list);
        let move_8e8f = *moves_list
            .as_slice()
            .iter()
            .find(|&mv| !mv.is_drop() && mv.from().unwrap() == sq_8e && mv.to() == sq_8f)
            .expect("8e8fの手が見つかりません");

        pos.do_move(move_8e8f);

        println!("8e8f 適用後、合法手生成...");
        let mut moves_list = MoveList::new();
        move_gen.generate_all(&pos, &mut moves_list);

        println!("クラッシュせずに完了！ 合法手数: {}", moves_list.len());
    });

    if let Err(e) = result {
        println!("パニック発生: {e:?}");
        panic!("予想通りクラッシュ");
    }
}
