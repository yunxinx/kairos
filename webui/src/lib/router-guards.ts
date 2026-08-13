import { redirect } from '@tanstack/vue-router';
import { getAdminKey } from '@/lib/session';

/** 受保护页面：无 admin key 则去登录。 */
export function requireAuth(): void {
  if (!getAdminKey()) {
    throw redirect({ to: '/login' });
  }
}

/** 登录页：已持有 key 则进入控制台。 */
export function requireGuest(): void {
  if (getAdminKey()) {
    throw redirect({ to: '/overview' });
  }
}
