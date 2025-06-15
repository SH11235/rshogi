import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "./card";

describe("Card components", () => {
    describe("Card", () => {
        it("renders correctly", () => {
            render(<Card>Card content</Card>);

            expect(screen.getByText("Card content")).toBeInTheDocument();
        });

        it("accepts custom className", () => {
            const { container } = render(<Card className="custom-card">Content</Card>);
            const card = container.firstChild as HTMLElement;

            expect(card).toHaveClass("custom-card");
        });

        it("has correct data-slot attribute", () => {
            const { container } = render(<Card>Content</Card>);
            const card = container.firstChild as HTMLElement;

            expect(card).toHaveAttribute("data-slot", "card");
        });
    });

    describe("CardHeader", () => {
        it("renders correctly", () => {
            render(<CardHeader>Header content</CardHeader>);

            expect(screen.getByText("Header content")).toBeInTheDocument();
        });

        it("has correct data-slot attribute", () => {
            const { container } = render(<CardHeader>Header</CardHeader>);
            const header = container.firstChild as HTMLElement;

            expect(header).toHaveAttribute("data-slot", "card-header");
        });
    });

    describe("CardTitle", () => {
        it("renders correctly", () => {
            render(<CardTitle>Card Title</CardTitle>);

            expect(screen.getByText("Card Title")).toBeInTheDocument();
        });

        it("has correct data-slot attribute", () => {
            const { container } = render(<CardTitle>Title</CardTitle>);
            const title = container.firstChild as HTMLElement;

            expect(title).toHaveAttribute("data-slot", "card-title");
        });
    });

    describe("CardDescription", () => {
        it("renders correctly", () => {
            render(<CardDescription>Card description</CardDescription>);

            expect(screen.getByText("Card description")).toBeInTheDocument();
        });

        it("has correct data-slot attribute", () => {
            const { container } = render(<CardDescription>Description</CardDescription>);
            const description = container.firstChild as HTMLElement;

            expect(description).toHaveAttribute("data-slot", "card-description");
        });
    });

    describe("CardContent", () => {
        it("renders correctly", () => {
            render(<CardContent>Content area</CardContent>);

            expect(screen.getByText("Content area")).toBeInTheDocument();
        });

        it("has correct data-slot attribute", () => {
            const { container } = render(<CardContent>Content</CardContent>);
            const content = container.firstChild as HTMLElement;

            expect(content).toHaveAttribute("data-slot", "card-content");
        });
    });

    describe("CardFooter", () => {
        it("renders correctly", () => {
            render(<CardFooter>Footer content</CardFooter>);

            expect(screen.getByText("Footer content")).toBeInTheDocument();
        });

        it("has correct data-slot attribute", () => {
            const { container } = render(<CardFooter>Footer</CardFooter>);
            const footer = container.firstChild as HTMLElement;

            expect(footer).toHaveAttribute("data-slot", "card-footer");
        });
    });

    describe("Complete Card", () => {
        it("renders all components together", () => {
            render(
                <Card>
                    <CardHeader>
                        <CardTitle>Test Title</CardTitle>
                        <CardDescription>Test Description</CardDescription>
                    </CardHeader>
                    <CardContent>
                        <p>Test content</p>
                    </CardContent>
                    <CardFooter>
                        <button type="button">Test Button</button>
                    </CardFooter>
                </Card>,
            );

            expect(screen.getByText("Test Title")).toBeInTheDocument();
            expect(screen.getByText("Test Description")).toBeInTheDocument();
            expect(screen.getByText("Test content")).toBeInTheDocument();
            expect(screen.getByText("Test Button")).toBeInTheDocument();
        });

        it("maintains correct structure", () => {
            const { container } = render(
                <Card data-testid="card">
                    <CardHeader data-testid="header">
                        <CardTitle data-testid="title">Title</CardTitle>
                    </CardHeader>
                    <CardContent data-testid="content">Content</CardContent>
                </Card>,
            );

            const card = container.querySelector('[data-testid="card"]') as HTMLElement;
            const header = container.querySelector('[data-testid="header"]') as HTMLElement;
            const title = container.querySelector('[data-testid="title"]') as HTMLElement;
            const content = container.querySelector('[data-testid="content"]') as HTMLElement;

            expect(card).toContainElement(header);
            expect(header).toContainElement(title);
            expect(card).toContainElement(content);
        });
    });
});
