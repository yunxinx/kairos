import { E2E_ADMIN_KEY } from './gateway';
import type { Page } from '@playwright/test';

const ADMIN_KEY_STORAGE = 'kairos-admin-key';

/** 已登录夹具：写入 locale 与 admin key，跳过登录表单。 */
export async function seedAdminSession(page: Page): Promise<void> {
  await page.addInitScript(
    ({ localeKey, adminKey, locale, key }) => {
      localStorage.setItem(localeKey, locale);
      localStorage.setItem(adminKey, key);
    },
    {
      localeKey: 'kairos-locale',
      adminKey: ADMIN_KEY_STORAGE,
      locale: 'en',
      key: E2E_ADMIN_KEY,
    },
  );
}
