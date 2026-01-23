import type { ReactElement } from "react";
import { useCallback, useRef, useState } from "react";
import { Button } from "../button";

export interface NnueImportAreaProps {
    /** ファイル選択時のコールバック */
    onFileSelect: (file: File) => void;
    /** インポート中かどうか */
    isImporting?: boolean;
    /** 無効化 */
    disabled?: boolean;
}

/**
 * NNUE ファイルインポートエリア
 *
 * ファイルドロップとファイル選択ダイアログをサポート。
 */
export function NnueImportArea({
    onFileSelect,
    isImporting = false,
    disabled = false,
}: NnueImportAreaProps): ReactElement {
    const inputRef = useRef<HTMLInputElement>(null);
    const [isDragOver, setIsDragOver] = useState(false);

    const handleDragOver = useCallback(
        (e: React.DragEvent) => {
            e.preventDefault();
            if (!disabled && !isImporting) {
                setIsDragOver(true);
            }
        },
        [disabled, isImporting],
    );

    const handleDragLeave = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        setIsDragOver(false);
    }, []);

    const handleDrop = useCallback(
        (e: React.DragEvent) => {
            e.preventDefault();
            setIsDragOver(false);
            if (disabled || isImporting) return;

            const file = e.dataTransfer.files[0];
            if (file?.name.toLowerCase().endsWith(".nnue")) {
                onFileSelect(file);
            }
        },
        [disabled, isImporting, onFileSelect],
    );

    const handleFileChange = useCallback(
        (e: React.ChangeEvent<HTMLInputElement>) => {
            const file = e.target.files?.[0];
            if (file) {
                onFileSelect(file);
            }
            // リセットして同じファイルを再選択可能に
            e.target.value = "";
        },
        [onFileSelect],
    );

    const handleButtonClick = useCallback(() => {
        inputRef.current?.click();
    }, []);

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
                {isImporting
                    ? "インポート中..."
                    : isDragOver
                      ? "ここにドロップ"
                      : "NNUE ファイルをドラッグ＆ドロップ"}
            </div>
            <Button
                variant="outline"
                onClick={handleButtonClick}
                disabled={disabled || isImporting}
            >
                ファイルを選択...
            </Button>
        </div>
    );
}
