import { Slot } from "@radix-ui/react-slot";
import { cn } from "@shogi/design-system";
import { cva, type VariantProps } from "class-variance-authority";
import type { ButtonHTMLAttributes, ReactElement } from "react";
import { forwardRef } from "react";

const buttonVariants = cva(
    "inline-flex items-center justify-center whitespace-nowrap rounded-md text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50",
    {
        variants: {
            variant: {
                default: "bg-primary text-primary-foreground shadow hover:bg-primary/90",
                secondary: "bg-secondary text-secondary-foreground shadow-sm hover:bg-secondary/80",
                outline:
                    "border border-input bg-background hover:bg-muted/50 hover:text-foreground",
                ghost: "text-foreground hover:bg-muted/60",
                destructive:
                    "bg-destructive text-destructive-foreground shadow-sm hover:bg-destructive/90",
            },
            size: {
                default: "h-10 px-4 py-2",
                sm: "h-9 rounded-md px-3",
                lg: "h-11 rounded-md px-8",
                icon: "h-10 w-10",
            },
        },
        defaultVariants: {
            variant: "default",
            size: "default",
        },
    },
);

export interface ButtonProps
    extends ButtonHTMLAttributes<HTMLButtonElement>,
        VariantProps<typeof buttonVariants> {
    asChild?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
    { className, variant, size, asChild = false, ...props },
    ref,
): ReactElement {
    const Component = asChild ? Slot : "button";
    return (
        <Component
            className={cn(buttonVariants({ variant, size }), className)}
            ref={ref}
            {...props}
        />
    );
});

export const buttonStyles = buttonVariants;
