import { expect, type APIRequestContext, type Page } from '@playwright/test';
import { E2E_ADMIN_EMAIL, E2E_ADMIN_ORIGIN, E2E_ADMIN_PASSWORD } from './gateway';

/**
 * 确保当前请求上下文使用 root Cookie 会话。
 */
export async function ensureE2eRootSession(request: APIRequestContext): Promise<void> {
  const response = await request.post('/api/login', {
    data: { email: E2E_ADMIN_EMAIL, password: E2E_ADMIN_PASSWORD },
  });
  expect(response.ok(), await response.text()).toBeTruthy();
}

export async function e2eRootHeaders(
  request: APIRequestContext,
): Promise<{ Origin: string }> {
  await ensureE2eRootSession(request);
  return { Origin: E2E_ADMIN_ORIGIN };
}

/** 已登录夹具：写入 locale 并通过登录接口建立 Cookie 会话。 */
export async function seedAdminSession(page: Page): Promise<void> {
  await page.addInitScript(
    ({ localeKey, locale }) => {
      localStorage.setItem(localeKey, locale);
    },
    {
      localeKey: 'kairos-locale',
      locale: 'en',
    },
  );
  const response = await page.request.post('/api/login', {
    data: { email: E2E_ADMIN_EMAIL, password: E2E_ADMIN_PASSWORD },
  });
  expect(response.ok(), await response.text()).toBeTruthy();
}
