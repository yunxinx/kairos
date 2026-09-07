import { ref } from 'vue';

/** 一列的拖拽调宽规则：无 `width` 的列不设宽，吃掉剩余宽度（与现有 colgroup 惝例一致）。 */
export type ColumnResizeSpec = {
  id: string;
  /** 调宽生效的列宽（像素）；省略表示该列不参与拖拽。 */
  width?: number;
  /** 拖拽下限；省略时取 width。 */
  minWidth?: number;
  /** 拖拽上限；省略时无上限。 */
  maxWidth?: number;
};

function clamp(width: number, spec: ColumnResizeSpec): number {
  const min = spec.minWidth ?? spec.width ?? width;
  const max = spec.maxWidth ?? Number.POSITIVE_INFINITY;
  return Math.min(Math.max(Math.round(width), min), max);
}

/**
 * 表格列拖拽调宽 + localStorage 持久化。存储形状为 `{ [id]: px }` 的 JSON：
 * 读回时逐列校验（数字、clamp 到上下限），列集变更（新增列、列显隐无关）后
 * 新列走默认宽，存量偏好继续生效。列宽拖动只改内存态，拖拽结束才落盘，
 * 避免高频写存储。
 *
 * 拖拽期间相邻列不联动（左列变宽，右列自然被压缩）：表格是 `table-layout:fixed`，
 * 未定宽列吸收余量，这与现有 colgroup 的分配约定一致。
 */
export function useColumnResize(storageKey: string, specs: readonly ColumnResizeSpec[]) {
  const resizableSpecs = specs.filter((spec) => spec.width !== undefined);

  function defaults(): Record<string, number> {
    const next: Record<string, number> = {};
    for (const spec of resizableSpecs) {
      next[spec.id] = spec.width as number;
    }
    return next;
  }

  function load(): Record<string, number> {
    const next = defaults();
    try {
      const raw = localStorage.getItem(storageKey);
      if (!raw) return next;
      const parsed: unknown = JSON.parse(raw);
      if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
        return next;
      }
      const rec = parsed as Record<string, unknown>;
      for (const spec of resizableSpecs) {
        const stored = rec[spec.id];
        if (typeof stored === 'number' && Number.isFinite(stored)) {
          next[spec.id] = clamp(stored, spec);
        }
      }
    } catch {
      return next;
    }
    return next;
  }

  const widths = ref(load());

  function persist() {
    try {
      localStorage.setItem(storageKey, JSON.stringify(widths.value));
    } catch {
      // 持久化是锦上添花：写不进去就当会话内状态用。
    }
  }

  function applyWidth(id: string, width: number): void {
    const spec = resizableSpecs.find((item) => item.id === id);
    if (!spec) return;
    const next = clamp(width, spec);
    if (next === widths.value[id]) return;
    widths.value = { ...widths.value, [id]: next };
  }

  function resize(id: string, width: number): void {
    applyWidth(id, width);
  }

  /** 拖拽/键盘调整结束（mouseup、键盘松开）时调用：此刻落盘。 */
  function commit(): void {
    persist();
  }

  function reset(id: string): void {
    const spec = resizableSpecs.find((item) => item.id === id);
    if (!spec) return;
    applyWidth(id, spec.width as number);
    persist();
  }

  /** 列显隐菜单「重置列宽」入口：全部回到默认并落盘。 */
  function resetAll(): void {
    widths.value = defaults();
    persist();
  }

  return {
    widths,
    resize,
    commit,
    reset,
    resetAll,
  };
}
