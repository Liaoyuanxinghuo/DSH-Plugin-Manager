// 列表拖动排序工具（纯函数，便于单元测试）

/** 读取 localStorage 中的顺序数组（非法 JSON 返回空） */
export function readStoredOrder(key: string): string[] {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return [];
    const v = JSON.parse(raw);
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** 将列表按存储顺序重排；未记录的新项保持原相对顺序追加到末尾 */
export function applyStoredOrder<T>(list: T[], getId: (x: T) => string, stored: string[]): T[] {
  if (!stored || stored.length === 0 || list.length === 0) return list;
  const map = new Map<string, T>();
  for (const x of list) map.set(getId(x), x);
  const ordered: T[] = [];
  for (const id of stored) {
    const item = map.get(id);
    if (item) {
      ordered.push(item);
      map.delete(id);
    }
  }
  // 保持原相对顺序的新项
  const rest = list.filter((x) => map.has(getId(x)));
  return [...ordered, ...rest];
}

/** 把 from 位置的元素移动到 to 位置，返回新数组 */
export function moveItem<T>(list: T[], from: number, to: number): T[] {
  if (from === to || from < 0 || to < 0 || from >= list.length || to >= list.length) return [...list];
  const arr = [...list];
  const [x] = arr.splice(from, 1);
  arr.splice(to, 0, x);
  return arr;
}
