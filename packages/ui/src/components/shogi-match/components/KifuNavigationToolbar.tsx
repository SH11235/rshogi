/**
 * 棋譜ナビゲーションツールバー
 *
 * 前へ/次へ/最初へ/最後へのナビゲーションボタンと
 * 分岐切替機能を提供する
 */

import type { ReactElement } from "react";
import { useCallback, useState } from "react";

interface BranchInfo {
    hasBranches: boolean;
    currentIndex: number;
    count: number;
    onSwitch: (index: number) => void;
    /** メインラインに昇格 */
    onPromoteToMain?: () => void;
}

interface KifuNavigationToolbarProps {
    /** 現在の手数 */
    currentPly: number;
    /** 最大手数 */
    totalPly: number;
    /** 1手戻る */
    onBack: () => void;
    /** 1手進む */
    onForward: () => void;
    /** 最初へ */
    onToStart: () => void;
    /** 最後へ */
    onToEnd: () => void;
    /** 無効化（対局中など） */
    disabled?: boolean;
    /** 分岐情報 */
    branchInfo?: BranchInfo;
    /** 巻き戻し中か */
    isRewound?: boolean;
}

/**
 * ナビゲーションボタン
 */
function NavButton({
    onClick,
    disabled,
    title,
    children,
}: {
    onClick: () => void;
    disabled?: boolean;
    title: string;
    children: React.ReactNode;
}) {
    return (
        <button
            type="button"
            onClick={onClick}
            disabled={disabled}
            title={title}
            className={`
                w-8 h-8 flex items-center justify-center
                rounded border border-border bg-background
                text-foreground text-sm font-medium
                transition-colors duration-150
                ${
                    disabled
                        ? "opacity-40 cursor-not-allowed"
                        : "hover:bg-accent cursor-pointer active:scale-95"
                }
            `}
        >
            {children}
        </button>
    );
}

export function KifuNavigationToolbar({
    currentPly,
    totalPly,
    onBack,
    onForward,
    onToStart,
    onToEnd,
    disabled = false,
    branchInfo,
    isRewound = false,
}: KifuNavigationToolbarProps): ReactElement {
    const [showBranchMenu, setShowBranchMenu] = useState(false);

    const canGoBack = currentPly > 0;
    const canGoForward = currentPly < totalPly;

    const handleBranchSelect = useCallback(
        (index: number) => {
            branchInfo?.onSwitch(index);
            setShowBranchMenu(false);
        },
        [branchInfo],
    );

    return (
        <div className="flex items-center gap-1.5 mb-2">
            {/* 最初へ */}
            <NavButton onClick={onToStart} disabled={disabled || !canGoBack} title="最初へ戻る">
                ⏮
            </NavButton>

            {/* 1手戻る */}
            <NavButton onClick={onBack} disabled={disabled || !canGoBack} title="1手戻る">
                ◀
            </NavButton>

            {/* 手数表示 */}
            <div
                className={`
                    flex-1 text-center text-[13px] font-medium
                    ${isRewound ? "text-wafuu-shu" : "text-foreground"}
                `}
            >
                {currentPly}/{totalPly}手
            </div>

            {/* 1手進む */}
            <NavButton onClick={onForward} disabled={disabled || !canGoForward} title="1手進む">
                ▶
            </NavButton>

            {/* 最後へ */}
            <NavButton onClick={onToEnd} disabled={disabled || !canGoForward} title="最後へ進む">
                ⏭
            </NavButton>

            {/* 分岐切替（分岐がある場合のみ表示） */}
            {branchInfo && branchInfo.hasBranches && (
                <div className="relative">
                    <button
                        type="button"
                        onClick={() => setShowBranchMenu(!showBranchMenu)}
                        disabled={disabled}
                        title={`分岐 ${branchInfo.currentIndex + 1}/${branchInfo.count}`}
                        className={`
                            px-2 h-8 flex items-center gap-1
                            rounded border border-border bg-background
                            text-foreground text-[12px]
                            transition-colors duration-150
                            ${disabled ? "opacity-40 cursor-not-allowed" : "hover:bg-accent cursor-pointer"}
                        `}
                    >
                        <span>分岐</span>
                        <span className="text-wafuu-shu font-medium">
                            {branchInfo.currentIndex + 1}/{branchInfo.count}
                        </span>
                        <span className="text-[10px]">▼</span>
                    </button>

                    {/* 分岐メニュー */}
                    {showBranchMenu && (
                        <div className="absolute top-full right-0 mt-1 z-50 bg-card border border-border rounded shadow-lg min-w-[100px]">
                            {Array.from({ length: branchInfo.count }, (_, i) => ({
                                id: `branch-${i}`,
                                index: i,
                            })).map((item) => (
                                <button
                                    type="button"
                                    key={item.id}
                                    onClick={() => handleBranchSelect(item.index)}
                                    className={`
                                        w-full px-3 py-1.5 text-left text-[12px]
                                        transition-colors duration-150
                                        ${
                                            item.index === branchInfo.currentIndex
                                                ? "bg-accent font-medium"
                                                : "hover:bg-accent/50"
                                        }
                                    `}
                                >
                                    変化 {item.index + 1}
                                    {item.index === 0 && (
                                        <span className="ml-1 text-[10px] text-muted-foreground">
                                            (メイン)
                                        </span>
                                    )}
                                </button>
                            ))}
                            {/* メインに昇格ボタン（メイン以外の分岐を選択中の場合） */}
                            {branchInfo.currentIndex > 0 && branchInfo.onPromoteToMain && (
                                <>
                                    <div className="border-t border-border my-1" />
                                    <button
                                        type="button"
                                        onClick={() => {
                                            branchInfo.onPromoteToMain?.();
                                            setShowBranchMenu(false);
                                        }}
                                        className="w-full px-3 py-1.5 text-left text-[12px] text-wafuu-shu hover:bg-accent/50 transition-colors duration-150"
                                    >
                                        ★ メインに昇格
                                    </button>
                                </>
                            )}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
}
