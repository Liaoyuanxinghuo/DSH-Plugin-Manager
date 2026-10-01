import { describe, expect, it } from "vitest";
import { filterAndSortMarket, nameMatchRank } from "./marketRank";

describe("市场搜索排序", () => {
  it("搜 codex-ui 时 dsh-codex-ui 应在最上，无关高星不出现", () => {
    const list = [
      { name: "dsh-cost-meter", stars: 344, descriptionZh: "会话与当日 API 费用统计" },
      { name: "dsh-memory", stars: 278, descriptionZh: "白箱AGI架构探索" },
      { name: "dsh-codex-subscription", stars: 103, descriptionZh: "通过 ChatGPT OAuth 在 DSH 中使用 Codex 模型" },
      { name: "dsh-codex-ui", stars: 103, descriptionZh: "为 DeepSeek Harness 网页端重构 Codex 风格侧栏" },
    ];
    const sorted = filterAndSortMarket(list, "codex-ui");
    // 无关插件被过滤掉
    expect(sorted.map((x) => x.name)).not.toContain("dsh-cost-meter");
    expect(sorted.map((x) => x.name)).not.toContain("dsh-memory");
    // dsh-codex-ui 名称包含 codex-ui，应在最上
    expect(sorted[0]?.name).toBe("dsh-codex-ui");
  });

  it("名称匹配优先于更高 star", () => {
    const list = [
      { name: "aaa-high-star", stars: 99999, descriptionZh: "提到 codex-ui 的说明" },
      { name: "dsh-codex-ui", stars: 41184 },
    ];
    const sorted = filterAndSortMarket(list, "codex-ui");
    expect(sorted[0].name).toBe("dsh-codex-ui");
  });

  it("无关键词时按 star 降序", () => {
    const list = [
      { name: "a", stars: 1 },
      { name: "b", stars: 9 },
      { name: "c", stars: 5 },
    ];
    expect(filterAndSortMarket(list, "").map((x) => x.name)).toEqual(["b", "c", "a"]);
  });

  it("档位：精确 < 后缀 < 词段 < 包含", () => {
    expect(nameMatchRank("codex-ui", "codex-ui")).toBe(0);
    expect(nameMatchRank("dsh-codex-ui", "codex-ui")).toBe(1);
    expect(nameMatchRank("codex-ui-kit", "codex-ui")).toBe(2);
    expect(nameMatchRank("mycodex-uiX", "codex-ui")).toBe(3);
    expect(nameMatchRank("unrelated", "codex-ui")).toBe(5);
  });
});
