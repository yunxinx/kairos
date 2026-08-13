export interface NavTab {
  to: string;
  labelKey: string;
}

/** 管理面导航。路径避开资源 API（`/tokens` `/channels` `/settings` `/logs`）。 */
export const NAV_TABS: NavTab[] = [
  { to: '/overview', labelKey: 'nav.overview' },
  { to: '/token', labelKey: 'nav.tokens' },
  { to: '/channel', labelKey: 'nav.channel' },
  { to: '/pricing', labelKey: 'nav.pricing' },
  { to: '/config', labelKey: 'nav.settings' },
  { to: '/requests', labelKey: 'nav.logs' },
];
