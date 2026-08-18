import { ref, type Ref } from 'vue';

/** 默认停留时长（毫秒）；指针悬停视口时由 Reka Toast 暂停倒计时。 */
export const TOAST_DURATION_MS = 3_000;

/** 离场动画时长；须与 `.toast-item[data-state='closed']` 动画对齐。 */
const TOAST_LEAVE_MS = 220;

/** 同时展开的条数上限；超出时先收起最早一条。 */
const MAX_OPEN_TOASTS = 5;

export type ToastKind = 'success' | 'danger';

export interface ToastItem {
  id: number;
  kind: ToastKind;
  message: string;
  open: boolean;
  duration: number;
}

const toasts: Ref<ToastItem[]> = ref([]);
let nextId = 1;
const removalTimers = new Map<number, ReturnType<typeof setTimeout>>();

function pushToast(kind: ToastKind, message: string, duration = TOAST_DURATION_MS): number {
  const openCount = toasts.value.reduce((count, item) => count + (item.open ? 1 : 0), 0);
  if (openCount >= MAX_OPEN_TOASTS) {
    const oldest = toasts.value.find((item) => item.open);
    if (oldest) dismissToast(oldest.id);
  }
  const id = nextId;
  nextId += 1;
  toasts.value = [...toasts.value, { id, kind, message, open: true, duration }];
  return id;
}

/** 关掉一条 Toast，并在离场动画结束后从列表移除。 */
export function dismissToast(id: number): void {
  const item = toasts.value.find((entry) => entry.id === id);
  if (item) item.open = false;
  removeClosedToast(id);
}

/** Toast 已进入关闭态后调用：等动画播完再卸掉 DOM。 */
export function removeClosedToast(id: number): void {
  if (removalTimers.has(id)) return;
  removalTimers.set(
    id,
    setTimeout(() => {
      toasts.value = toasts.value.filter((item) => item.id !== id);
      removalTimers.delete(id);
    }, TOAST_LEAVE_MS),
  );
}

/**
 * 全局 Toast 栈：模块单例，供任意 setup / 事件回调推送。
 * 成功与危险两条通道；复制走 `copy`，避免各处自管计时器。
 */
export function useToast(): {
  toasts: Ref<ToastItem[]>;
  success: (message: string) => number;
  error: (message: string) => number;
  dismiss: (id: number) => void;
  copy: (text: string, copiedMessage: string, failedMessage: string) => Promise<void>;
} {
  return {
    toasts,
    success: (message) => pushToast('success', message),
    error: (message) => pushToast('danger', message),
    dismiss: dismissToast,
    copy: async (text, copiedMessage, failedMessage) => {
      try {
        await navigator.clipboard.writeText(text);
        pushToast('success', copiedMessage);
      } catch {
        pushToast('danger', failedMessage);
      }
    },
  };
}
