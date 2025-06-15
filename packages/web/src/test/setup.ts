import "@testing-library/jest-dom";

// Clean up after each test
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => {
    cleanup();
});
