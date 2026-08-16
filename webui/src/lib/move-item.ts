/** 在有序列表中移动一项；越界时原样返回。 */
export function moveItem<T>(items: readonly T[], from: number, to: number): T[] {
  if (from < 0 || from >= items.length || to < 0 || to >= items.length || from === to) {
    return [...items];
  }
  const next = [...items];
  const [removed] = next.splice(from, 1);
  if (removed === undefined) return [...items];
  next.splice(to, 0, removed);
  return next;
}
