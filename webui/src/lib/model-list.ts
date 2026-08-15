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
