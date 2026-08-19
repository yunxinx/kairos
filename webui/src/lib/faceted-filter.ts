/** 分面筛选一项：选中值、展示文案、可选计数。 */
export interface FacetedFilterOption {
  value: string;
  label: string;
  count?: number;
}

/** 按出现次数聚合成筛选选项；value 与 label 同为原字符串。 */
export function countedFacetOptions(values: Iterable<string>): FacetedFilterOption[] {
  const counts = new Map<string, number>();
  for (const value of values) {
    counts.set(value, (counts.get(value) ?? 0) + 1);
  }
  return [...counts.entries()]
    .sort((left, right) => left[0].localeCompare(right[0]))
    .map(([value, count]) => ({ value, label: value, count }));
}
