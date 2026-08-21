import { expect, type APIRequestContext, type Page } from '@playwright/test';
import { E2E_ADMIN_EMAIL, E2E_ADMIN_PASSWORD } from './gateway';

/** 与前端 `session.ts` 同一键：存的是登录换来的会话，不是配置里的登录密码。 */
const ADMIN_KEY_STORAGE = 'kairos-admin-key';

const sessionByRequest = new WeakMap<object, Promise<string>>();

/**
 * 用 e2e 配置里的 root 登录，换管理会话。同一 request 上下文内复用。
 * 登录密码不能当 Bearer；管理 API 只认这里拿到的 `ksess_…`。
 */
export function e2eRootBearer(request: APIRequestContext): Promise<string> {
  const cached = sessionByRequest.get(request);
  if (cached) return cached;
  const pending = (async () => {
    const resp = await request.post('/api/login', {
      data: { email: E2E_ADMIN_EMAIL, password: E2E_ADMIN_PASSWORD },
    });
    expect(resp.ok(), await resp.text()).toBeTruthy();
    const body = (await resp.json()) as { token: string };
    expect(body.token.startsWith('ksess_')).toBe(true);
    return body.token;
  })();
  sessionByRequest.set(request, pending);
  return pending;
}

export async function e2eRootHeaders(
  request: APIRequestContext,
): Promise<{ Authorization: string }> {
  return { Authorization: `Bearer ${await e2eRootBearer(request)}` };
}

/** 已登录夹具：写入 locale 与会话令牌，跳过登录表单。 */
export async function seedAdminSession(page: Page): Promise<void> {
  const token = await e2eRootBearer(page.request);
  await page.addInitScript(
    ({ localeKey, adminKey, locale, key }) => {
      localStorage.setItem(localeKey, locale);
      localStorage.setItem(adminKey, key);
    },
    {
      localeKey: 'kairos-locale',
      adminKey: ADMIN_KEY_STORAGE,
      locale: 'en',
      key: token,
    },
  );
}
