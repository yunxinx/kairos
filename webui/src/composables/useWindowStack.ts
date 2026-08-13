import { computed, onScopeDispose, ref, type ComputedRef, type Ref } from 'vue';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

/** 单页浮窗上限；超出时按打开顺序淘汰最旧窗口。 */
export const MAX_FLOATING_WINDOWS = 5;

export interface WindowStackEntry<T> {
  id: number;
  /** 层叠序号，越大越靠前；渲染时叠加在 --z-window 之上。 */
  z: number;
  /** 草稿是否有未保存更改，决定淘汰时能否直接关闭。 */
  dirty: boolean;
  /** 淘汰被阻止时的短暂提示动画。 */
  attention: boolean;
  anchor: FloatingWindowAnchor | null;
  payload: T;
}

/**
 * 浮窗栈：按打开顺序维护窗口列表，支持置顶、脏检查与 FIFO 淘汰。
 * 打开第 6 个窗口时，最旧窗口干净则直接关闭腾位；脏则置顶并提示，
 * 打开请求被拒绝，由用户处理旧窗口后再次触发。
 */
export function useWindowStack<T>(): {
  windows: Ref<WindowStackEntry<T>[]>;
  topmostId: ComputedRef<number | null>;
  open: (anchor: FloatingWindowAnchor | null, payload: T) => WindowStackEntry<T> | null;
  close: (id: number) => void;
  setDirty: (id: number, dirty: boolean) => void;
  bringToFront: (id: number) => void;
} {
  const windows = ref<WindowStackEntry<T>[]>([]) as Ref<WindowStackEntry<T>[]>;
  let nextId = 1;
  let nextZ = 1;
  const attentionTimers = new Map<number, ReturnType<typeof setTimeout>>();

  const topmostId = computed<number | null>(() => {
    let top: WindowStackEntry<T> | null = null;
    for (const entry of windows.value) {
      if (top === null || entry.z > top.z) top = entry;
    }
    return top?.id ?? null;
  });

  function findEntry(id: number): WindowStackEntry<T> | undefined {
    return windows.value.find((entry) => entry.id === id);
  }

  /** 层叠序号压缩为 1..n，窗口 z-index（--z-window + z）因此恒低于 --z-popover 弹层。 */
  function normalizeZOrder(): void {
    const sorted = [...windows.value].sort((a, b) => a.z - b.z);
    sorted.forEach((entry, index) => {
      entry.z = index + 1;
    });
  }

  function bringToFront(id: number): void {
    const entry = findEntry(id);
    if (!entry) return;
    entry.z = nextZ++;
    normalizeZOrder();
  }

  function close(id: number): void {
    clearTimeout(attentionTimers.get(id));
    attentionTimers.delete(id);
    windows.value = windows.value.filter((entry) => entry.id !== id);
  }

  function setDirty(id: number, dirty: boolean): void {
    const entry = findEntry(id);
    if (entry) entry.dirty = dirty;
  }

  function open(anchor: FloatingWindowAnchor | null, payload: T): WindowStackEntry<T> | null {
    if (windows.value.length >= MAX_FLOATING_WINDOWS) {
      const oldest = windows.value[0];
      if (!oldest) return null;
      if (oldest.dirty) {
        bringToFront(oldest.id);
        oldest.attention = true;
        clearTimeout(attentionTimers.get(oldest.id));
        attentionTimers.set(
          oldest.id,
          setTimeout(() => {
            oldest.attention = false;
            attentionTimers.delete(oldest.id);
          }, 900),
        );
        return null;
      }
      close(oldest.id);
    }
    const entry: WindowStackEntry<T> = {
      id: nextId++,
      z: nextZ++,
      dirty: false,
      attention: false,
      anchor,
      payload,
    };
    windows.value.push(entry);
    normalizeZOrder();
    return entry;
  }

  onScopeDispose(() => {
    for (const timer of attentionTimers.values()) clearTimeout(timer);
    attentionTimers.clear();
  });

  return { windows, topmostId, open, close, setDirty, bringToFront };
}
