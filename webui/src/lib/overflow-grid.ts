/** 表格成员列双栏行数；超出时最后一格是 +N。 */
export const OVERFLOW_GRID_COLUMNS = 2;
export const OVERFLOW_GRID_ROWS = 3;
export const OVERFLOW_GRID_SLOTS = OVERFLOW_GRID_COLUMNS * OVERFLOW_GRID_ROWS;

/** 满格时露出 `slots - 1` 项，其余进 +N；未满则全部露出。 */
export function overflowGridItems<T>(
  items: readonly T[],
  slots = OVERFLOW_GRID_SLOTS,
): { visible: readonly T[]; hidden: readonly T[] } {
  if (items.length <= slots) {
    return { visible: items, hidden: [] };
  }
  return {
    visible: items.slice(0, slots - 1),
    hidden: items.slice(slots - 1),
  };
}
