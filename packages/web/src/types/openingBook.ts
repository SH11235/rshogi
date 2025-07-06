export interface BookMove {
    notation: string;
    evaluation: number;
    depth: number;
}

export interface LoadProgress {
    phase: "downloading" | "decompressing" | "indexing";
    loaded: number;
    total: number;
}

export type BookLevel = "early" | "standard" | "full";
