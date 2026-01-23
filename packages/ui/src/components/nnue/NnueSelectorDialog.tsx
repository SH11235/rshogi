import type { EngineClient } from "@shogi/engine-client";
import { type ReactElement, useCallback, useState } from "react";
import { useEngineRestart } from "../../hooks/useEngineRestart";
import { useNnueSelector } from "../../hooks/useNnueSelector";
import { useNnueStorage } from "../../hooks/useNnueStorage";
import { Button } from "../button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "../dialog";
import { EngineRestartingOverlay } from "./EngineRestartingOverlay";
import { NnueErrorAlert } from "./NnueErrorAlert";
import { NnueImportArea } from "./NnueImportArea";
import { NnueList } from "./NnueList";
import { NnueProgressOverlay } from "./NnueProgressOverlay";

export interface NnueSelectorDialogProps {
    /** モーダルが開いているか */
    open: boolean;
    /** モーダルを閉じる時のコールバック */
    onOpenChange: (open: boolean) => void;
    /** 現在エンジンで使用中の NNUE ID（null = デフォルト） */
    currentNnueId: string | null;
    /** NNUE 選択確定時のコールバック */
    onNnueChange?: (nnueId: string | null) => void;
    /** エンジンクライアント（再起動用） */
    engine?: EngineClient | null;
    /** エンジンが初期化済みか */
    isEngineInitialized?: boolean;
}

/**
 * NNUE 選択モーダル
 *
 * NNUE 一覧表示、インポート、削除、選択を一つのモーダルで提供する。
 */
export function NnueSelectorDialog({
    open,
    onOpenChange,
    currentNnueId,
    onNnueChange,
    engine,
    isEngineInitialized = false,
}: NnueSelectorDialogProps): ReactElement {
    const {
        nnueList,
        isLoading: isStorageLoading,
        error: storageError,
        importFromFile,
        deleteNnue,
        clearError: clearStorageError,
        storageUsage,
    } = useNnueStorage();

    const { selectedNnueId, select, showRestartWarning } = useNnueSelector({
        currentNnueId,
        isEngineInitialized,
    });

    const {
        isRestarting,
        error: restartError,
        restart,
        clearError: clearRestartError,
    } = useEngineRestart({
        engine: engine ?? null,
    });

    const [isImporting, setIsImporting] = useState(false);
    const [deletingId, setDeletingId] = useState<string | null>(null);

    const handleFileSelect = useCallback(
        async (file: File) => {
            setIsImporting(true);
            try {
                const meta = await importFromFile(file);
                // インポート後は自動的にその NNUE を選択
                select(meta.id);
            } catch {
                // エラーは useNnueStorage で管理される
            } finally {
                setIsImporting(false);
            }
        },
        [importFromFile, select],
    );

    const handleDelete = useCallback(
        async (id: string) => {
            setDeletingId(id);
            try {
                await deleteNnue(id);
                // 削除した NNUE が選択中だった場合はデフォルトに戻す
                if (selectedNnueId === id) {
                    select(null);
                }
            } catch {
                // エラーは useNnueStorage で管理される
            } finally {
                setDeletingId(null);
            }
        },
        [deleteNnue, selectedNnueId, select],
    );

    const handleConfirm = useCallback(async () => {
        // 変更がない場合は閉じるだけ
        if (selectedNnueId === currentNnueId) {
            onOpenChange(false);
            return;
        }

        // エンジン初期化済みの場合は再起動
        if (isEngineInitialized && engine) {
            await restart();
        }

        onNnueChange?.(selectedNnueId);
        onOpenChange(false);
    }, [
        selectedNnueId,
        currentNnueId,
        isEngineInitialized,
        engine,
        restart,
        onNnueChange,
        onOpenChange,
    ]);

    const handleCancel = useCallback(() => {
        onOpenChange(false);
    }, [onOpenChange]);

    const handleClearError = useCallback(() => {
        clearStorageError();
        clearRestartError();
    }, [clearStorageError, clearRestartError]);

    const error = storageError ?? restartError;
    const isDisabled = isImporting || deletingId !== null || isRestarting;

    return (
        <>
            <Dialog open={open} onOpenChange={onOpenChange}>
                <DialogContent
                    style={{
                        width: "min(520px, calc(100% - 24px))",
                        maxHeight: "80vh",
                        display: "flex",
                        flexDirection: "column",
                    }}
                >
                    <DialogHeader>
                        <DialogTitle>NNUE ファイル選択</DialogTitle>
                    </DialogHeader>

                    <div
                        style={{
                            flex: 1,
                            overflow: "auto",
                            display: "flex",
                            flexDirection: "column",
                            gap: "16px",
                            position: "relative",
                            minHeight: "200px",
                        }}
                    >
                        {/* エラー表示 */}
                        <NnueErrorAlert error={error} onClose={handleClearError} />

                        {/* NNUE 一覧 */}
                        <NnueList
                            nnueList={nnueList}
                            selectedId={selectedNnueId}
                            onSelect={select}
                            onDelete={handleDelete}
                            deletingId={deletingId}
                            disabled={isDisabled}
                        />

                        {/* インポートエリア */}
                        <NnueImportArea
                            onFileSelect={handleFileSelect}
                            isImporting={isImporting}
                            disabled={isDisabled}
                        />

                        {/* ストレージ使用量 */}
                        {storageUsage && (
                            <div
                                style={{
                                    fontSize: "12px",
                                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                    textAlign: "right",
                                }}
                            >
                                使用量: {(storageUsage.used / (1024 * 1024)).toFixed(1)} MB
                                {storageUsage.quota &&
                                    ` / ${(storageUsage.quota / (1024 * 1024)).toFixed(0)} MB`}
                            </div>
                        )}

                        {/* 進捗オーバーレイ */}
                        <NnueProgressOverlay
                            visible={isStorageLoading && !isImporting}
                            message="読み込み中..."
                        />
                    </div>

                    {/* 再起動警告 */}
                    {showRestartWarning && (
                        <div
                            style={{
                                display: "flex",
                                alignItems: "center",
                                gap: "8px",
                                padding: "8px 12px",
                                borderRadius: "6px",
                                backgroundColor: "hsl(var(--warning, 38 92% 50%) / 0.1)",
                                color: "hsl(var(--warning, 38 92% 50%))",
                                fontSize: "13px",
                            }}
                        >
                            <svg
                                width="16"
                                height="16"
                                viewBox="0 0 16 16"
                                fill="none"
                                aria-hidden="true"
                            >
                                <path
                                    d="M8 1L15 14H1L8 1Z"
                                    stroke="currentColor"
                                    strokeWidth="1.5"
                                    strokeLinejoin="round"
                                />
                                <path
                                    d="M8 6v4"
                                    stroke="currentColor"
                                    strokeWidth="1.5"
                                    strokeLinecap="round"
                                />
                                <circle cx="8" cy="12" r="0.5" fill="currentColor" />
                            </svg>
                            変更するとエンジンが再起動されます
                        </div>
                    )}

                    <DialogFooter>
                        <Button variant="outline" onClick={handleCancel} disabled={isRestarting}>
                            キャンセル
                        </Button>
                        <Button onClick={handleConfirm} disabled={isDisabled}>
                            {showRestartWarning ? "適用して再起動" : "適用"}
                        </Button>
                    </DialogFooter>
                </DialogContent>
            </Dialog>

            {/* エンジン再起動中オーバーレイ */}
            <EngineRestartingOverlay visible={isRestarting} />
        </>
    );
}
