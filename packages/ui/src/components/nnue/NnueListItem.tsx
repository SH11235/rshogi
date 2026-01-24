import type { NnueMeta } from "@shogi/app-core";
import { cn } from "@shogi/design-system";
import { type ReactElement, useCallback, useId, useRef, useState } from "react";
import { Button } from "../button";
import { Input } from "../input";

export interface NnueListItemProps {
    /** NNUE メタデータ */
    meta: NnueMeta;
    /** 選択されているか */
    isSelected?: boolean;
    /** 選択時のコールバック */
    onSelect?: () => void;
    /** 削除時のコールバック */
    onDelete?: () => void;
    /** 削除ボタンを表示するか（プリセットは削除不可） */
    showDelete?: boolean;
    /** 削除中かどうか */
    isDeleting?: boolean;
    /** 無効化 */
    disabled?: boolean;
    /** ラジオグループ名（選択機能使用時に必要） */
    name?: string;
    /** 選択機能を有効にするか（デフォルト: true） */
    selectable?: boolean;
    /** 表示名変更時のコールバック（指定時、インライン編集が有効になる） */
    onDisplayNameChange?: (newName: string) => Promise<void>;
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
    isSelected = false,
    onSelect,
    onDelete,
    showDelete = true,
    isDeleting = false,
    disabled = false,
    name,
    selectable = true,
    onDisplayNameChange,
}: NnueListItemProps): ReactElement {
    const isPreset = meta.source === "preset";
    const canDelete = showDelete && !isPreset && onDelete;
    const canEdit = !isPreset && onDisplayNameChange;
    const inputId = useId();
    const editInputRef = useRef<HTMLInputElement>(null);

    // 編集状態
    const [isEditing, setIsEditing] = useState(false);
    const [editValue, setEditValue] = useState(meta.displayName);
    const [isSaving, setIsSaving] = useState(false);

    const startEditing = useCallback(() => {
        if (!canEdit || disabled) return;
        setEditValue(meta.displayName);
        setIsEditing(true);
        // 次のレンダリング後にフォーカス
        setTimeout(() => editInputRef.current?.select(), 0);
    }, [canEdit, disabled, meta.displayName]);

    const cancelEditing = useCallback(() => {
        setIsEditing(false);
        setEditValue(meta.displayName);
    }, [meta.displayName]);

    const saveDisplayName = useCallback(async () => {
        if (!onDisplayNameChange) return;
        const trimmed = editValue.trim();
        if (trimmed === "" || trimmed === meta.displayName) {
            cancelEditing();
            return;
        }
        setIsSaving(true);
        try {
            await onDisplayNameChange(trimmed);
            setIsEditing(false);
        } catch {
            // エラーは親コンポーネントで処理される
        } finally {
            setIsSaving(false);
        }
    }, [onDisplayNameChange, editValue, meta.displayName, cancelEditing]);

    const handleKeyDown = useCallback(
        (e: React.KeyboardEvent) => {
            if (e.key === "Enter") {
                e.preventDefault();
                void saveDisplayName();
            } else if (e.key === "Escape") {
                cancelEditing();
            }
        },
        [saveDisplayName, cancelEditing],
    );

    // 選択機能が無効な場合のスタイル
    const containerStyle = selectable
        ? {
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
          }
        : {
              display: "flex",
              alignItems: "center",
              gap: "12px",
              padding: "12px",
              borderRadius: "8px",
              backgroundColor: "transparent",
              border: "1px solid hsl(var(--border, 0 0% 86%))",
              opacity: disabled ? 0.5 : 1,
          };

    const containerClassName = selectable
        ? cn(
              "hover:bg-muted/50 focus-within:ring-2 focus-within:ring-ring focus-within:ring-offset-2 focus-within:ring-offset-background",
              isSelected && "bg-accent",
          )
        : "";

    return (
        <div style={containerStyle} className={containerClassName}>
            {/* ラジオボタン（選択機能有効時のみ） */}
            {selectable && (
                <>
                    <input
                        id={inputId}
                        type="radio"
                        name={name}
                        value={meta.id}
                        checked={isSelected}
                        onChange={() => onSelect?.()}
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
                                    <span style={{ color: "hsl(var(--warning, 38 92% 50%))" }}>
                                        未検証
                                    </span>
                                )}
                            </div>
                        </div>
                    </label>
                </>
            )}

            {/* コンテンツ（選択機能無効時） */}
            {!selectable && (
                <div style={{ flex: 1, minWidth: 0 }}>
                    <div
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: "8px",
                            marginBottom: "4px",
                        }}
                    >
                        {isEditing ? (
                            <Input
                                ref={editInputRef}
                                value={editValue}
                                onChange={(e) => setEditValue(e.target.value)}
                                onBlur={() => void saveDisplayName()}
                                onKeyDown={handleKeyDown}
                                disabled={isSaving}
                                className="h-7 text-sm font-medium"
                                style={{ maxWidth: "200px" }}
                            />
                        ) : canEdit ? (
                            <button
                                type="button"
                                onClick={(e) => {
                                    e.stopPropagation();
                                    startEditing();
                                }}
                                disabled={disabled}
                                style={{
                                    fontWeight: 500,
                                    overflow: "hidden",
                                    textOverflow: "ellipsis",
                                    whiteSpace: "nowrap",
                                    background: "none",
                                    border: "none",
                                    padding: "2px 4px",
                                    margin: "-2px -4px",
                                    borderRadius: "4px",
                                    cursor: disabled ? "not-allowed" : "pointer",
                                    textAlign: "left",
                                }}
                                className="hover:bg-muted/50"
                                title="クリックして編集"
                            >
                                {meta.displayName}
                            </button>
                        ) : (
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
                        )}
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
            )}

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
