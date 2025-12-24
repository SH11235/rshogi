/**
 * 棋譜＋評価値管理フック
 *
 * 指し手履歴と盤面履歴からKIF形式の棋譜を生成し、
 * エンジンからの評価値を各手に紐付ける
 */

import type { BoardState } from "@shogi/app-core";
import type { EngineInfoEvent } from "@shogi/engine-client";
import { useCallback, useMemo, useRef } from "react";
import type { EvalHistory, KifMove } from "../utils/kifFormat";
import { convertMovesToKif } from "../utils/kifFormat";

interface EvalEntry {
    scoreCp?: number;
    scoreMate?: number;
    depth?: number;
}

interface UseKifuWithEvalResult {
    /** KIF形式の指し手リスト */
    kifMoves: KifMove[];
    /** 評価値の履歴（グラフ用） */
    evalHistory: EvalHistory[];
    /** 盤面履歴（KIFエクスポート用） */
    boardHistory: BoardState[];
    /** 盤面履歴を更新（指し手適用前に呼ぶ） */
    recordBoardState: (board: BoardState) => void;
    /** 評価値を記録（エンジンのinfoイベントで呼ぶ） */
    recordEval: (ply: number, event: EngineInfoEvent) => void;
    /** 履歴をクリア */
    clearHistory: () => void;
}

/**
 * 棋譜＋評価値管理フック
 *
 * @param moves USI形式の指し手配列
 * @returns KIF形式の棋譜と評価値履歴
 */
export function useKifuWithEval(moves: string[]): UseKifuWithEvalResult {
    // 盤面履歴（各手を適用する直前の盤面状態）
    const boardHistoryRef = useRef<BoardState[]>([]);

    // 評価値マップ（ply → 評価値）
    const evalMapRef = useRef<Map<number, EvalEntry>>(new Map());

    // 更新トリガー用カウンター（useMemoの依存配列用）
    const updateCounterRef = useRef(0);

    /**
     * 盤面状態を記録（指し手適用前に呼ぶ）
     */
    const recordBoardState = useCallback((board: BoardState) => {
        boardHistoryRef.current = [...boardHistoryRef.current, board];
        updateCounterRef.current += 1;
    }, []);

    /**
     * 評価値を記録
     */
    const recordEval = useCallback((ply: number, event: EngineInfoEvent) => {
        const existing = evalMapRef.current.get(ply);
        // より深い探索深さの評価値で更新
        if (!existing || (event.depth !== undefined && (existing.depth ?? 0) < event.depth)) {
            evalMapRef.current.set(ply, {
                scoreCp: event.scoreCp,
                scoreMate: event.scoreMate,
                depth: event.depth,
            });
            updateCounterRef.current += 1;
        }
    }, []);

    /**
     * 履歴をクリア
     */
    const clearHistory = useCallback(() => {
        boardHistoryRef.current = [];
        evalMapRef.current.clear();
        updateCounterRef.current += 1;
    }, []);

    // KIF形式の棋譜を生成
    const kifMoves = useMemo(() => {
        // updateCounterを依存に含めて再計算をトリガー
        void updateCounterRef.current;

        const boardHistory = boardHistoryRef.current;
        const evalMap = evalMapRef.current;

        // 盤面履歴がない場合は空配列
        if (boardHistory.length === 0 || moves.length === 0) {
            return [];
        }

        // movesと盤面履歴の長さを揃える
        const validMoves = moves.slice(0, boardHistory.length);

        return convertMovesToKif(validMoves, boardHistory, evalMap);
    }, [moves, updateCounterRef.current]);

    // 評価値履歴を生成（グラフ用）
    const evalHistory = useMemo((): EvalHistory[] => {
        void updateCounterRef.current;

        const history: EvalHistory[] = [
            { ply: 0, evalCp: 0, evalMate: null }, // 開始局面
        ];

        for (let i = 0; i < moves.length; i++) {
            const ply = i + 1;
            const evalEntry = evalMapRef.current.get(ply);

            history.push({
                ply,
                evalCp: evalEntry?.scoreCp ?? null,
                evalMate: evalEntry?.scoreMate ?? null,
            });
        }

        return history;
    }, [moves, updateCounterRef.current]);

    return {
        kifMoves,
        evalHistory,
        boardHistory: boardHistoryRef.current,
        recordBoardState,
        recordEval,
        clearHistory,
    };
}
