use desktop_lib::{get_initial_board_for_test, parse_sfen_to_board_for_test};

#[test]
fn test_get_initial_board_command() {
    let result = get_initial_board_for_test();
    assert!(result.is_ok());
    let board = result.unwrap();
    assert_eq!(board.turn, "sente");
    assert_eq!(board.cells.len(), 9);
}

#[test]
fn test_parse_sfen_command() {
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let result = parse_sfen_to_board_for_test(sfen.to_string());
    assert!(result.is_ok());
    let board = result.unwrap();
    assert_eq!(board.turn, "sente");
    assert_eq!(board.cells[0].len(), 9);
}
