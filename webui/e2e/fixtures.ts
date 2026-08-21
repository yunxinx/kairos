import { test as base, expect } from '@playwright/test';
import { seedAdminSession } from './helpers/session';

/** 固定英文 locale，避免 zh-CN 文案导致选择器 flaky。 */
export const test = base.extend({
  page: async ({ page }, use) => {
    await page.addInitScript(() => {
      localStorage.setItem('kairos-locale', 'en');
    });
    await use(page);
  },
});

/** 已持有管理会话（`ksess_…`）的页面，供资源页 e2e 跳过登录表单。 */
export const authedTest = base.extend({
  page: async ({ page }, use) => {
    await seedAdminSession(page);
    await use(page);
  },
});

export { expect };
