import { computed, ref, type ComputedRef, type Ref } from 'vue';

export interface RowSelection<K extends string | number> {
  /** 当前选中键集合（只读）。 */
  selected: Readonly<Ref<ReadonlySet<K>>>;
  /** 选中数量。 */
  count: ComputedRef<number>;
  isSelected(key: K): boolean;
  /** 切换单行选中态。 */
  toggle(key: K): void;
  /** 将给定键批量置为选中或取消；空集合不动，避免无谓的响应式触发。 */
  setMany(keys: readonly K[], selected: boolean): void;
  clear(): void;
  /** 剔除已不在列表中的选中键（删除或过滤数据源后避免幽灵选择）。 */
  prune(existing: readonly K[]): void;
}

/** 表格行选择状态：选中键以集合维护，全选只作用于调用方给定的可见行。 */
export function useRowSelection<K extends string | number>(): RowSelection<K> {
  const selected = ref<Set<K>>(new Set()) as Ref<Set<K>>;

  function isSelected(key: K): boolean {
    return selected.value.has(key);
  }

  function toggle(key: K): void {
    const next = new Set(selected.value);
    if (next.has(key)) {
      next.delete(key);
    } else {
      next.add(key);
    }
    selected.value = next;
  }

  function setMany(keys: readonly K[], value: boolean): void {
    if (keys.length === 0) return;
    const next = new Set(selected.value);
    for (const key of keys) {
      if (value) next.add(key);
      else next.delete(key);
    }
    selected.value = next;
  }

  function clear(): void {
    if (selected.value.size > 0) selected.value = new Set();
  }

  function prune(existing: readonly K[]): void {
    if (selected.value.size === 0) return;
    const alive = new Set(existing);
    const next = new Set<K>();
    for (const key of selected.value) {
      if (alive.has(key)) next.add(key);
    }
    if (next.size !== selected.value.size) selected.value = next;
  }

  return {
    selected,
    count: computed(() => selected.value.size),
    isSelected,
    toggle,
    setMany,
    clear,
    prune,
  };
}
