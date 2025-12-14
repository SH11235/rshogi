import type { ReactElement, ReactNode } from "react";

export interface PlaygroundPageProps {
    eyebrow: string;
    title?: string;
    summary: string;
    note?: string;
    children: ReactNode;
}

export function PlaygroundPage({
    eyebrow,
    title = "Shogi Playground",
    summary,
    note,
    children,
}: PlaygroundPageProps): ReactElement {
    return (
        <main className="mx-auto flex max-w-[1100px] flex-col gap-[18px] px-5 pb-[72px] pt-10">
            <section className="rounded-2xl border border-[hsl(var(--border))] bg-gradient-to-br from-[rgba(255,138,76,0.12)] to-[rgba(255,209,126,0.18)] px-[22px] py-5 shadow-[0_18px_36px_rgba(0,0,0,0.12)]">
                <p className="m-0 text-xs font-bold uppercase tracking-[0.14em] text-[hsl(var(--accent))]">
                    {eyebrow}
                </p>
                <h1 className="mb-1 mt-2 text-[30px] tracking-[-0.02em]">{title}</h1>
                <p className="m-0 text-[15px] leading-relaxed text-[hsl(var(--muted-foreground))]">
                    {summary}
                </p>
                {note ? (
                    <p className="mb-0 mt-2 text-xs text-[hsl(var(--muted-foreground))]">{note}</p>
                ) : null}
            </section>

            <div className="flex flex-col gap-[14px]">{children}</div>
        </main>
    );
}
