use engine_core::usi::{move_to_usi, parse_usi_square};
use engine_core::{movegen::MoveGen, shogi::MoveList, Position};

#[test]
fn test_usi_8e8f_coordinates() {
    // 8eと8fの座標を確認
    let sq_8e = parse_usi_square("8e").unwrap();
    let sq_8f = parse_usi_square("8f").unwrap();

    println!("8e = Square({}) = ({}, {})", sq_8e.index(), sq_8e.file(), sq_8e.rank());
    println!("8f = Square({}) = ({}, {})", sq_8f.index(), sq_8f.file(), sq_8f.rank());

    // USI座標系: ファイル1-9（右から左）、ランクa-i（上から下）
    // 内部座標系: ファイル0-8、ランク0-8
    assert_eq!(sq_8e.file(), 1); // ファイル8は内部表現で1
    assert_eq!(sq_8e.rank(), 4); // ランクeは内部表現で4
    assert_eq!(sq_8f.file(), 1); // ファイル8は内部表現で1
    assert_eq!(sq_8f.rank(), 5); // ランクfは内部表現で5
}

#[test]
fn test_pawn_movement_direction() {
    let mut pos = Position::startpos();
    let mut move_gen = MoveGen::new();

    // 初期局面での先手の歩の動き
    let mut moves_list = MoveList::new();
    move_gen.generate_all(&pos, &mut moves_list);

    // 8gの歩が8fに動けるか確認
    let sq_8g = parse_usi_square("8g").unwrap();
    let sq_8f = parse_usi_square("8f").unwrap();

    let found = moves_list
        .as_slice()
        .iter()
        .any(|&mv| !mv.is_drop() && mv.from() == Some(sq_8g) && mv.to() == sq_8f);

    assert!(found, "先手の歩(8g)は8fに動けるはず");

    // 手順を進めて後手の歩の動きを確認
    let moves_str = "7i6h 3c3d 2g2f 4a3b 4g4f 8c8d 5g5f 8d8e 8g8f";
    for move_str in moves_str.split_whitespace() {
        let mut moves_list = MoveList::new();
        move_gen.generate_all(&pos, &mut moves_list);

        let m = moves_list
            .as_slice()
            .iter()
            .find(|&mv| move_to_usi(mv) == move_str)
            .unwrap_or_else(|| panic!("手 '{move_str}' が見つかりません"));

        pos.do_move(*m);
    }

    // この時点で後手番のはず
    assert_eq!(pos.side_to_move, engine_core::shogi::Color::White);

    // 8eと8fの座標を定義
    let sq_8e = parse_usi_square("8e").unwrap();
    let sq_8f = parse_usi_square("8f").unwrap();

    // 8eと8fの駒を確認
    println!("\n8eの駒: {:?}", pos.board.piece_on(sq_8e));
    println!("8fの駒: {:?}", pos.board.piece_on(sq_8f));

    // 後手の合法手を生成
    moves_list.clear();
    move_gen.generate_all(&pos, &mut moves_list);

    println!("\n後手の合法手（8eから）:");
    for (i, &mv) in moves_list.as_slice().iter().enumerate() {
        if !mv.is_drop() && mv.from() == Some(sq_8e) {
            println!("  {}: {} (to={})", i, move_to_usi(&mv), mv.to());
        }
    }

    // 8e8fが合法手にあるか確認
    let found_8e8f = moves_list
        .as_slice()
        .iter()
        .any(|&mv| !mv.is_drop() && mv.from() == Some(sq_8e) && mv.to() == sq_8f);

    if !found_8e8f {
        // なぜ8e8fが合法手にないか調査
        println!("\n8e8fが合法手にない理由を調査:");

        // 歩の動きの定義を確認
        println!("後手の歩の動き方向を確認...");

        // 他の後手の歩の動きを確認
        for &mv in moves_list.as_slice() {
            if !mv.is_drop() {
                if let Some(from) = mv.from() {
                    let piece = pos.board.piece_on(from);
                    if let Some(p) = piece {
                        if p.piece_type == engine_core::shogi::PieceType::Pawn
                            && p.color == engine_core::shogi::Color::White
                        {
                            println!(
                                "  後手の歩: {} -> {} (rank {} -> {})",
                                from,
                                mv.to(),
                                from.rank(),
                                mv.to().rank()
                            );
                        }
                    }
                }
            }
        }
    }

    assert!(found_8e8f, "後手の歩(8e)は8fに動けるはず（8fの先手の歩を取る）");
}
