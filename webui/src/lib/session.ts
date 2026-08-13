import { ref } from 'vue';

const STORAGE_KEY = 'kairos-admin-key';

const adminKey = ref<string | null>(readStored());

let onInvalidated: (() => void) | undefined;

function readStored(): string | null {
  return localStorage.getItem(STORAGE_KEY);
}

/** 当前持有的 admin key；未登录为 `null`。 */
export function getAdminKey(): string | null {
  return adminKey.value;
}

/** 是否已在本地持有 admin key。在 computed 内调用可追踪响应式。 */
export function hasAdminKey(): boolean {
  return Boolean(adminKey.value);
}

/** 登录成功后写入 localStorage。 */
export function setAdminKey(key: string): void {
  localStorage.setItem(STORAGE_KEY, key);
  adminKey.value = key;
}

/** 退出：清除本地凭据。 */
export function clearAdminKey(): void {
  localStorage.removeItem(STORAGE_KEY);
  adminKey.value = null;
}

/** 注册凭据失效回调（401 时跳转登录页）。 */
export function onAdminKeyInvalidated(callback: () => void): void {
  onInvalidated = callback;
}

/** API 返回 401 时清除凭据并通知路由。 */
export function invalidateAdminKey(): void {
  clearAdminKey();
  onInvalidated?.();
}
