import type { EngineEvent } from "@shogi/engine-client";
import { describe, expect, it } from "vitest";
import { formatEvent } from "./useEngineManager";

describe("formatEvent", () => {
    it("bestmove イベントを正しくフォーマットする", () => {
        const event: EngineEvent = {
            type: "bestmove",
            move: "7g7f",
        };
        const result = formatEvent(event, "S:engine1");
        expect(result).toBe("[S:engine1] bestmove 7g7f");
    });

    it("info イベントを正しくフォーマットする（全フィールド）", () => {
        const event: EngineEvent = {
            type: "info",
            depth: 10,
            seldepth: 15,
            scoreCp: 150,
            nodes: 100000,
            nps: 50000,
            pv: ["7g7f", "3c3d", "2g2f"],
        };
        const result = formatEvent(event, "G:engine2");
        expect(result).toBe(
            "[G:engine2] info depth 10 seldepth 15 score cp 150 nodes 100000 nps 50000 pv 7g7f 3c3d 2g2f",
        );
    });

    it("info イベントを正しくフォーマットする（一部フィールドのみ）", () => {
        const event: EngineEvent = {
            type: "info",
            depth: 5,
            scoreCp: -200,
        };
        const result = formatEvent(event, "S:test");
        expect(result).toBe("[S:test] info depth 5 score cp -200");
    });

    it("info イベントで pv が空配列の場合は含めない", () => {
        const event: EngineEvent = {
            type: "info",
            depth: 3,
            pv: [],
        };
        const result = formatEvent(event, "G:test");
        expect(result).toBe("[G:test] info depth 3");
    });

    it("info イベントでフィールドが undefined の場合は含めない", () => {
        const event: EngineEvent = {
            type: "info",
        };
        const result = formatEvent(event, "S:engine");
        expect(result).toBe("[S:engine] info");
    });

    it("error イベントを正しくフォーマットする", () => {
        const event: EngineEvent = {
            type: "error",
            message: "Engine initialization failed",
        };
        const result = formatEvent(event, "G:engine3");
        expect(result).toBe("[G:engine3] error: Engine initialization failed");
    });

    it("ラベルが異なっても正しく動作する", () => {
        const event: EngineEvent = {
            type: "bestmove",
            move: "2g2f",
        };
        expect(formatEvent(event, "先手:内蔵エンジン")).toBe("[先手:内蔵エンジン] bestmove 2g2f");
        expect(formatEvent(event, "後手:外部エンジン")).toBe("[後手:外部エンジン] bestmove 2g2f");
    });
});
