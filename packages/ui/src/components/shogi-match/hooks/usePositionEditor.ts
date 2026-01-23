import type { BoardState, Piece, PieceType, Player, PositionState, Square } from "@shogi/app-core";
import { cloneBoard, getPositionService } from "@shogi/app-core";
import { useCallback, useState } from "react";
import { addToHand, cloneHandsState, consumeFromHand, countPieces } from "../utils/boardUtils";
import { PIECE_CAP, PIECE_LABELS } from "../utils/constants";
import type { LegalMoveCache } from "../utils/legalMoveCache";

/**
 * usePositionEditor の props
 */
export interface UsePositionEditorProps {
    /** 初期の局面 */
    initialPosition: PositionState;
    /** 初期の盤面 */
    initialBoard: BoardState | null;
    /** 対局が実行中かどうか */
    isMatchRunning: boolean;
    /** position を更新するコールバック */
    onPositionChange: (position: PositionState) => void;
    /** initialBoard を更新するコールバック */
    onInitialBoardChange: (board: BoardState) => void;
    /** moves を更新するコールバック */
    onMovesChange: (moves: string[]) => void;
    /** lastMove を更新するコールバック */
    onLastMoveChange: (lastMove: undefined) => void;
    /** selection を更新するコールバック */
    onSelectionChange: (selection: null) => void;
    /** message を設定するコールバック */
    onMessageChange: (message: string | null) => void;
    /** SFEN を更新するコールバック */
    onStartSfenRefresh: (position: PositionState) => Promise<void>;
    /** 合法手キャッシュ */
    legalCache: LegalMoveCache;
    /** 対局終了フラグの ref */
    matchEndedRef: React.MutableRefObject<boolean>;
    /** isMatchRunning を設定するコールバック */
    onMatchRunningChange: (isRunning: boolean) => void;
    /** position の ref */
    positionRef: React.MutableRefObject<PositionState>;
    /** moves の ref */
    movesRef: React.MutableRefObject<string[]>;
    /** search states をクリアするコールバック */
    onSearchStatesReset: () => void;
    /** active search をクリアするコールバック */
    onActiveSearchReset: () => void;
    /** clocks の ticking を null にするコールバック */
    onClockStop: () => void;
    /** basePosition を更新するコールバック */
    onBasePositionChange: (position: PositionState) => void;
}

/**
 * usePositionEditor の返り値
 */
interface UsePositionEditorReturn {
    // 編集状態
    isEditMode: boolean;
    editOwner: Player;
    editPieceType: PieceType | null;
    editPromoted: boolean;
    editFromSquare: Square | null;
    editTool: "place" | "erase";

    // 状態更新関数
    setIsEditMode: (v: boolean) => void;
    setEditOwner: (v: Player) => void;
    setEditPieceType: (v: PieceType | null) => void;
    setEditPromoted: (v: boolean) => void;
    setEditFromSquare: (v: Square | null) => void;
    setEditTool: (v: "place" | "erase") => void;

    // アクション関数
    resetToStartposForEdit: () => Promise<void>;
    updateTurnForEdit: (turn: Player) => void;
    placePieceAt: (
        square: Square,
        piece: Piece | null,
        options?: { fromSquare?: Square },
    ) => boolean;
    applyEditedPosition: (nextPosition: PositionState) => void;
    finalizeEditedPosition: () => Promise<void>;
}

/**
 * 局面編集モード管理のカスタムフック
 *
 * 編集モードでの駒配置・削除・局面操作を管理します。
 *
 * @param props - フックの設定
 * @returns 編集状態と操作関数
 */
