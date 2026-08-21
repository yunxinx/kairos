import { roleAtLeast, type ManagementRole } from '@/api/types';

export interface NavTab {
  to: string;
  labelKey: string;
  minRole: ManagementRole;
}

/** 管理面导航。路径避开资源 API（`/tokens` `/channels` `/settings` `/logs` `/users`）。 */
export const NAV_TABS: NavTab[] = [
  { to: '/overview', labelKey: 'nav.overview', minRole: 'user' },
  { to: '/token', labelKey: 'nav.tokens', minRole: 'user' },
  { to: '/channel', labelKey: 'nav.channel', minRole: 'root' },
  { to: '/models', labelKey: 'nav.models', minRole: 'admin' },
  { to: '/admin/users', labelKey: 'nav.users', minRole: 'admin' },
  { to: '/requests', labelKey: 'nav.logs', minRole: 'user' },
  { to: '/config', labelKey: 'nav.settings', minRole: 'root' },
];

/** 按角色过滤可见导航。未知角色时不展示，避免闪出越权入口。 */
export function navTabsFor(role: ManagementRole | undefined): NavTab[] {
  if (!role) return [];
  return NAV_TABS.filter((tab) => roleAtLeast(role, tab.minRole));
}
