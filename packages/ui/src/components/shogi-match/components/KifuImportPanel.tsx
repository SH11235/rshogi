/**
 * 棋譜インポートパネル
 *
 * SFEN/KIF形式の棋譜をインポートする機能を提供
 */

import type { ReactElement } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import { type KifMoveData, parseKif, parseSfen } from "../utils/kifParser";

interface KifuImportPanelProps {
    /** SFENインポート時のコールバック（sfen: 開始局面, moves: 指し手配列） */
    onImportSfen: (sfen: string, moves: string[]) => Promise<void>;
    /** KIFインポート時のコールバック（moves: 指し手配列, moveData: 各手の詳細データ, startSfen: 開始局面） */
    onImportKif: (moves: string[], moveData: KifMoveData[], startSfen?: string) => Promise<void>;
    /** 局面が準備完了しているか */
    positionReady: boolean;
}

export function KifuImportPanel({
    onImportSfen,
    onImportKif,
    positionReady,
}: KifuImportPanelProps): ReactElement {
    const [inputValue, setInputValue] = useState("");
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState(false);
    // タイマーIDを保持してクリーンアップに使用
    const successTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    // コンポーネントアンマウント時にタイマーをクリーンアップ
    useEffect(() => {
        return () => {
            if (successTimerRef.current) {
                clearTimeout(successTimerRef.current);
            }
        };
    }, []);

    const handleImport = useCallback(async () => {
        if (!inputValue.trim()) {
            setError("入力が空です");
            return;
        }

        setError(null);
        setSuccess(false);
        // 既存のタイマーをクリア
        if (successTimerRef.current) {
            clearTimeout(successTimerRef.current);
            successTimerRef.current = null;
        }

        try {
            const looksLikeKif =
                /#KIF|手数----|開始日時|終了日時|手合割|開始局面|先手：|後手：|持ち時間|表題|棋戦/.test(
                    inputValue,
                ) ||
                /[▲△☗☖]/.test(inputValue) ||
                /[１２３４５６７８９1-9][一二三四五六七八九].*(歩|香|桂|銀|金|角|飛|玉|王|と|馬|龍|竜)/.test(
                    inputValue,
                );

            if (looksLikeKif) {
                const result = parseKif(inputValue);
                if (!result.success) {
                    setError(result.error ?? "KIFのパースに失敗しました");
                    return;
                }
                await onImportKif(result.moves, result.moveData, result.startSfen);
                setSuccess(true);
                setInputValue("");
                successTimerRef.current = setTimeout(() => setSuccess(false), 2000);
                return;
            }

            const { sfen, moves } = parseSfen(inputValue);
            if (!sfen) {
                setError("SFENの形式が正しくありません");
                return;
            }
            await onImportSfen(sfen, moves);
            setSuccess(true);
            setInputValue("");
            successTimerRef.current = setTimeout(() => setSuccess(false), 2000);
        } catch (e) {
            setError(e instanceof Error ? e.message : "インポートに失敗しました");
        }
    }, [inputValue, onImportSfen, onImportKif]);

    const handleInputChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
        setInputValue(e.target.value);
        setError(null);
        setSuccess(false);
    }, []);

    return (
        <div className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]">
            <div className="font-bold mb-2">インポート（SFEN / KIF）</div>

            {/* 説明文 */}
            <div className="text-xs text-muted-foreground mb-2">
                SFEN形式の局面、またはKIF形式の棋譜を貼り付けてください
                <br />
                例: <code className="bg-muted px-1 rounded">startpos moves 7g7f 3c3d</code>
                <br />
                例: <code className="bg-muted px-1 rounded">1 ７六歩(77)</code>
            </div>

            {/* 入力エリア */}
            <textarea
                value={inputValue}
                onChange={handleInputChange}
                placeholder={
                    "startpos moves 7g7f 3c3d\nlnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n1 ７六歩(77)\n2 ３四歩(33)"
                }
                rows={5}
                className="w-full p-2 text-sm font-mono rounded-md border border-border bg-background resize-none focus:outline-none focus:ring-2 focus:ring-ring"
                disabled={!positionReady}
            />

            {/* エラー表示 */}
            {error && <div className="mt-2 text-xs text-destructive">{error}</div>}

            {/* 読み込みボタン */}
            <button
                type="button"
                className={`mt-3 w-full py-2 text-sm rounded-md border transition-colors ${
                    success
                        ? "bg-green-600 text-white border-green-600"
                        : "bg-primary text-primary-foreground border-primary hover:bg-primary/90"
                } disabled:opacity-50 disabled:cursor-not-allowed`}
                onClick={handleImport}
                disabled={!positionReady || !inputValue.trim()}
            >
                {success ? "読み込み完了" : "読み込み"}
            </button>
        </div>
    );
}
