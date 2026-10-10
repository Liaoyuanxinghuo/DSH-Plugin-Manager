import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(),
  open: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { COMPLEMENT_INBOX, diffAgainstCurrent } from "./ComplementDialog";

describe("插件补全差集", () => {
  it("同名无论版本异同都跳过，只补完全缺失", () => {
    const r = diffAgainstCurrent(
      { keep: "1.0.0", newer: "^2.0.0" },
      [],
      new Set(["keep", "newer"]),
    );
    expect(r.missing).toEqual([]);
    expect(r.skippedSame).toBe(2);
  });

  it("基础包永不参与", () => {
    for (const name of COMPLEMENT_INBOX) {
      const r = diffAgainstCurrent({ [name]: "1.0.0" }, [], new Set());
      expect(r.missing).toEqual([]);
      expect(r.skippedInbox).toBe(1);
    }
  });

  it("缺失项按名称排序并标记 bundle", () => {
    const r = diffAgainstCurrent(
      { "z-pkg": "1.0.0", "a-pkg": "2.0.0" },
      ["a-pkg"],
      new Set(),
    );
    expect(r.missing.map((m) => m.name)).toEqual(["a-pkg", "z-pkg"]);
    expect(r.missing[0].isBundle).toBe(true);
    expect(r.missing[1].isBundle).toBe(false);
  });
});
