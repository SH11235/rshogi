import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SettingsDialog } from "./SettingsDialog";

// useGameSettings のモック
vi.mock("@/hooks/useGameSettings", () => ({
    useGameSettings: () => ({
        settings: {
            timeControl: {
                mainTimeMinutes: 10,
                byoyomiSeconds: 30,
                enabled: false,
            },
            audio: {
                masterVolume: 50,
                pieceSound: true,
                checkSound: true,
                gameEndSound: true,
            },
            display: {
                theme: "auto",
                animations: true,
                showValidMoves: true,
                showLastMove: true,
            },
        },
        updateTimeControl: vi.fn(),
        updateAudio: vi.fn(),
        updateDisplay: vi.fn(),
        resetSettings: vi.fn(),
    }),
}));

describe("SettingsDialog", () => {
    const defaultProps = {
        isOpen: true,
        onOpenChange: vi.fn(),
    };

    it("should render settings dialog when open", () => {
        render(<SettingsDialog {...defaultProps} />);

        expect(screen.getByText("⚙️ ゲーム設定")).toBeInTheDocument();
        expect(screen.getByText("⏱️ 時間制御")).toBeInTheDocument();
        expect(screen.getByText("🔊 音声設定")).toBeInTheDocument();
        expect(screen.getByText("🎨 表示設定")).toBeInTheDocument();
    });

    it("should show time control checkbox", () => {
        render(<SettingsDialog {...defaultProps} />);

        const enableCheckbox = screen.getByLabelText("時間制限を有効にする");
        expect(enableCheckbox).not.toBeChecked();
        expect(enableCheckbox).toBeInTheDocument();
    });

    it("should display current audio settings", () => {
        render(<SettingsDialog {...defaultProps} />);

        expect(screen.getByDisplayValue("50")).toBeInTheDocument(); // 音量スライダー
        expect(screen.getByLabelText("駒音")).toBeChecked();
        expect(screen.getByLabelText("王手音")).toBeChecked();
        expect(screen.getByLabelText("ゲーム終了音")).toBeChecked();
    });

    it("should display current display settings", () => {
        render(<SettingsDialog {...defaultProps} />);

        expect(screen.getByLabelText("アニメーション")).toBeChecked();
        expect(screen.getByLabelText("有効手のハイライト")).toBeChecked();
        expect(screen.getByLabelText("最後の手をハイライト")).toBeChecked();
    });

    it("should show theme buttons", () => {
        render(<SettingsDialog {...defaultProps} />);

        expect(screen.getByText("ライト")).toBeInTheDocument();
        expect(screen.getByText("ダーク")).toBeInTheDocument();
        expect(screen.getByText("自動")).toBeInTheDocument();
    });

    it("should show reset confirmation dialog", async () => {
        const user = userEvent.setup();
        render(<SettingsDialog {...defaultProps} />);

        const resetButton = screen.getByText("設定をリセット");
        await user.click(resetButton);

        expect(screen.getByText("設定をリセットしますか？")).toBeInTheDocument();
        expect(
            screen.getByText("すべての設定がデフォルト値に戻されます。この操作は元に戻せません。"),
        ).toBeInTheDocument();
    });

    it("should have close button", () => {
        render(<SettingsDialog {...defaultProps} />);

        expect(screen.getByText("閉じる")).toBeInTheDocument();
    });

    it("should not render when closed", () => {
        render(<SettingsDialog {...defaultProps} isOpen={false} />);

        expect(screen.queryByText("⚙️ ゲーム設定")).not.toBeInTheDocument();
    });
});
