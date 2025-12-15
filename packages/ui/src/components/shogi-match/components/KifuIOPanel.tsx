import type { ReactElement } from "react";

const baseCard = {
    background: "hsl(var(--card, 0 0% 100%))",
    border: "1px solid hsl(var(--border, 0 0% 86%))",
    borderRadius: "12px",
    padding: "12px",
    boxShadow: "0 14px 28px rgba(0,0,0,0.12)",
};

export interface KifuIOPanelProps {
    /** 現在の手数のリスト */
    moves: string[];
    /** エクスポート用のUSI文字列 */
    exportUsi: string;
    /** エクスポート用のCSA文字列 */
    exportCsa: string;
    /** USIインポート時のコールバック */
    onImportUsi: (usi: string) => Promise<void>;
    /** CSAインポート時のコールバック */
    onImportCsa: (csa: string) => Promise<void>;
    /** 局面が準備完了しているか */
    positionReady: boolean;
}

export function KifuIOPanel({
    moves,
    exportUsi,
    exportCsa,
    onImportUsi,
    onImportCsa,
    positionReady,
}: KifuIOPanelProps): ReactElement {
    return (
        <div style={baseCard}>
            <div style={{ fontWeight: 700, marginBottom: "6px" }}>棋譜 / 入出力</div>
            <div
                style={{
                    fontSize: "13px",
                    color: "hsl(var(--muted-foreground, 0 0% 48%))",
                }}
            >
                先手から {moves.length} 手目
            </div>
            <ol
                style={{
                    paddingLeft: "18px",
                    maxHeight: "160px",
                    overflow: "auto",
                    margin: "8px 0",
                }}
            >
                {moves.map((mv, idx) => (
                    <li
                        key={`${idx}-${mv}`}
                        style={{
                            fontFamily: "ui-monospace, monospace",
                            fontSize: "13px",
                        }}
                    >
                        {idx + 1}. {mv}
                    </li>
                ))}
            </ol>
            <div style={{ display: "grid", gridTemplateColumns: "1fr", gap: "8px" }}>
                <label style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
                    USI / SFEN（現在の開始局面 + moves）
                    <textarea
                        value={exportUsi}
                        onChange={(e) => {
                            void onImportUsi(e.target.value);
                        }}
                        rows={3}
                        style={{
                            width: "100%",
                            padding: "8px",
                            borderRadius: "8px",
                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                        }}
                    />
                </label>
                <label style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
                    CSA
                    <textarea
                        value={exportCsa}
                        onChange={(e) => {
                            if (!positionReady) return;
                            void onImportCsa(e.target.value);
                        }}
                        rows={3}
                        style={{
                            width: "100%",
                            padding: "8px",
                            borderRadius: "8px",
                            border: "1px solid hsl(var(--border, 0 0% 86%))",
                        }}
                    />
                </label>
            </div>
        </div>
    );
}
