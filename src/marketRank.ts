// 市场搜索排序：名称匹配度优先，star 次之
export type MarketRankItem = {
  name: string;
  descriptionZh?: string;
  descriptionEn?: string;
  stars?: number | null;
};

/** 匹配档位（越小越靠前）：0 精确 / 1 后缀 / 2 词段 / 3 名称包含 / 4 仅描述 / 5 不匹配 */
export function nameMatchRank(name: string, keyword: string): number {
  const n = name.toLowerCase();
  const k = keyword.toLowerCase().trim();
  if (!k) return 5;
  if (n === k) return 0;
  if (n.endsWith(`-${k}`) || n.endsWith(`_${k}`) || n.endsWith(`/${k}`)) return 1;
  if (
    n.includes(`-${k}`) ||
    n.includes(`_${k}`) ||
    n.includes(`/${k}`) ||
    n.startsWith(`${k}-`) ||
    n.startsWith(`${k}_`) ||
    n.startsWith(`${k}/`)
  ) {
    return 2;
  }
  if (n.includes(k)) return 3;
  return 5;
}

function descHit(item: MarketRankItem, k: string): boolean {
  return (
    (item.descriptionZh ?? "").toLowerCase().includes(k) ||
    (item.descriptionEn ?? "").toLowerCase().includes(k)
  );
}

/**
 * 市场检索：先过滤，再排序。
 * - 无关键词：全部保留，按 star 降序
 * - 有关键词：名称/描述命中才保留；**名称匹配档优先**，同档再 star
 */
export function filterAndSortMarket<T extends MarketRankItem>(
  list: T[],
  keyword: string,
): T[] {
  const k = keyword.trim().toLowerCase();
  if (!k) {
    return [...list].sort((a, b) => (b.stars ?? 0) - (a.stars ?? 0));
  }
  const matched = list.filter((p) => nameMatchRank(p.name, k) < 5 || descHit(p, k));
  return matched.sort((a, b) => {
    const ra = nameMatchRank(a.name, k) < 5 ? nameMatchRank(a.name, k) : 4;
    const rb = nameMatchRank(b.name, k) < 5 ? nameMatchRank(b.name, k) : 4;
    if (ra !== rb) return ra - rb;
    return (b.stars ?? 0) - (a.stars ?? 0);
  });
}

/** 兼容旧签名 */
export function sortMarketHits<T extends MarketRankItem>(list: T[], keyword: string): T[] {
  return filterAndSortMarket(list, keyword);
}

