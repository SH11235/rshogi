/**
 * 棋譜インポートパネル
 *
 * SFEN/KIF形式の棋譜をインポートする機能を提供
 */

import type { ReactElement } from "react";
import { useCallback, useState } from "react";
import { parseKif, parseSfen } from "../utils/kifParser";

type ImportTab = "sfen" | "kif";

interface KifuImportPanelProps {
    /** SFENインポート時のコールバック（sfen: 開始局面, moves: 指し手配列） */
    onImportSfen: (sfen: string, moves: string[]) => Promise<void>;
    /** KIFインポート時のコールバック（moves: 指し手配列） */
    onImportKif: (moves: string[]) => Promise<void>;
    /** 局面が準備完了しているか */
    positionReady: boolean;
}

export function KifuImportPanel({
    onImportSfen,
    onImportKif,
    positionReady,
}: KifuImportPanelProps): ReactElement {
    const [activeTab, setActiveTab] = useState<ImportTab>("sfen");
    const [inputValue, setInputValue] = useState("");
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState(false);

    const handleImport = useCallback(async () => {
        if (!inputValue.trim()) {
            setError("入力が空です");
            return;
        }

        setError(null);
        setSuccess(false);

        try {
            if (activeTab === "sfen") {
                const { sfen, moves } = parseSfen(inputValue);
                if (!sfen) {
                    setError("SFENの形式が正しくありません");
                    return;
                }
                await onImportSfen(sfen, moves);
                setSuccess(true);
                setInputValue("");
                setTimeout(() => setSuccess(false), 2000);
            } else {
                const result = parseKif(inputValue);
                if (!result.success) {
                    setError(result.error ?? "KIFのパースに失敗しました");
                    return;
                }
                await onImportKif(result.moves);
                setSuccess(true);
                setInputValue("");
                setTimeout(() => setSuccess(false), 2000);
            }
        } catch (e) {
            setError(e instanceof Error ? e.message : "インポートに失敗しました");
        }
    }, [activeTab, inputValue, onImportSfen, onImportKif]);

    const handleTabChange = useCallback((tab: ImportTab) => {
        setActiveTab(tab);
        setError(null);
        setSuccess(false);
    }, []);

    const handleInputChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
        setInputValue(e.target.value);
        setError(null);
        setSuccess(false);
    }, []);

    return (
        <div className="bg-card border border-border rounded-xl p-3 shadow-lg w-[var(--panel-width)]">
            <div className="font-bold mb-2">インポート</div>

            {/* タブ切り替え */}
            <div className="flex gap-1 mb-3">
                <button
                    type="button"
                    className={`flex-1 px-3 py-1.5 text-sm rounded-md border transition-colors ${
                        activeTab === "sfen"
                            ? "bg-primary text-primary-foreground border-primary"
                            : "bg-background text-foreground border-border hover:bg-accent"
                    }`}
                    onClick={() => handleTabChange("sfen")}
                >
                    SFEN
                </button>
                <button
                    type="button"
                    className={`flex-1 px-3 py-1.5 text-sm rounded-md border transition-colors ${
                        activeTab === "kif"
                            ? "bg-primary text-primary-foreground border-primary"
                            : "bg-background text-foreground border-border hover:bg-accent"
                    }`}
                    onClick={() => handleTabChange("kif")}
                >
                    KIF
                </button>
            </div>

            {/* 説明文 */}
            <div className="text-xs text-muted-foreground mb-2">
                {activeTab === "sfen" ? (
                    <>
                        SFEN形式の局面を貼り付けてください
                        <br />
                        例: <code className="bg-muted px-1 rounded">lnsgkgsnl/...</code>
                    </>
                ) : (
                    <>
                        KIF形式の棋譜を貼り付けてください
                        <br />
                        例: <code className="bg-muted px-1 rounded">1 ７六歩(77)</code>
                    </>
                )}
            </div>

            {/* 入力エリア */}
            <textarea
                value={inputValue}
                onChange={handleInputChange}
                placeholder={
                    activeTab === "sfen"
                        ? "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
                        : "1 ７六歩(77)\n2 ３四歩(33)\n..."
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
