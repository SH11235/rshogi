import type { NnueMeta } from "@shogi/app-core";
import { type ReactElement, useId } from "react";
import { NnueListItem } from "./NnueListItem";

export interface NnueListProps {
    /** NNUE メタデータ一覧 */
    nnueList: NnueMeta[];
    /** 選択中の NNUE ID（null = デフォルト） */
    selectedId: string | null;
    /** 選択変更時のコールバック */
    onSelect: (id: string | null) => void;
    /** 削除時のコールバック */
    onDelete?: (id: string) => void;
    /** 削除中の NNUE ID */
    deletingId?: string | null;
    /** 操作無効化 */
    disabled?: boolean;
    /** 表示名変更時のコールバック */
    onDisplayNameChange?: (id: string, newName: string) => Promise<void>;
}

/**
 * NNUE 一覧表示コンポーネント
 *
 * プリセットとユーザーアップロードを分けて表示する。
 */
export function NnueList({
    nnueList,
    selectedId,
    onSelect,
    onDelete,
    deletingId,
    disabled = false,
    onDisplayNameChange,
}: NnueListProps): ReactElement {
    const groupName = useId();
    const presets = nnueList.filter((m) => m.source === "preset");
    const userUploaded = nnueList.filter((m) => m.source !== "preset");

    return (
        <div
            role="radiogroup"
            aria-label="NNUE ファイル一覧"
            style={{ display: "flex", flexDirection: "column", gap: "8px" }}
        >
            {/* デフォルト（内蔵）オプション */}
            <NnueListItem
                meta={{
                    id: "__default__",
                    displayName: "デフォルト NNUE",
                    originalFileName: "embedded.nnue",
                    size: 0,
                    contentHashSha256: "",
                    source: "preset",
                    createdAt: 0,
                    verified: true,
                }}
                isSelected={selectedId === null}
                onSelect={() => onSelect(null)}
                name={groupName}
                showDelete={false}
                disabled={disabled}
            />

            {/* プリセット */}
            {presets.length > 0 && (
                <>
                    <div
                        style={{
                            fontSize: "12px",
                            fontWeight: 500,
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                            marginTop: "8px",
                            marginBottom: "4px",
                        }}
                    >
                        プリセット
                    </div>
                    {presets.map((meta) => (
                        <NnueListItem
                            key={meta.id}
                            meta={meta}
                            isSelected={selectedId === meta.id}
                            onSelect={() => onSelect(meta.id)}
                            name={groupName}
                            onDelete={onDelete ? () => onDelete(meta.id) : undefined}
                            showDelete={true}
                            isDeleting={deletingId === meta.id}
                            disabled={disabled}
                        />
                    ))}
                </>
            )}

            {/* ユーザーアップロード */}
            {userUploaded.length > 0 && (
                <>
                    <div
                        style={{
                            fontSize: "12px",
                            fontWeight: 500,
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                            marginTop: "8px",
                            marginBottom: "4px",
                        }}
                    >
                        インポート済み
                    </div>
                    {userUploaded.map((meta) => (
                        <NnueListItem
                            key={meta.id}
                            meta={meta}
                            isSelected={selectedId === meta.id}
                            onSelect={() => onSelect(meta.id)}
                            name={groupName}
                            onDelete={onDelete ? () => onDelete(meta.id) : undefined}
                            showDelete={true}
                            isDeleting={deletingId === meta.id}
                            disabled={disabled}
                            onDisplayNameChange={
                                onDisplayNameChange
                                    ? (newName) => onDisplayNameChange(meta.id, newName)
                                    : undefined
                            }
                        />
                    ))}
                </>
            )}
        </div>
    );
}
