export type MateSearchStatus = "idle" | "searching" | "found" | "not_found" | "error";

export interface MateSearchState {
    status: MateSearchStatus;
    depth: number; // 探索中の深さ
    maxDepth: number; // 最大探索深さ
    result: MateSearchResultUI | null;
    error: string | null;
}

export interface MateSearchResultUI {
    isMate: boolean;
    moves: string[]; // 棋譜形式の手順
    nodeCount: number;
    elapsedMs: number;
    depth: number; // 詰み手数
}

export interface MateSearchOptions {
    maxDepth: number;
    useWasm?: boolean;
    timeout?: number;
}
