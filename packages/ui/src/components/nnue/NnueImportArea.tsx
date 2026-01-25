import type { NnueStorageCapabilities } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useCallback, useMemo, useRef, useState } from "react";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
} from "../alert-dialog";
import { Button } from "../button";

export interface NnueImportAreaProps {
    /** ストレージの capability */
    capabilities: NnueStorageCapabilities;
    /** ファイル選択時のコールバック（File インポート: drag&drop + input） */
    onFileSelect?: (file: File) => void;
    /** ファイル選択ボタンクリック時のコールバック（Tauri dialog を開く） */
    onRequestFilePath?: () => void;
    /** インポート中かどうか */
    isImporting?: boolean;
    /** 無効化 */
    disabled?: boolean;
}

/** 許可された拡張子 */
const ALLOWED_EXTENSIONS = [".nnue", ".bin"];

/** ファイルが許可された拡張子かどうかを判定 */
function isAllowedExtension(fileName: string): boolean {
    const lowerName = fileName.toLowerCase();
    return ALLOWED_EXTENSIONS.some((ext) => lowerName.endsWith(ext));
}

/**
 * NNUE ファイルインポートエリア
 *
 * ファイルドロップとファイル選択ダイアログをサポート。
 * capabilities に基づいて機能を切り替える。
 */
export function NnueImportArea({
    capabilities,
    onFileSelect,
    onRequestFilePath,
    isImporting = false,
    disabled = false,
}: NnueImportAreaProps): ReactElement {
    const inputRef = useRef<HTMLInputElement>(null);
    const [isDragOver, setIsDragOver] = useState(false);
    const [pendingFile, setPendingFile] = useState<File | null>(null);
    const isCoarsePointer = useMemo(() => {
        if (typeof window === "undefined") return false;
        if (typeof window.matchMedia !== "function") return false;
        return window.matchMedia("(pointer: coarse)").matches;
    }, []);

    // capability とコールバックの両方が必要
    const canFileImport = capabilities.supportsFileImport && Boolean(onFileSelect);
    const canPathImport = capabilities.supportsPathImport && Boolean(onRequestFilePath);
    const canImport = canFileImport || canPathImport;

    // ファイルを処理（許可された拡張子ならそのまま、それ以外は確認ダイアログ）
    const processFile = useCallback(
        (file: File) => {
            if (isAllowedExtension(file.name)) {
                onFileSelect?.(file);
            } else {
                setPendingFile(file);
            }
        },
        [onFileSelect],
    );

    const handleDragOver = useCallback(
        (e: React.DragEvent) => {
            e.preventDefault();
            // canFileImport の場合のみドラッグ＆ドロップをサポート
            if (!disabled && !isImporting && canFileImport) {
                setIsDragOver(true);
            }
        },
        [disabled, isImporting, canFileImport],
    );

    const handleDragLeave = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        setIsDragOver(false);
    }, []);

    const handleDrop = useCallback(
        (e: React.DragEvent) => {
            e.preventDefault();
            setIsDragOver(false);
            if (disabled || isImporting || !canFileImport) return;

            const file = e.dataTransfer.files[0];
            if (file) {
                processFile(file);
            }
        },
        [disabled, isImporting, canFileImport, processFile],
    );

    const handleFileChange = useCallback(
        (e: React.ChangeEvent<HTMLInputElement>) => {
            const file = e.target.files?.[0];
            if (file) {
                processFile(file);
            }
            // リセットして同じファイルを再選択可能に
            e.target.value = "";
        },
        [processFile],
    );

    const handleButtonClick = useCallback(() => {
        // canPathImport が優先される（将来の Desktop drag&drop 対応時）
        if (canPathImport) {
            onRequestFilePath?.();
        } else if (canFileImport) {
            inputRef.current?.click();
        }
    }, [canPathImport, canFileImport, onRequestFilePath]);

    // 確認ダイアログでOKを押した場合
    const handleConfirmImport = useCallback(() => {
        if (pendingFile) {
            onFileSelect?.(pendingFile);
            setPendingFile(null);
        }
    }, [pendingFile, onFileSelect]);

    // 確認ダイアログでキャンセルを押した場合
    const handleCancelImport = useCallback(() => {
        setPendingFile(null);
    }, []);

    // メッセージも capability とコールバックの両方で判定
    const dropMessage = canFileImport
        ? isCoarsePointer
            ? ""
            : "NNUE ファイルをドラッグ＆ドロップ"
        : "ファイル選択ボタンをクリック";

    return (
        <>
            <section
                aria-label="NNUE ファイルインポートエリア"
                onDragOver={handleDragOver}
                onDragLeave={handleDragLeave}
                onDrop={handleDrop}
                style={{
                    border: isDragOver
                        ? "2px dashed hsl(var(--primary, 220 90% 56%))"
                        : "2px dashed hsl(var(--border, 0 0% 86%))",
                    borderRadius: "8px",
                    padding: "24px",
                    textAlign: "center",
                    backgroundColor: isDragOver ? "hsl(var(--accent, 210 40% 96%))" : "transparent",
                    transition: "border-color 150ms, background-color 150ms",
                    opacity: disabled || isImporting ? 0.5 : 1,
                    cursor: disabled || isImporting ? "not-allowed" : "default",
                }}
            >
                <input
                    ref={inputRef}
                    type="file"
                    accept=".nnue,.bin"
                    onChange={handleFileChange}
                    disabled={disabled || isImporting}
                    style={{ display: "none" }}
                />
                <div
                    style={{
                        marginBottom: "12px",
                        color: "hsl(var(--muted-foreground, 0 0% 45%))",
                    }}
                >
                    {isImporting ? "インポート中..." : isDragOver ? "ここにドロップ" : dropMessage}
                </div>
                <Button
                    variant="outline"
                    onClick={handleButtonClick}
                    disabled={disabled || isImporting || !canImport}
                >
                    ファイルを選択...
                </Button>
            </section>

            {/* 未知の拡張子の確認ダイアログ */}
            <AlertDialog
                open={pendingFile !== null}
                onOpenChange={(open) => !open && handleCancelImport()}
            >
                <AlertDialogContent>
                    <AlertDialogHeader>
                        <AlertDialogTitle>ファイル形式の確認</AlertDialogTitle>
                        <AlertDialogDescription>
                            「{pendingFile?.name}」は一般的な NNUE ファイルの拡張子（.nnue,
                            .bin）ではありません。 このファイルをインポートしますか？
                        </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                        <AlertDialogCancel onClick={handleCancelImport}>
                            キャンセル
                        </AlertDialogCancel>
                        <AlertDialogAction onClick={handleConfirmImport}>
                            インポート
                        </AlertDialogAction>
                    </AlertDialogFooter>
                </AlertDialogContent>
            </AlertDialog>
        </>
    );
}
