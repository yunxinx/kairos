import { computed } from 'vue';
import { hasSession } from '@/lib/session';

/**
 * 错误/404 页「返回安全路由」：已登录去概览，否则去营销首页。
 */
export function useSafeHomeTarget() {
  return computed(() => (hasSession() ? '/overview' : '/'));
}
