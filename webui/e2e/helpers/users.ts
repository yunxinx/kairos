import { expect, type Page } from '@playwright/test';
import type { ManagementRole } from '../../src/api/types';
import { E2E_ADMIN_ORIGIN } from './gateway';
import { e2eRootHeaders } from './session';

/** 经管理 API 创建管理用户。 */
export async function seedUser(
  page: Page,
  body: { email: string; role: ManagementRole; password?: string; display_name?: string },
): Promise<{ id: number }> {
  const resp = await page.request.post('/api/users', {
    headers: await e2eRootHeaders(page),
    data: {
      display_name: body.display_name ?? body.email,
      password: body.password ?? 'password1',
      ...body,
    },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  return (await resp.json()) as { id: number };
}

/** 登录并进入概览。 */
export async function openSession(
  page: Page,
  email: string,
  password = 'password1',
): Promise<void> {
  await page.context().clearCookies();
  const resp = await page.request.post('/api/login', {
    // 写请求同源守卫要求携带与 Host 匹配的 Origin；与 loginViaApi 的请求头约定一致。
    headers: { Origin: E2E_ADMIN_ORIGIN },
    data: { email, password },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  await page.goto('/overview');
}
