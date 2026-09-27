import { describe, it, expect, beforeEach } from "vitest";
import { readStoredOrder, applyStoredOrder, moveItem } from "./reorder";

beforeEach(() => {
  localStorage.clear();
});

describe("readStoredOrder", () => {
  it("空/非法 JSON 返回空数组", () => {
    expect(readStoredOrder("k")).toEqual([]);
    localStorage.setItem("k", "{bad");
    expect(readStoredOrder("k")).toEqual([]);
  });
  it("合法数组返回字符串项", () => {
    localStorage.setItem("k", JSON.stringify(["a", "b"]));
    expect(readStoredOrder("k")).toEqual(["a", "b"]);
  });
  it("过滤非字符串项", () => {
    localStorage.setItem("k", JSON.stringify(["a", 2, null]));
    expect(readStoredOrder("k")).toEqual(["a"]);
  });
});

describe("applyStoredOrder", () => {
  const list = [
    { id: "a", v: 1 },
    { id: "b", v: 2 },
    { id: "c", v: 3 },
  ];
  it("按存储顺序重排", () => {
    const out = applyStoredOrder(list, (x) => x.id, ["c", "a", "b"]);
    expect(out.map((x) => x.id)).toEqual(["c", "a", "b"]);
  });
  it("部分匹配：未知 id 忽略，新项追加到末尾", () => {
    const out = applyStoredOrder(list, (x) => x.id, ["c", "zzz", "a"]);
    expect(out.map((x) => x.id)).toEqual(["c", "a", "b"]);
  });
  it("空存储原样返回", () => {
    expect(applyStoredOrder(list, (x) => x.id, [])).toEqual(list);
    expect(applyStoredOrder<{ id: string; v: number }>([], (x) => x.id, ["a"])).toEqual([]);
  });
});

describe("moveItem", () => {
  it("向后移动", () => {
    expect(moveItem(["a", "b", "c"], 0, 2)).toEqual(["b", "c", "a"]);
  });
  it("向前移动", () => {
    expect(moveItem(["a", "b", "c"], 2, 0)).toEqual(["c", "a", "b"]);
  });
  it("越界/同位置返回副本", () => {
    expect(moveItem(["a", "b"], 0, 0)).toEqual(["a", "b"]);
    expect(moveItem(["a", "b"], 0, 5)).toEqual(["a", "b"]);
    expect(moveItem(["a", "b"], -1, 1)).toEqual(["a", "b"]);
  });
});
