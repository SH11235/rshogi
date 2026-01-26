import type { NnueDownloadProgress, PresetWithStatus } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import type { ReactElement } from "react";
import { getDownloadedMeta } from "../../hooks/usePresetManager";
import { Button } from "../button";
import { Progress } from "../progress";

interface PresetListItemProps {
    /** プリセットと状態 */
    preset: PresetWithStatus;
    /** 選択されているか */
    isSelected?: boolean;
    /** 選択時のコールバック（ダウンロード済みの場合のみ有効） */
    onSelect?: (nnueId: string) => void;
    /** ダウンロード時のコールバック */
    onDownload: (presetKey: string) => void;
    /** ダウンロード中かどうか */
    isDownloading: boolean;
    /** ダウンロード進捗 */
    downloadProgress: NnueDownloadProgress | null;
    /** 無効化 */
    disabled?: boolean;
    /** 選択機能を有効にするか（デフォルト: true） */
    selectable?: boolean;
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
    isSelected = false,
    onSelect,
    onDownload,
    isDownloading,
    downloadProgress,
    disabled = false,
    selectable = true,
}: PresetListItemProps): ReactElement {
    const { config, status } = preset;
    const { meta: downloadedMeta } = getDownloadedMeta(preset);
    // 選択可能: selectable が true かつダウンロード済み
    const canSelect = selectable && downloadedMeta !== null;

    const handleChange = () => {
        if (disabled || isDownloading) return;
        if (canSelect && downloadedMeta && onSelect) {
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

    const baseStyle: React.CSSProperties = {
        display: "flex",
        flexDirection: "column",
        gap: "8px",
        padding: "12px",
        borderRadius: "8px",
        backgroundColor: isSelected ? "hsl(var(--accent, 210 40% 96%))" : "transparent",
        border: isSelected
            ? "1px solid hsl(var(--primary, 220 90% 56%))"
            : "1px solid hsl(var(--border, 0 0% 86%))",
        opacity: disabled ? 0.5 : 1,
        transition: "background-color 150ms, border-color 150ms",
    };

    // 選択可能（ダウンロード済み）の場合のラベルスタイル
    const labelStyle: React.CSSProperties = {
        display: "flex",
        alignItems: "center",
        gap: "12px",
        cursor: !disabled ? "pointer" : "default",
    };

    // ステータスバッジを描画
    const renderStatusBadge = () => {
        if (status === "latest") {
            return (
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
            );
        }
        if (status === "update-available") {
            return (
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
            );
        }
        if (status === "not-downloaded") {
            return (
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
            );
        }
        return null;
    };

    // コンテンツ部分（名前、バッジ、説明）
    const contentSection = (
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
                {renderStatusBadge()}
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
    );

    // ダウンロードボタン
    const downloadButton = (status === "not-downloaded" || status === "update-available") && (
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
    );

    // ダウンロード進捗
    const progressSection = isDownloading && downloadProgress && (
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
                    {formatSize(downloadProgress.loaded)} / {formatSize(downloadProgress.total)} (
                    {progressPercent}%)
                </span>
            </div>
        </div>
    );

    // 選択不可の場合は静的な div
    if (!canSelect) {
        return (
            <div style={baseStyle}>
                <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
                    {contentSection}
                    {downloadButton}
                </div>
                {progressSection}
            </div>
        );
    }

    // 選択可能な場合は input type="radio" を使用
    return (
        <div
            style={baseStyle}
            className={cn(!disabled && "hover:bg-muted/50", isSelected && "bg-accent")}
        >
            <label style={labelStyle}>
                <input
                    type="radio"
                    checked={isSelected}
                    onChange={handleChange}
                    disabled={disabled || isDownloading}
                    style={{
                        width: "20px",
                        height: "20px",
                        margin: 0,
                        flexShrink: 0,
                        accentColor: "hsl(var(--primary, 220 90% 56%))",
                    }}
                />
                {contentSection}
                {downloadButton}
            </label>
            {progressSection}
        </div>
    );
}
