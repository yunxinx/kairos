/** 模型清单的排序与比较：编辑器 chip、同步表格与保存载荷共用同一顺序约定。 */

/** 模型自然顺序：数字段按数值比较（如 gpt-9 排在 gpt-10 前）。 */
export function compareModels(left: string, right: string): number {
  return left.localeCompare(right, undefined, { numeric: true, sensitivity: 'base' });
}

/** 两个模型集合是否相同（不看顺序），用于脏状态判定。 */
export function sameModelSet(left: string[], right: string[]): boolean {
  if (left.length !== right.length) return false;
  const sortedLeft = [...left].sort();
  const sortedRight = [...right].sort();
  return sortedLeft.every((model, index) => model === sortedRight[index]);
}

/** 两个别名映射是否相同（不看键序），用于脏状态判定。 */
export function sameAliasMap(left: Record<string, string>, right: Record<string, string>): boolean {
  const leftKeys = Object.keys(left);
  if (leftKeys.length !== Object.keys(right).length) return false;
  return leftKeys.every((alias) => Object.hasOwn(right, alias) && right[alias] === left[alias]);
}

/** 连通性探测表格的一行：展示名与出站主模型名。 */
export interface ProbeModelRow {
  displayName: string;
  probeModel: string;
}

/**
 * 从清单去重出探测行：按主模型名分组。
 * 主模型名在清单则只展示主模型名；仅别名在清单则展示清单中出现的第一个别名。
 */
export function probeModelRows(models: string[], aliases: Record<string, string>): ProbeModelRow[] {
  const canonicalOf = (name: string): string => aliases[name] ?? name;
  const groups = new Map<string, string[]>();
  for (const name of models) {
    const canonical = canonicalOf(name);
    const list = groups.get(canonical);
    if (list) {
      list.push(name);
    } else {
      groups.set(canonical, [name]);
    }
  }
  const rows: ProbeModelRow[] = [];
  for (const [canonical, names] of groups) {
    const displayName = names.includes(canonical) ? canonical : (names[0] ?? canonical);
    rows.push({ displayName, probeModel: canonical });
  }
  return rows;
}
