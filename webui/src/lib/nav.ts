import { roleAtLeast, type ManagementRole } from '@/api/types';

export interface NavTab {
  to: string;
  labelKey: string;
  minRole: ManagementRole;
}

/** 管理面导航。管理 API 在 `/api` 下，SPA 独占根命名空间，路径直接用领域词。 */
export const NAV_TABS: NavTab[] = [
  { to: '/overview', labelKey: 'nav.overview', minRole: 'user' },
  { to: '/tokens', labelKey: 'nav.tokens', minRole: 'user' },
  { to: '/channels', labelKey: 'nav.channel', minRole: 'root' },
  { to: '/models', labelKey: 'nav.models', minRole: 'admin' },
  { to: '/users', labelKey: 'nav.users', minRole: 'admin' },
  { to: '/logs', labelKey: 'nav.logs', minRole: 'user' },
  { to: '/settings', labelKey: 'nav.settings', minRole: 'root' },
];

/** 按角色过滤可见导航。未知角色时不展示，避免闪出越权入口。 */
export function navTabsFor(role: ManagementRole | undefined): NavTab[] {
  if (!role) return [];
  return NAV_TABS.filter((tab) => roleAtLeast(role, tab.minRole));
}
