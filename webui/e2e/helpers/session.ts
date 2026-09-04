import { expect, type APIResponse, type Page } from '@playwright/test';
import { E2E_ADMIN_EMAIL, E2E_ADMIN_ORIGIN, E2E_ADMIN_PASSWORD } from './gateway';

const SESSION_COOKIE = 'kairos_session';
const SESSION_COOKIE_DOMAIN = new URL(E2E_ADMIN_ORIGIN).hostname;
const SESSION_COOKIE_PATH = '/api';

/**
 * e2e 的连通性探测、上游模型同步与协议面用例全部指向环回上游：出站安检默认
 * 拒绝私网地址，这里确保运行时开关放行。设置落库持久，同一测试进程只在首个
 * 建立会话的上下文里执行一次；后续上下文直接复用结果。
 */
let runtimeSettingsReady: Promise<void> | undefined;

async function enablePrivateNetworksOnce(page: Page): Promise<void> {
  const listed = await page.request.get('/api/settings');
  expect(listed.ok(), await listed.text()).toBeTruthy();
  const settings = (await listed.json()) as Record<string, unknown> & {
    allow_private_networks?: boolean;
  };
  if (settings.allow_private_networks === true) return;
  const updated = await page.request.put('/api/settings', {
    headers: { Origin: E2E_ADMIN_ORIGIN },
    data: { ...settings, allow_private_networks: true },
  });
  expect(updated.ok(), await updated.text()).toBeTruthy();
}

function ensureRuntimeSettings(page: Page): Promise<void> {
  runtimeSettingsReady ??= enablePrivateNetworksOnce(page);
  return runtimeSettingsReady;
}

function findSessionSetCookie(response: APIResponse): string {
  const setCookie = response
    .headersArray()
    .filter((header) => header.name.toLowerCase() === 'set-cookie')
    .map((header) => header.value)
    .find((value) => value.startsWith(`${SESSION_COOKIE}=`));
  expect(setCookie, '登录响应应下发会话 Cookie').toBeTruthy();
  return setCookie as string;
}

/**
 * 钉住服务端会话 Cookie 的安全属性：HttpOnly、SameSite=Strict、Secure。
 * 属性改写只发生在下方的测试 jar 副本上，服务器行为由本断言锁定。
 */
function assertSessionCookieAttributes(response: APIResponse): void {
  const setCookie = findSessionSetCookie(response);
  for (const attribute of ['HttpOnly', 'SameSite=Strict', 'Secure']) {
    expect(setCookie).toContain(attribute);
  }
}

/**
 * Playwright 的 Node 侧 cookie jar 严格执行 Secure 属性：http 请求一律不回发
 * Secure cookie，且对环回地址没有浏览器网络栈的可信来源豁免。环回 HTTP 的
 * 测试环境按可信网络对待——从登录响应提取会话令牌，把 jar 副本的属性改写为
 * 可回发后注入；同一 BrowserContext 的页面请求与 page.request 共用该 jar。
 */
async function injectSessionCookie(page: Page, response: APIResponse): Promise<void> {
  const setCookie = findSessionSetCookie(response);
  const value = setCookie.slice(`${SESSION_COOKIE}=`.length).split(';', 1)[0];
  await page.context().addCookies([
    {
      name: SESSION_COOKIE,
      value,
      domain: SESSION_COOKIE_DOMAIN,
      path: SESSION_COOKIE_PATH,
      secure: false,
      httpOnly: true,
      sameSite: 'Strict',
    },
  ]);
}

/** 通过 API 以给定账号登录，并建立 page.request 可回发的会话。 */
export async function loginViaApi(page: Page, email: string, password: string): Promise<void> {
  const response = await page.request.post('/api/login', {
    data: { email, password },
  });
  expect(response.ok(), await response.text()).toBeTruthy();
  assertSessionCookieAttributes(response);
  await injectSessionCookie(page, response);
}

/** root 会话的写请求头：附带同源 Origin。 */
export async function e2eRootHeaders(page: Page): Promise<{ Origin: string }> {
  await loginViaApi(page, E2E_ADMIN_EMAIL, E2E_ADMIN_PASSWORD);
  await ensureRuntimeSettings(page);
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
  await loginViaApi(page, E2E_ADMIN_EMAIL, E2E_ADMIN_PASSWORD);
  await ensureRuntimeSettings(page);
}
