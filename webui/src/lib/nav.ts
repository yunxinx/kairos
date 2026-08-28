import type { MeView, ManagementRole, PlanCapabilities } from '@/api/types';
import { roleAtLeast } from '@/api/types';
import { hasCapability, type ManagementCapability } from '@/lib/capabilities';

export interface NavTab {
  to: string;
  labelKey: string;
  minRole: ManagementRole;
  /** 管理面能力要求；root 自动通过，普通用户按导航自身语义处理。 */
  capability?: ManagementCapability;
  /** 普通用户是否总是可见（自助能力，不属于管理面收窄范围）。 */
  userSelfService?: boolean;
}

/** 管理面导航。管理 API 在 `/api` 下，SPA 独占根命名空间，路径直接用领域词。 */
export const NAV_TABS: NavTab[] = [
  { to: '/overview', labelKey: 'nav.overview', minRole: 'user', userSelfService: true },
  { to: '/tokens', labelKey: 'nav.tokens', minRole: 'user', userSelfService: true },
  { to: '/channels', labelKey: 'nav.channel', minRole: 'root' },
  // 普通用户看到的是另一张表（自己能调什么、按什么价收），不是管理视图；
  // 入口共用一条导航项，由页面按角色分流。
  {
    to: '/models',
    labelKey: 'nav.models',
    minRole: 'user',
    userSelfService: true,
  },
  { to: '/plans', labelKey: 'nav.plans', minRole: 'root' },
  { to: '/users', labelKey: 'nav.users', minRole: 'admin', capability: 'manage_users' },
  {
    to: '/logs',
    labelKey: 'nav.logs',
    minRole: 'user',
    capability: 'view_logs_stats',
    userSelfService: true,
  },
  { to: '/settings', labelKey: 'nav.settings', minRole: 'root' },
];

/** 按生效能力过滤可见导航。未知用户时不展示，避免闪出越权入口。 */
export function navTabsFor(me: MeView | null | undefined): NavTab[] {
  if (!me) return [];
  return NAV_TABS.filter((tab) => {
    if (!roleAtLeast(me.role, tab.minRole)) return false;
    if (tab.to === '/logs' && me.role === 'user') return true;
    if (tab.capability && !hasCapability(me, tab.capability)) return false;
    return true;
  });
}

export type { PlanCapabilities };
