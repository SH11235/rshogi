import type { EngineClient } from "@shogi/engine-client";
import { type ReactElement, useCallback, useState } from "react";
import { useEngineRestart } from "../../hooks/useEngineRestart";
import { useNnueSelector } from "../../hooks/useNnueSelector";
import { useNnueStorage } from "../../hooks/useNnueStorage";
import { usePresetManager } from "../../hooks/usePresetManager";
import { Button } from "../button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "../dialog";
import { EngineRestartingOverlay } from "./EngineRestartingOverlay";
import { NnueErrorAlert } from "./NnueErrorAlert";
import { NnueImportArea } from "./NnueImportArea";
import { NnueList } from "./NnueList";
import { NnueProgressOverlay } from "./NnueProgressOverlay";
import { PresetListItem } from "./PresetListItem";

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
    /** プリセット manifest.json の URL（指定時のみプリセット機能が有効） */
    manifestUrl?: string;
    /** Desktop 用: ファイル選択ダイアログを開いてパスを取得するコールバック */
    onRequestFilePath?: () => Promise<string | null>;
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
    manifestUrl,
    onRequestFilePath,
}: NnueSelectorDialogProps): ReactElement {
    const {
        nnueList,
        isLoading: isStorageLoading,
        error: storageError,
        importFromFile,
        importFromPath,
        deleteNnue,
        clearError: clearStorageError,
        storageUsage,
        refreshList,
        platform,
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

    const {
        presets,
        isLoading: isPresetsLoading,
        downloadingKey,
        downloadProgress,
        error: presetError,
        download: downloadPreset,
        clearError: clearPresetError,
        isConfigured: isPresetConfigured,
    } = usePresetManager({
        manifestUrl,
        autoFetch: open && Boolean(manifestUrl),
        onDownloadComplete: (meta) => {
            // ダウンロード完了時にストレージを更新して自動選択
            void refreshList();
            select(meta.id);
        },
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

    // Desktop 用: ファイルダイアログでパスを取得してインポート
    const handleRequestFilePath = useCallback(async () => {
        if (!onRequestFilePath) return;
        setIsImporting(true);
        try {
            const filePath = await onRequestFilePath();
            if (filePath) {
                const meta = await importFromPath(filePath);
                select(meta.id);
            }
        } catch {
            // エラーは useNnueStorage で管理される
        } finally {
            setIsImporting(false);
        }
    }, [onRequestFilePath, importFromPath, select]);

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
        clearPresetError();
    }, [clearStorageError, clearRestartError, clearPresetError]);

    const error = storageError ?? restartError ?? presetError;
    const isDisabled =
        isImporting || deletingId !== null || isRestarting || downloadingKey !== null;

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

                        {/* プリセット一覧（manifestUrl が設定されている場合のみ） */}
                        {isPresetConfigured && presets.length > 0 && (
                            <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
                                <div
                                    style={{
                                        fontSize: "12px",
                                        fontWeight: 500,
                                        color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                        marginTop: "8px",
                                        marginBottom: "4px",
                                    }}
                                >
                                    ダウンロード可能なプリセット
                                </div>
                                {presets.map((preset) => (
                                    <PresetListItem
                                        key={preset.config.presetKey}
                                        preset={preset}
                                        isSelected={preset.localMetas.some(
                                            (m) => m.id === selectedNnueId,
                                        )}
                                        onSelect={select}
                                        onDownload={downloadPreset}
                                        isDownloading={downloadingKey === preset.config.presetKey}
                                        downloadProgress={
                                            downloadingKey === preset.config.presetKey
                                                ? downloadProgress
                                                : null
                                        }
                                        disabled={isDisabled}
                                    />
                                ))}
                            </div>
                        )}

                        {/* プリセット読み込み中 */}
                        {isPresetConfigured && isPresetsLoading && (
                            <div
                                style={{
                                    fontSize: "13px",
                                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                    textAlign: "center",
                                    padding: "16px",
                                }}
                            >
                                プリセット一覧を読み込み中...
                            </div>
                        )}

                        {/* インポートエリア */}
                        <NnueImportArea
                            onFileSelect={platform === "web" ? handleFileSelect : undefined}
                            onRequestFilePath={
                                platform === "desktop" ? handleRequestFilePath : undefined
                            }
                            platform={platform ?? "web"}
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
