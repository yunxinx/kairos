import { computed, ref } from 'vue';
import type { MeView } from '@/api/types';

/** 历史键名未改，避免已登录浏览器被踢下线；值已是 `ksess_…` 会话，不是登录口令。 */
const STORAGE_KEY = 'kairos-admin-key';

const adminKey = ref<string | null>(readStored());
const currentUser = ref<MeView | null>(null);

let onInvalidated: (() => void) | undefined;

function readStored(): string | null {
  return localStorage.getItem(STORAGE_KEY);
}

/** 当前持有的管理会话令牌（`ksess_…`）；未登录为 `null`。 */
export function getAdminKey(): string | null {
  return adminKey.value;
}

/** 是否已在本地持有管理凭证。在 computed 内调用可追踪响应式。 */
export function hasAdminKey(): boolean {
  return Boolean(adminKey.value);
}

/** 登录成功后写入 localStorage。 */
export function setAdminKey(key: string): void {
  localStorage.setItem(STORAGE_KEY, key);
  adminKey.value = key;
}

/** 退出：清除本地凭据与当前用户。 */
export function clearAdminKey(): void {
  localStorage.removeItem(STORAGE_KEY);
  adminKey.value = null;
  currentUser.value = null;
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

/** 当前登录用户；尚未 hydrate 时为 `null`。 */
export function getMe(): MeView | null {
  return currentUser.value;
}

/** 写入 `/me` 结果，供导航与守卫同步。 */
export function setMe(user: MeView | null): void {
  currentUser.value = user;
}

/** 响应式当前用户，供导航按角色过滤。 */
export function useCurrentUser() {
  return computed(() => currentUser.value);
}
