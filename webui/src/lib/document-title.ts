import { i18n } from '@/app/providers/i18n';

const APP_TITLE_KEY = 'app.title';

/** 从路由 match 链中取最深一层的 `staticData.titleKey`。 */
export function resolveRouteTitleKey(matches: readonly unknown[]): string | undefined {
  for (let index = matches.length - 1; index >= 0; index -= 1) {
    const match = matches[index];
    if (typeof match !== 'object' || match === null || !('staticData' in match)) {
      continue;
    }
    const titleKey = (match as { staticData?: { titleKey?: string } }).staticData?.titleKey;
    if (titleKey) {
      return titleKey;
    }
  }
  return undefined;
}

/** 页面标题与站点名组合；无页面 key 时仅显示站点名。 */
export function formatDocumentTitle(pageTitleKey?: string): string {
  const appName = i18n.global.t(APP_TITLE_KEY);
  if (!pageTitleKey) {
    return appName;
  }
  const pageTitle = i18n.global.t(pageTitleKey);
  return `${pageTitle} · ${appName}`;
}

/** 将当前路由标题写入 `document.title`。 */
export function syncDocumentTitle(pageTitleKey?: string): void {
  document.title = formatDocumentTitle(pageTitleKey);
}
