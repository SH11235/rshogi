/// <reference types="vite/client" />

interface ImportMetaEnv {
    /** デフォルトの NNUE プリセットキー */
    readonly VITE_DEFAULT_NNUE_PRESET?: string;
}

interface ImportMeta {
    readonly env: ImportMetaEnv;
}
