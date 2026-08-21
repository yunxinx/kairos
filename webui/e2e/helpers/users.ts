import { expect, type Page } from '@playwright/test';
import type { ManagementRole } from '../../src/api/types';
import { e2eRootHeaders } from './session';

/** 经管理 API 创建管理用户。 */
export async function seedUser(
  page: Page,
  body: { email: string; role: ManagementRole; password?: string; display_name?: string },
): Promise<{ id: number }> {
  const resp = await page.request.post('/users', {
    headers: await e2eRootHeaders(page.request),
    data: {
      display_name: body.display_name ?? body.email,
      password: body.password ?? 'password1',
      ...body,
    },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  return (await resp.json()) as { id: number };
}

/** 写入会话到 localStorage 并进入概览。 */
export async function openSession(
  page: Page,
  email: string,
  password = 'password1',
): Promise<void> {
  const resp = await page.request.post('/login', { data: { email, password } });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  const body = (await resp.json()) as { token: string };
  await page.goto('/login');
  await page.evaluate(
    ({ storage, token }) => {
      localStorage.setItem(storage, token);
    },
    { storage: 'kairos-admin-key', token: body.token },
  );
  await page.goto('/overview');
}
