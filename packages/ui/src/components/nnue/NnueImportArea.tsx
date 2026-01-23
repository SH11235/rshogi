import type { NnueStorageCapabilities } from "@shogi/app-core";
import type { ReactElement } from "react";
import { useCallback, useRef, useState } from "react";
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

    // capability とコールバックの両方が必要
    const canFileImport = capabilities.supportsFileImport && Boolean(onFileSelect);
    const canPathImport = capabilities.supportsPathImport && Boolean(onRequestFilePath);
    const canImport = canFileImport || canPathImport;

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
            if (file?.name.toLowerCase().endsWith(".nnue")) {
                onFileSelect?.(file);
            }
        },
        [disabled, isImporting, canFileImport, onFileSelect],
    );

    const handleFileChange = useCallback(
        (e: React.ChangeEvent<HTMLInputElement>) => {
            const file = e.target.files?.[0];
            if (file) {
                onFileSelect?.(file);
            }
            // リセットして同じファイルを再選択可能に
            e.target.value = "";
        },
        [onFileSelect],
    );

    const handleButtonClick = useCallback(() => {
        // canPathImport が優先される（将来の Desktop drag&drop 対応時）
        if (canPathImport) {
            onRequestFilePath?.();
        } else if (canFileImport) {
            inputRef.current?.click();
        }
    }, [canPathImport, canFileImport, onRequestFilePath]);

    // メッセージも capability とコールバックの両方で判定
    const dropMessage = canFileImport
        ? "NNUE ファイルをドラッグ＆ドロップ"
        : "ファイル選択ボタンをクリック";

    return (
        // biome-ignore lint/a11y/noStaticElementInteractions: Drop zone requires interactive div
        // biome-ignore lint/a11y/useSemanticElements: Drop zone with region role
        <div
            role="region"
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
                accept=".nnue"
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
        </div>
    );
}
