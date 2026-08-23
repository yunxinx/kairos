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
  { to: '/plans', labelKey: 'nav.plans', minRole: 'root' },
  { to: '/channels', labelKey: 'nav.channel', minRole: 'root' },
  {
    to: '/models',
    labelKey: 'nav.models',
    minRole: 'admin',
    capability: 'edit_prices',
  },
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

const MODEL_CAPABILITIES: ManagementCapability[] = [
  'edit_prices',
  'edit_model_groups',
  'edit_unified_models',
  'edit_price_catalog',
  'view_own_plan_groups',
  'view_other_groups',
];

function modelVisible(me: MeView): boolean {
  if (me.role === 'root') return true;
  if (me.role !== 'admin') return false;
  return MODEL_CAPABILITIES.some((cap) => me.capabilities[cap]);
}

/** 按生效能力过滤可见导航。未知用户时不展示，避免闪出越权入口。 */
export function navTabsFor(me: MeView | null | undefined): NavTab[] {
  if (!me) return [];
  return NAV_TABS.filter((tab) => {
    if (!roleAtLeast(me.role, tab.minRole)) return false;
    if (tab.to === '/models') return modelVisible(me);
    if (tab.to === '/logs' && me.role === 'user') return true;
    if (tab.capability && !hasCapability(me, tab.capability)) return false;
    return true;
  });
}

export type { PlanCapabilities };
