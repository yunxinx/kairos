import { computed, ref, watch, type Ref } from 'vue';

/** 一列的显隐规则：锁定列不可关，也不进「显示列」列表。 */
export type ColumnVisibilitySpec<Id extends string> = {
  id: Id;
  locked?: boolean;
  /** 缺省可见；锁定列始终可见。 */
  defaultVisible?: boolean;
};

export type ColumnVisibilityItem<Id extends string> = {
  id: Id;
  checked: boolean;
  /** 再关掉就会只剩时间/操作时为 true。 */
  disabled: boolean;
};

/**
 * 表格列显隐：锁定列恒显，可关列写入 localStorage。
 * 至少保留一列非锁定数据，避免表体只剩时间与操作。
 */
export function useColumnVisibility<Id extends string>(
  storageKey: string,
  specs: readonly ColumnVisibilitySpec<Id>[],
) {
  const dataIds = specs.filter((spec) => !spec.locked).map((spec) => spec.id);

  function defaults(): Record<Id, boolean> {
    const next = {} as Record<Id, boolean>;
    for (const spec of specs) {
      next[spec.id] = spec.locked ? true : (spec.defaultVisible ?? true);
    }
    return next;
  }

  function dataVisibleCount(map: Record<Id, boolean>): number {
    let count = 0;
    for (const id of dataIds) {
      if (map[id]) count += 1;
    }
    return count;
  }

  function load(): Record<Id, boolean> {
    const next = defaults();
    try {
      const raw = localStorage.getItem(storageKey);
      if (!raw) return next;
      const parsed = JSON.parse(raw) as unknown;
      if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
        return next;
      }
      const rec = parsed as Record<string, unknown>;
      for (const spec of specs) {
        if (spec.locked) {
          next[spec.id] = true;
          continue;
        }
        const stored = rec[spec.id];
        if (typeof stored === 'boolean') {
          next[spec.id] = stored;
        }
      }
    } catch {
      return defaults();
    }
    if (dataVisibleCount(next) === 0) {
      return defaults();
    }
    return next;
  }

  const state = ref(load()) as Ref<Record<Id, boolean>>;

  function persist() {
    const payload: Partial<Record<Id, boolean>> = {};
    for (const spec of specs) {
      if (!spec.locked) {
        payload[spec.id] = state.value[spec.id];
      }
    }
    localStorage.setItem(storageKey, JSON.stringify(payload));
  }

  watch(state, persist, { deep: true });

  const visible = computed(() => state.value);

  const columnCount = computed(() => {
    let count = 0;
    for (const spec of specs) {
      if (state.value[spec.id]) count += 1;
    }
    return count;
  });

  function isLastDataColumn(id: Id): boolean {
    return state.value[id] && dataVisibleCount(state.value) <= 1;
  }

  function setVisible(id: Id, next: boolean) {
    const spec = specs.find((item) => item.id === id);
    if (!spec || spec.locked) return;
    if (!next && isLastDataColumn(id)) return;
    if (state.value[id] === next) return;
    const nextState = { ...state.value };
    nextState[id] = next;
    state.value = nextState;
  }

  function hide(id: Id) {
    setVisible(id, false);
  }

  function menuItems(ids: readonly Id[]): ColumnVisibilityItem<Id>[] {
    return ids.map((id) => ({
      id,
      checked: state.value[id],
      disabled: isLastDataColumn(id),
    }));
  }

  return {
    visible,
    columnCount,
    setVisible,
    hide,
    isLastDataColumn,
    menuItems,
  };
}