export function usePositionEditor(props: UsePositionEditorProps): UsePositionEditorReturn {
    const [isEditMode, setIsEditMode] = useState(true);
    const [editOwner, setEditOwner] = useState<Player>("sente");
    const [editPieceType, setEditPieceType] = useState<PieceType | null>(null);
    const [editPromoted, setEditPromoted] = useState(false);
    const [editFromSquare, setEditFromSquare] = useState<Square | null>(null);
    const [editTool, setEditTool] = useState<"place" | "erase">("place");

    /**
     * 編集後の局面を主状態に反映する
     */
    const applyEditedPosition = useCallback(
        (nextPosition: PositionState) => {
            props.onPositionChange(nextPosition);
            props.positionRef.current = nextPosition;
            props.onInitialBoardChange(cloneBoard(nextPosition.board));
            props.onMovesChange([]);
            props.movesRef.current = [];
            props.onLastMoveChange(undefined);
            props.onSelectionChange(null);
            props.onMessageChange(null);
            setEditFromSquare(null);

            // 検索状態のリセット
            props.onSearchStatesReset();
            props.onActiveSearchReset();

            // キャッシュのクリア
            props.legalCache.clear();

            // クロックを停止
            props.onClockStop();

            // 対局終了フラグをリセット
            props.matchEndedRef.current = false;
            props.onMatchRunningChange(false);

            // SFEN を更新（非同期）
            void props.onStartSfenRefresh(nextPosition);
        },
        [props],
    );

    /**
     * 平手初期局面を復元する
     */
    const resetToStartposForEdit = useCallback(async () => {
        if (props.isMatchRunning) return;

        try {
            const service = getPositionService();
            const pos = await service.getInitialBoard();
            const clonedPos: PositionState = {
                board: cloneBoard(pos.board),
                hands: cloneHandsState(pos.hands),
                turn: pos.turn,
                ply: pos.ply,
            };
            applyEditedPosition(clonedPos);
            props.onInitialBoardChange(cloneBoard(pos.board));
            props.onMessageChange("平手初期化しました。");
        } catch (error) {
            props.onMessageChange(`平手初期化に失敗しました: ${String(error)}`);
        }
    }, [props, applyEditedPosition]);

    /**
     * 手番を更新する
     */
    const updateTurnForEdit = useCallback(
        (turn: Player) => {
            if (props.isMatchRunning) return;

            const current = props.positionRef.current;
            applyEditedPosition({ ...current, turn });
        },
        [props, applyEditedPosition],
    );

    /**
     * 指定マスに駒を配置・削除する
     */
    const placePieceAt = useCallback(
        (square: Square, piece: Piece | null, options?: { fromSquare?: Square }): boolean => {
            const current = props.positionRef.current;
            const nextBoard = cloneBoard(current.board);
            let workingHands = cloneHandsState(current.hands);

            // 移動元が指定されている場合は、そのマスから駒を削除
            if (options?.fromSquare) {
                nextBoard[options.fromSquare] = null;
            }

            // 配置先に既に駒がある場合は、その駒を手駒に回収
            const existing = nextBoard[square];
            if (existing) {
                const base = existing.type;
                workingHands = addToHand(workingHands, existing.owner, base);
            }

            // piece が null の場合：駒を削除
            if (!piece) {
                // 玉は削除できない
                if (existing?.type === "K") {
                    props.onMessageChange("玉は削除できません。");
                    return false;
                }
                nextBoard[square] = null;
                const nextPosition: PositionState = {
                    ...current,
                    board: nextBoard,
                    hands: workingHands,
                };
                applyEditedPosition(nextPosition);
                return true;
            }

            // 駒を配置する場合
            const baseType = piece.type;
            const consumedHands = consumeFromHand(workingHands, piece.owner, baseType);
            const handsForPlacement = consumedHands ?? workingHands;

            // 配置後の駒数をカウント
            const countsBefore = countPieces({
                ...current,
                board: nextBoard,
                hands: handsForPlacement,
            });

            const nextCount = countsBefore[piece.owner][baseType] + 1;

            // 駒の上限枚数チェック
            if (nextCount > PIECE_CAP[baseType]) {
                const ownerLabel = piece.owner === "sente" ? "先手" : "後手";
                props.onMessageChange(
                    `${ownerLabel}の${PIECE_LABELS[baseType]}は最大${PIECE_CAP[baseType]}枚までです`,
                );
                return false;
            }

            // 玉は1枚限定チェック
            if (piece.type === "K" && countsBefore[piece.owner][baseType] >= PIECE_CAP.K) {
                props.onMessageChange("玉はそれぞれ1枚まで配置できます。");
                return false;
            }

            // 駒を配置
            nextBoard[square] = piece.promoted ? { ...piece, promoted: true } : { ...piece };
            const finalHands = consumedHands ?? workingHands;
            const nextPosition: PositionState = {
                ...current,
                board: nextBoard,
                hands: finalHands,
            };
            applyEditedPosition(nextPosition);
            return true;
        },
        [props, applyEditedPosition],
    );

    /**
     * 編集済み局面を確定する
     */
    const finalizeEditedPosition = useCallback(async () => {
        if (props.isMatchRunning) return;

        const current = props.positionRef.current;
        const clonedPos: PositionState = {
            board: cloneBoard(current.board),
            hands: cloneHandsState(current.hands),
            turn: current.turn,
            ply: current.ply,
        };
        props.onBasePositionChange(clonedPos);
        props.onInitialBoardChange(cloneBoard(current.board));
        await props.onStartSfenRefresh(current);
        props.legalCache.clear();
        setIsEditMode(false);
    }, [props]);

    return {
        // 編集状態
        isEditMode,
        editOwner,
        editPieceType,
        editPromoted,
        editFromSquare,
        editTool,

        // 状態更新関数
        setIsEditMode,
        setEditOwner,
        setEditPieceType,
        setEditPromoted,
        setEditFromSquare,
        setEditTool,

        // アクション関数
        resetToStartposForEdit,
        updateTurnForEdit,
        placePieceAt,
        applyEditedPosition,
        finalizeEditedPosition,
    };
}
