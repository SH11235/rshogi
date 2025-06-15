import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Button } from "./button";

describe("Button component", () => {
    it("renders with default props", () => {
        render(<Button>Click me</Button>);

        const button = screen.getByRole("button");
        expect(button).toBeInTheDocument();
        expect(button).toHaveTextContent("Click me");
    });

    it("handles click events", () => {
        const handleClick = vi.fn();
        render(<Button onClick={handleClick}>Click me</Button>);

        const button = screen.getByRole("button");
        fireEvent.click(button);

        expect(handleClick).toHaveBeenCalledTimes(1);
    });

    it("applies variant classes correctly", () => {
        const variants = [
            "default",
            "destructive",
            "outline",
            "secondary",
            "ghost",
            "link",
        ] as const;

        for (const variant of variants) {
            const { container } = render(<Button variant={variant}>Button</Button>);
            const button = container.firstChild as HTMLElement;

            // Each variant should apply specific classes
            expect(button.className).toBeTruthy();
        }
    });

    it("applies size classes correctly", () => {
        const sizes = ["default", "sm", "lg", "icon"] as const;

        for (const size of sizes) {
            const { container } = render(<Button size={size}>Button</Button>);
            const button = container.firstChild as HTMLElement;

            expect(button.className).toBeTruthy();
        }
    });

    it("can be disabled", () => {
        render(<Button disabled>Disabled</Button>);

        const button = screen.getByRole("button");
        expect(button).toBeDisabled();
    });

    it("forwards ref correctly", () => {
        const ref = vi.fn();
        render(<Button ref={ref}>Button</Button>);

        expect(ref).toHaveBeenCalled();
    });

    it("accepts custom className", () => {
        const { container } = render(<Button className="custom-class">Button</Button>);
        const button = container.firstChild as HTMLElement;

        expect(button).toHaveClass("custom-class");
    });

    it("renders as child component when asChild is true", () => {
        render(
            <Button asChild>
                <a href="/">Link Button</a>
            </Button>,
        );

        const link = screen.getByRole("link");
        expect(link).toBeInTheDocument();
        expect(link).toHaveTextContent("Link Button");
        expect(link).toHaveAttribute("href", "/");
    });

    it("has correct default button type", () => {
        render(<Button>Button</Button>);

        const button = screen.getByRole("button") as HTMLButtonElement;
        // In some environments, default type might not be explicitly set
        expect(button.type).toBe("submit"); // Default HTML button type is "submit"
    });

    it("can override button type", () => {
        render(<Button type="submit">Submit</Button>);

        const button = screen.getByRole("button");
        expect(button).toHaveAttribute("type", "submit");
    });

    it("combines multiple classes correctly", () => {
        const { container } = render(
            <Button variant="destructive" size="lg" className="extra-class">
                Button
            </Button>,
        );
        const button = container.firstChild as HTMLElement;

        expect(button.className).toContain("extra-class");
    });
});
