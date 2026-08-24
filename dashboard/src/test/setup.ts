import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Vitest globals are off, so RTL's automatic cleanup never registers and
// renders would leak between tests.
afterEach(() => {
  cleanup();
});

