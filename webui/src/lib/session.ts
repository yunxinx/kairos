import { computed, ref } from 'vue';
import { queryClient } from '@/app/providers/query';
import type { MeView } from '@/api/types';

const isSessionActive = ref(false);
const currentUser = ref<MeView | null>(null);
const sessionGeneration = ref(0);
/** 已知浏览器无管理会话（上一轮 /me 401）：登录页守卫跳过探测，消除噪音请求。 */
const knownLoggedOut = ref(false);
/** 各资源页浮窗栈声明的未保存工作注册表；键为声明方标识，值为当前是否有脏窗口。 */
const unsavedWork = ref(new Map<string, boolean>());

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
  knownLoggedOut.value = false;
}

/** 退出：清除当前页面的会话投影与用户数据。 */
export function clearSession(): void {
  resetIdentityBoundary();
  isSessionActive.value = false;
  knownLoggedOut.value = true;
}

/** 上一轮 /me 是否已确认无会话；登录页据此跳过注定 401 的探测。 */
export function hasKnownLoggedOut(): boolean {
  return knownLoggedOut.value;
}

/**
 * 声明本组件的未保存工作状态（浮窗栈 dirty 聚合）。
 *
 * 会话失效（401）跳转登录页前查询该注册表：仍有脏草稿时不静默跳转，
 * 由失效提示引导用户手动处理。返回停止函数，组件卸载即注销声明。
 */
export function declareUnsavedWork(source: string): {
  /** 更新本声明方的当前状态。 */
  set: (hasDirty: boolean) => void;
  /** 注销声明（组件卸载）。 */
  stop: () => void;
} {
  unsavedWork.value.set(source, false);
  return {
    set: (hasDirty: boolean) => unsavedWork.value.set(source, hasDirty),
    stop: () => unsavedWork.value.delete(source),
  };
}

/** 是否存在声明中的未保存工作；供会话失效路径查询。 */
export function hasUnsavedWork(): boolean {
  return [...unsavedWork.value.values()].some(Boolean);
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
