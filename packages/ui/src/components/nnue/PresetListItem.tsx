import type { NnueDownloadProgress, PresetWithStatus } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";
import { Progress } from "../progress";
import { Button } from "../button";
import { getDownloadedMeta } from "../../hooks/usePresetManager";

export interface PresetListItemProps {
    /** プリセットと状態 */
    preset: PresetWithStatus;
    /** 選択されているか */
    isSelected: boolean;
    /** 選択時のコールバック（ダウンロード済みの場合のみ有効） */
    onSelect: (nnueId: string) => void;
    /** ダウンロード時のコールバック */
    onDownload: (presetKey: string) => void;
    /** ダウンロード中かどうか */
    isDownloading: boolean;
    /** ダウンロード進捗 */
    downloadProgress: NnueDownloadProgress | null;
    /** 無効化 */
    disabled?: boolean;
}

function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function getPhaseLabel(phase: NnueDownloadProgress["phase"]): string {
    switch (phase) {
        case "downloading":
            return "ダウンロード中";
        case "validating":
            return "検証中";
        case "saving":
            return "保存中";
        default:
            return "処理中";
    }
}

/**
 * プリセット NNUE のリストアイテム
 *
 * ダウンロード状態に応じて表示を切り替える
 */
export function PresetListItem({
    preset,
    isSelected,
    onSelect,
    onDownload,
    isDownloading,
    downloadProgress,
    disabled = false,
}: PresetListItemProps): ReactElement {
    const { config, status } = preset;
    const { meta: downloadedMeta } = getDownloadedMeta(preset);
    const canSelect = downloadedMeta !== null;

    const handleClick = () => {
        if (disabled || isDownloading) return;
        if (canSelect && downloadedMeta) {
            onSelect(downloadedMeta.id);
        }
    };

    const handleDownload = (e: React.MouseEvent) => {
        e.stopPropagation();
        if (!isDownloading && !disabled) {
            onDownload(config.presetKey);
        }
    };

    const progressPercent =
        downloadProgress && downloadProgress.total > 0
            ? Math.round((downloadProgress.loaded / downloadProgress.total) * 100)
            : 0;

    return (
        <div
            style={{
                display: "flex",
                flexDirection: "column",
                gap: "8px",
                padding: "12px",
                borderRadius: "8px",
                backgroundColor: isSelected ? "hsl(var(--accent, 210 40% 96%))" : "transparent",
                border: isSelected
                    ? "1px solid hsl(var(--primary, 220 90% 56%))"
                    : "1px solid hsl(var(--border, 0 0% 86%))",
                cursor: canSelect && !disabled ? "pointer" : "default",
                opacity: disabled ? 0.5 : 1,
                transition: "background-color 150ms, border-color 150ms",
            }}
            className={cn(canSelect && !disabled && "hover:bg-muted/50", isSelected && "bg-accent")}
            onClick={handleClick}
            onKeyDown={
                canSelect && !disabled ? (e) => e.key === "Enter" && handleClick() : undefined
            }
            role={canSelect ? "radio" : undefined}
            aria-checked={canSelect ? isSelected : undefined}
            tabIndex={canSelect && !disabled ? 0 : -1}
        >
            <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
                {/* Radio indicator (only for downloaded presets) */}
                {canSelect && (
                    <div
                        style={{
                            width: "20px",
                            height: "20px",
                            borderRadius: "50%",
                            border: isSelected
                                ? "6px solid hsl(var(--primary, 220 90% 56%))"
                                : "2px solid hsl(var(--muted-foreground, 0 0% 45%))",
                            flexShrink: 0,
                        }}
                    />
                )}

                {/* Content */}
                <div style={{ flex: 1, minWidth: 0 }}>
                    <div
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: "8px",
                            marginBottom: "4px",
                        }}
                    >
                        <span
                            style={{
                                fontWeight: 500,
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                                whiteSpace: "nowrap",
                            }}
                        >
                            {config.displayName}
                        </span>
                        {/* Status badge */}
                        {status === "latest" && (
                            <span
                                style={{
                                    fontSize: "11px",
                                    padding: "2px 6px",
                                    borderRadius: "4px",
                                    backgroundColor: "hsl(var(--success, 142 76% 36%) / 0.1)",
                                    color: "hsl(var(--success, 142 76% 36%))",
                                }}
                            >
                                最新
                            </span>
                        )}
                        {status === "update-available" && (
                            <span
                                style={{
                                    fontSize: "11px",
                                    padding: "2px 6px",
                                    borderRadius: "4px",
                                    backgroundColor: "hsl(var(--warning, 38 92% 50%) / 0.1)",
                                    color: "hsl(var(--warning, 38 92% 50%))",
                                }}
                            >
                                更新あり
                            </span>
                        )}
                        {status === "not-downloaded" && (
                            <span
                                style={{
                                    fontSize: "11px",
                                    padding: "2px 6px",
                                    borderRadius: "4px",
                                    backgroundColor: "hsl(var(--muted, 0 0% 90%))",
                                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                }}
                            >
                                未ダウンロード
                            </span>
                        )}
                    </div>
                    <div
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: "12px",
                            fontSize: "13px",
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        }}
                    >
                        <span>{formatSize(config.size)}</span>
                        {config.license && <span>{config.license}</span>}
                    </div>
                    {config.description && (
                        <div
                            style={{
                                marginTop: "4px",
                                fontSize: "12px",
                                color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                lineHeight: 1.4,
                            }}
                        >
                            {config.description}
                        </div>
                    )}
                </div>

                {/* Download/Update button */}
                {(status === "not-downloaded" || status === "update-available") && (
                    <Button
                        variant={status === "update-available" ? "outline" : "default"}
                        size="sm"
                        onClick={handleDownload}
                        disabled={isDownloading || disabled}
                        style={{ flexShrink: 0 }}
                    >
                        {isDownloading
                            ? "ダウンロード中..."
                            : status === "update-available"
                              ? "更新"
                              : "ダウンロード"}
                    </Button>
                )}
            </div>

            {/* Download progress */}
            {isDownloading && downloadProgress && (
                <div style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
                    <Progress value={progressPercent} style={{ height: "6px" }} />
                    <div
                        style={{
                            display: "flex",
                            justifyContent: "space-between",
                            fontSize: "11px",
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        }}
                    >
                        <span>{getPhaseLabel(downloadProgress.phase)}</span>
                        <span>
                            {formatSize(downloadProgress.loaded)} /{" "}
                            {formatSize(downloadProgress.total)} ({progressPercent}%)
                        </span>
                    </div>
                </div>
            )}
        </div>
    );
}
