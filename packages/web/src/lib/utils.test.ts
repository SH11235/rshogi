import { describe, expect, it } from "vitest";
import { cn } from "./utils";

describe("cn utility function", () => {
    it("merges class names correctly", () => {
        expect(cn("btn", "btn-primary")).toBe("btn btn-primary");
    });

    it("handles conditional classes", () => {
        expect(cn("btn", true && "btn-primary", false && "btn-secondary")).toBe("btn btn-primary");
    });

    it("handles undefined and null values", () => {
        expect(cn("btn", undefined, null, "btn-primary")).toBe("btn btn-primary");
    });

    it("handles Tailwind class merging", () => {
        expect(cn("px-2 py-1", "px-4")).toBe("py-1 px-4");
        expect(cn("text-red-500", "text-blue-500")).toBe("text-blue-500");
    });

    it("handles empty input", () => {
        expect(cn()).toBe("");
    });

    it("handles array of classes", () => {
        expect(cn(["btn", "btn-primary"])).toBe("btn btn-primary");
    });

    it("handles object with boolean values", () => {
        expect(
            cn({
                btn: true,
                "btn-primary": true,
                "btn-secondary": false,
            }),
        ).toBe("btn btn-primary");
    });

    it("handles complex combinations", () => {
        const isActive = true;
        const isDisabled = false;

        expect(
            cn(
                "btn",
                "px-4 py-2",
                {
                    "btn-primary": isActive,
                    "btn-secondary": !isActive,
                    "opacity-50": isDisabled,
                },
                isActive && "font-semibold",
                "hover:bg-blue-600",
            ),
        ).toBe("btn px-4 py-2 btn-primary font-semibold hover:bg-blue-600");
    });
});
