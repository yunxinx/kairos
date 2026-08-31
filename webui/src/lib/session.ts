import { computed, ref } from 'vue';
import { queryClient } from '@/app/providers/query';
import type { MeView } from '@/api/types';

const isSessionActive = ref(false);
const currentUser = ref<MeView | null>(null);
const sessionGeneration = ref(0);

let onInvalidated: (() => void) | undefined;

/** 当前页面是否已经确认存在有效的 Cookie 会话。 */
export function hasSession(): boolean {
  return isSessionActive.value;
}

/** 登录或会话恢复成功后建立新的身份边界。 */
export function markSessionActive(): void {
  if (!isSessionActive.value) {
    resetIdentityBoundary();
  }
  isSessionActive.value = true;
}

/** 退出：清除当前页面的会话投影与用户数据。 */
export function clearSession(): void {
  resetIdentityBoundary();
  isSessionActive.value = false;
}

/**
 * 身份切换边界：销毁所有旧主体查询与 mutation，并让旧异步结果失去写会话资格。
 */
function resetIdentityBoundary(): void {
  queryClient.clear();
  currentUser.value = null;
  sessionGeneration.value += 1;
}

/** 注册会话失效回调（401 时跳转登录页）。 */
export function onSessionInvalidated(callback: () => void): void {
  onInvalidated = callback;
}

/** 仅让发起于当前身份代次的 401 使会话失效。 */
export function invalidateSession(generation: number): boolean {
  if (generation !== sessionGeneration.value || !isSessionActive.value) return false;
  clearSession();
  onInvalidated?.();
  return true;
}

/** 当前登录用户；尚未 hydrate 时为 `null`。 */
export function getMe(): MeView | null {
  return currentUser.value;
}

/** 捕获异步请求发起时的身份代次。 */
export function captureSessionGeneration(): number {
  return sessionGeneration.value;
}

/** 仅允许当前身份发起的异步请求回写用户投影。 */
export function setMeForSession(user: MeView, generation: number): boolean {
  if (generation !== sessionGeneration.value || !isSessionActive.value) return false;
  currentUser.value = user;
  return true;
}

/** 响应式身份代次，供确实需要感知主体变化的组合式逻辑使用。 */
export function useSessionGeneration() {
  return computed(() => sessionGeneration.value);
}

/** 响应式当前用户，供导航按角色过滤。 */
export function useCurrentUser() {
  return computed(() => currentUser.value);
}
