import { describe, expect, it } from "vitest";
import { cn } from "./cn";

describe("cn", () => {
    it("複数のクラスをマージする", () => {
        expect(cn("foo", "bar")).toBe("foo bar");
    });

    it("条件付きクラスを処理する", () => {
        expect(cn("foo", false && "bar")).toBe("foo");
        expect(cn("foo", true && "bar")).toBe("foo bar");
    });

    it("undefined と null を無視する", () => {
        expect(cn("foo", undefined, "bar", null)).toBe("foo bar");
    });

    it("配列を展開する", () => {
        expect(cn(["foo", "bar"])).toBe("foo bar");
    });

    it("Tailwind のクラス競合を解決する", () => {
        // tw-merge が後の方のクラスを優先する
        expect(cn("p-4", "p-8")).toBe("p-8");
        expect(cn("text-red-500", "text-blue-500")).toBe("text-blue-500");
        expect(cn("bg-red-500", "bg-blue-500")).toBe("bg-blue-500");
    });

    it("複雑な組み合わせを処理する", () => {
        expect(cn("px-2 py-1", "px-4")).toBe("py-1 px-4");
        expect(cn("text-sm font-bold", "text-lg")).toBe("font-bold text-lg");
    });

    it("空の入力を処理する", () => {
        expect(cn()).toBe("");
        expect(cn("")).toBe("");
    });

    it("オブジェクト形式を処理する", () => {
        expect(cn({ foo: true, bar: false })).toBe("foo");
        expect(cn({ foo: false, bar: true })).toBe("bar");
        expect(cn({ foo: true, bar: true })).toBe("foo bar");
    });

    it("複雑なネストを処理する", () => {
        expect(
            cn(
                "base-class",
                {
                    "conditional-class": true,
                    "ignored-class": false,
                },
                ["array-class-1", "array-class-2"],
                undefined,
                "final-class",
            ),
        ).toBe("base-class conditional-class array-class-1 array-class-2 final-class");
    });
});
