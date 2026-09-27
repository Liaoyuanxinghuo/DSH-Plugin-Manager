import { describe, expect, it } from "vitest";
import { formatSize } from "./api";

describe("formatSize", () => {
  it("格式化字节", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(512)).toBe("512 B");
  });

  it("格式化 KB", () => {
    expect(formatSize(2048)).toBe("2.0 KB");
    expect(formatSize(1024)).toBe("1.0 KB");
  });

  it("格式化 MB", () => {
    expect(formatSize(1024 * 1024)).toBe("1.0 MB");
    expect(formatSize(5 * 1024 * 1024)).toBe("5.0 MB");
  });

  it("格式化 GB", () => {
    expect(formatSize(2 * 1024 * 1024 * 1024)).toBe("2.00 GB");
  });
});
