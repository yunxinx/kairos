import { redirect } from '@tanstack/vue-router';
import { apiClient } from '@/api/client';
import { roleAtLeast, type ManagementRole } from '@/api/types';
import { hasCapability, type ManagementCapability } from '@/lib/capabilities';
import { getAdminKey, getMe, setMe } from '@/lib/session';

/** 拉一次 `/me` 填进会话；已有则跳过。 */
export async function ensureMe(): Promise<void> {
  if (!getAdminKey()) return;
  if (getMe()) return;
  setMe(await apiClient.getMe());
}

/** 受保护页面：无管理凭证则去登录。 */
export async function requireAuth(): Promise<void> {
  if (!getAdminKey()) {
    throw redirect({ to: '/login' });
  }
  try {
    await ensureMe();
  } catch {
    throw redirect({ to: '/login' });
  }
}

/** 登录页：已持有凭证则进入控制台。 */
export function requireGuest(): void {
  if (getAdminKey()) {
    throw redirect({ to: '/overview' });
  }
}

/** 角色不足则回概览。 */
export function requireRole(min: ManagementRole) {
  return async () => {
    await requireAuth();
    const me = getMe();
    if (!me || !roleAtLeast(me.role, min)) {
      throw redirect({ to: '/overview' });
    }
  };
}

/** 生效能力不足则回概览（root 自动通过）。 */
export function requireCapability(capability: ManagementCapability) {
  return async () => {
    await requireAuth();
    const me = getMe();
    if (!me || !hasCapability(me, capability)) {
      throw redirect({ to: '/overview' });
    }
  };
}

/** 任一管理面能力满足即可进入（root 自动通过）。 */
export function requireAnyCapability(capabilities: ManagementCapability[]) {
  return async () => {
    await requireAuth();
    const me = getMe();
    if (!me) throw redirect({ to: '/overview' });
    if (me.role === 'root') return;
    if (me.role === 'admin' && capabilities.some((cap) => me.capabilities[cap])) return;
    throw redirect({ to: '/overview' });
  };
}
