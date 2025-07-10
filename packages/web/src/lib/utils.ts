import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * Tailwind CSS class merging utility
 * Combines multiple class values and handles Tailwind conflicts properly
 */
export function cn(...inputs: ClassValue[]) {
    return twMerge(clsx(inputs));
}
