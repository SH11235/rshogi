import type { NnueMeta } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import { type ReactElement, useId } from "react";
import { Button } from "../button";

export interface NnueListItemProps {
    /** NNUE メタデータ */
    meta: NnueMeta;
    /** 選択されているか */
    isSelected: boolean;
    /** 選択時のコールバック */
    onSelect: () => void;
    /** 削除時のコールバック */
    onDelete?: () => void;
    /** 削除ボタンを表示するか（プリセットは削除不可） */
    showDelete?: boolean;
    /** 削除中かどうか */
    isDeleting?: boolean;
    /** 無効化 */
    disabled?: boolean;
    /** ラジオグループ名 */
    name: string;
}

function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * NNUE 一覧の個々のアイテム
 */
export function NnueListItem({
    meta,
    isSelected,
    onSelect,
    onDelete,
    showDelete = true,
    isDeleting = false,
    disabled = false,
    name,
}: NnueListItemProps): ReactElement {
    const isPreset = meta.source === "preset";
    const canDelete = showDelete && !isPreset && onDelete;
    const inputId = useId();

    return (
        <div
            style={{
                display: "flex",
                alignItems: "center",
                gap: "12px",
                padding: "12px",
                borderRadius: "8px",
                backgroundColor: isSelected ? "hsl(var(--accent, 210 40% 96%))" : "transparent",
                border: isSelected
                    ? "1px solid hsl(var(--primary, 220 90% 56%))"
                    : "1px solid hsl(var(--border, 0 0% 86%))",
                cursor: disabled ? "not-allowed" : "pointer",
                opacity: disabled ? 0.5 : 1,
                transition: "background-color 150ms, border-color 150ms",
            }}
            className={cn(
                "hover:bg-muted/50 focus-within:ring-2 focus-within:ring-ring focus-within:ring-offset-2 focus-within:ring-offset-background",
                isSelected && "bg-accent",
            )}
        >
            <input
                id={inputId}
                type="radio"
                name={name}
                value={meta.id}
                checked={isSelected}
                onChange={() => onSelect()}
                disabled={disabled}
                className="sr-only"
            />
            <label htmlFor={inputId} className="flex flex-1 min-w-0 items-center gap-3">
                {/* Radio indicator */}
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
                            {meta.displayName}
                        </span>
                        {isPreset && (
                            <span
                                style={{
                                    fontSize: "11px",
                                    padding: "2px 6px",
                                    borderRadius: "4px",
                                    backgroundColor: "hsl(var(--muted, 0 0% 90%))",
                                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                }}
                            >
                                プリセット
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
                        <span>{formatSize(meta.size)}</span>
                        {meta.verified ? (
                            <span style={{ color: "hsl(var(--success, 142 76% 36%))" }}>
                                検証済み
                            </span>
                        ) : (
                            <span style={{ color: "hsl(var(--warning, 38 92% 50%))" }}>未検証</span>
                        )}
                    </div>
                </div>
            </label>

            {/* Delete button */}
            {canDelete && (
                <Button
                    variant="ghost"
                    size="sm"
                    onClick={(e) => {
                        e.stopPropagation();
                        onDelete();
                    }}
                    disabled={isDeleting || disabled}
                    style={{ flexShrink: 0 }}
                    aria-label={`${meta.displayName} を削除`}
                >
                    {isDeleting ? "..." : "削除"}
                </Button>
            )}
        </div>
    );
}
