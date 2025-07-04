import { describe, expect, it } from "vitest";
import type { NormalMove } from "../domain/model/move";
import type { OpeningEntry, OpeningMove } from "./openingBook";

describe("OpeningBook Memory Analysis", () => {
    it("should calculate memory usage for a single entry", () => {
        // Create a sample move
        const sampleMove: NormalMove = {
            type: "move",
            from: { row: 7, column: 7 },
            to: { row: 6, column: 7 },
            piece: { type: "pawn", owner: "black", promoted: false },
            promote: false,
            captured: null,
        };

        // Create a sample opening move
        const openingMove: OpeningMove = {
            move: sampleMove,
            weight: 40,
            name: "居飛車",
            comment: "最も一般的な初手",
        };

        // Create a sample entry
        const entry: OpeningEntry = {
            position: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL",
            moves: [openingMove, openingMove, openingMove], // Average 3 moves per position
            depth: 5,
        };

        // Estimate memory size
        const moveSize = JSON.stringify(sampleMove).length;
        const openingMoveSize = JSON.stringify(openingMove).length;
        const entrySize = JSON.stringify(entry).length;

        console.log("Memory estimation for OpeningBook:");
        console.log(`- Single Move object: ~${moveSize} bytes`);
        console.log(`- Single OpeningMove object: ~${openingMoveSize} bytes`);
        console.log(`- Single OpeningEntry object: ~${entrySize} bytes`);
        console.log("");
        console.log("For 120,000 entries:");
        console.log(`- Total size: ~${((entrySize * 120000) / 1024 / 1024).toFixed(2)} MB`);
        console.log(
            `- With Map overhead (~30%): ~${((entrySize * 120000 * 1.3) / 1024 / 1024).toFixed(2)} MB`,
        );
        console.log("");
        console.log("Additional considerations:");
        console.log("- JavaScript object overhead: ~50-100 bytes per object");
        console.log("- String interning might reduce memory for repeated piece types");
        console.log("- Browser typically allows 1-4GB heap per tab");

        expect(entrySize).toBeGreaterThan(0);
    });

    it("should analyze memory with optimized structure", () => {
        // Optimized structure using indices instead of full objects
        const optimizedEntry = {
            p: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL", // position
            m: [
                { f: 76, t: 66, w: 40, n: 0 }, // from/to as numbers, name as index
                { f: 72, t: 62, w: 30, n: 1 },
                { f: 75, t: 65, w: 20, n: 2 },
            ],
            d: 5, // depth
        };

        const optimizedSize = JSON.stringify(optimizedEntry).length;
        console.log("\nOptimized structure:");
        console.log(`- Single entry: ~${optimizedSize} bytes`);
        console.log(
            `- 120,000 entries: ~${((optimizedSize * 120000) / 1024 / 1024).toFixed(2)} MB`,
        );
        console.log(
            `- With overhead: ~${((optimizedSize * 120000 * 1.3) / 1024 / 1024).toFixed(2)} MB`,
        );

        expect(optimizedSize).toBeLessThan(300); // Should be much smaller
    });

    it("should measure actual memory usage of current implementation", () => {
        const positions = new Map<string, OpeningEntry>();
        const testSize = 1000; // Test with 1000 entries

        // Generate test entries
        for (let i = 0; i < testSize; i++) {
            const entry: OpeningEntry = {
                position: `position_${i}_lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL`,
                moves: [
                    {
                        move: {
                            type: "move",
                            from: { row: 7, column: 7 },
                            to: { row: 6, column: 7 },
                            piece: { type: "pawn", owner: "black", promoted: false },
                            promote: false,
                            captured: null,
                        },
                        weight: 40,
                        name: "居飛車",
                        comment: "最も一般的な初手",
                    },
                ],
                depth: Math.floor(i / 100),
            };
            positions.set(entry.position, entry);
        }

        // Rough estimation based on serialized size
        const serializedSize = JSON.stringify(Array.from(positions.entries())).length;
        const perEntrySize = serializedSize / testSize;

        console.log("\nActual implementation test:");
        console.log(
            `- ${testSize} entries serialized size: ${(serializedSize / 1024).toFixed(2)} KB`,
        );
        console.log(`- Per entry: ~${perEntrySize.toFixed(0)} bytes`);
        console.log(
            `- 120,000 entries estimated: ~${((perEntrySize * 120000) / 1024 / 1024).toFixed(2)} MB`,
        );

        expect(positions.size).toBe(testSize);
    });
});
