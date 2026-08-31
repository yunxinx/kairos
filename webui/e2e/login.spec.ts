import { expect, test } from './fixtures';
import { E2E_ADMIN_EMAIL, E2E_ADMIN_ORIGIN, E2E_ADMIN_PASSWORD } from './helpers/gateway';
import { e2eRootHeaders } from './helpers/session';
import { seedUser } from './helpers/users';

test.describe.configure({ mode: 'serial' });

test.describe('email password login', () => {
  test('renders the marketing home at the admin root', async ({ page }) => {
    await page.goto('/');
    await expect(
      page.getByRole('heading', { name: /provider-agnostic ai gateway/i }),
    ).toBeVisible();
    await page.getByRole('link', { name: /get started/i }).click();
    await page.waitForURL('**/login');
    await expect(page.locator('#login-email')).toBeVisible();
  });

  test('accepts a valid email and password and reaches overview', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill(E2E_ADMIN_PASSWORD);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await expect(page.getByRole('navigation')).toBeVisible();
    await page.goto('/');
    await page.waitForURL('**/overview');
    await expect(page.getByRole('heading', { name: /overview/i })).toBeVisible();
  });

  test('rejects invalid credentials', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill('wrong-password');
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await expect(page.getByText(/invalid email or password|邮箱或密码不正确/i)).toBeVisible();
    await expect(page).toHaveURL(/\/login/);
  });

  test('keeps the session across refresh', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill(E2E_ADMIN_PASSWORD);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await page.reload();
    await expect(page).toHaveURL(/\/overview/);
    await expect(page.getByRole('navigation')).toBeVisible();
  });

  test('clears the session on sign out', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill(E2E_ADMIN_PASSWORD);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await page.getByTestId('account-menu-trigger').click();
    await page.getByTestId('nav-logout').click();
    await page.waitForURL('**/login');
    await page.reload();
    await expect(page).toHaveURL(/\/login/);
  });

  test('never renders the previous account token cache after switching accounts', async ({
    page,
  }) => {
    const secondEmail = 'cache-boundary@example.com';
    await seedUser(page, { email: secondEmail, role: 'user' });

    const rootToken = await page.request.post('/api/tokens', {
      headers: await e2eRootHeaders(page.request),
      data: {
        name: 'cache-a-only',
        balance_usd_micros: null,
        enabled: true,
        model_group: 'default',
      },
    });
    expect(rootToken.ok(), await rootToken.text()).toBeTruthy();
    const secondLogin = await page.request.post('/api/login', {
      data: { email: secondEmail, password: 'password1' },
    });
    expect(secondLogin.ok(), await secondLogin.text()).toBeTruthy();
    const secondToken = await page.request.post('/api/tokens', {
      headers: { Origin: E2E_ADMIN_ORIGIN },
      data: {
        name: 'cache-b-only',
        balance_usd_micros: null,
        enabled: true,
        model_group: 'default',
      },
    });
    expect(secondToken.ok(), await secondToken.text()).toBeTruthy();

    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill(E2E_ADMIN_PASSWORD);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await page.goto('/tokens');
    await expect(
      page.locator('[data-testid="token-row"]', { hasText: 'cache-a-only' }),
    ).toBeVisible();

    let releaseStaleRequest: (() => void) | undefined;
    const staleRequestGate = new Promise<void>((resolve) => {
      releaseStaleRequest = resolve;
    });
    let markStaleRequestStarted: (() => void) | undefined;
    const staleRequestStarted = new Promise<void>((resolve) => {
      markStaleRequestStarted = resolve;
    });
    let markStaleRequestFinished: (() => void) | undefined;
    const staleRequestFinished = new Promise<void>((resolve) => {
      markStaleRequestFinished = resolve;
    });

    let releaseSecondAccountTokens: (() => void) | undefined;
    const secondAccountTokenGate = new Promise<void>((resolve) => {
      releaseSecondAccountTokens = resolve;
    });
    let markSecondAccountRequestStarted: (() => void) | undefined;
    const secondAccountRequestStarted = new Promise<void>((resolve) => {
      markSecondAccountRequestStarted = resolve;
    });
    let requestPhase: 'stale-root' | 'second-account' = 'stale-root';
    await page.route('**/api/tokens', async (route) => {
      if (route.request().method() !== 'GET') {
        await route.continue();
        return;
      }
      if (requestPhase === 'stale-root') {
        markStaleRequestStarted?.();
        await staleRequestGate;
        await route.fulfill({
          status: 401,
          contentType: 'application/json',
          body: JSON.stringify({ error: { code: 'unauthorized', message: 'expired' } }),
        });
        markStaleRequestFinished?.();
        return;
      }
      if (requestPhase === 'second-account') {
        markSecondAccountRequestStarted?.();
        await secondAccountTokenGate;
      }
      await route.continue();
    });

    await page.reload();
    await staleRequestStarted;
    await page.getByTestId('account-menu-trigger').click();
    await page.getByTestId('nav-logout').click();
    await page.waitForURL('**/login');

    await page.locator('#login-email').fill(secondEmail);
    await page.locator('#login-password').fill('password1');
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    releaseStaleRequest?.();
    await staleRequestFinished;
    await expect(page).toHaveURL(/\/overview/);

    requestPhase = 'second-account';
    await page
      .getByRole('navigation')
      .getByRole('link', { name: /^tokens$/i })
      .click();
    await secondAccountRequestStarted;

    await expect(
      page.locator('[data-testid="token-row"]', { hasText: 'cache-a-only' }),
    ).toHaveCount(0);
    await expect(
      page.locator('[data-testid="token-row"]', { hasText: 'cache-b-only' }),
    ).toHaveCount(0);

    releaseSecondAccountTokens?.();
    await expect(
      page.locator('[data-testid="token-row"]', { hasText: 'cache-b-only' }),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="token-row"]', { hasText: 'cache-a-only' }),
    ).toHaveCount(0);
  });

  test('account menu stays open when toggling theme and language', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill(E2E_ADMIN_PASSWORD);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');

    const themeToggle = page.getByTestId('nav-theme-toggle');
    const localeToggle = page.getByTestId('nav-locale-toggle');

    await page.getByTestId('account-menu-trigger').click();
    await expect(themeToggle).toBeVisible();

    // 切主题：菜单保持展开，且主题确实变了（就地开关，不该逼用户重开菜单）。
    const before = await page.evaluate(() => document.documentElement.classList.contains('dark'));
    await themeToggle.click();
    await expect(themeToggle).toBeVisible();
    await expect
      .poll(() => page.evaluate(() => document.documentElement.classList.contains('dark')))
      .not.toBe(before);

    // 切语言：子菜单展开并选择，菜单同样保持展开。
    await localeToggle.hover();
    const zhOption = page.getByTestId('nav-locale-zh');
    await expect(zhOption).toBeVisible();
    await zhOption.click();
    await expect(localeToggle).toBeVisible();

    // 「账户」是导航动作，仍应关闭菜单。
    await page.getByTestId('nav-account').click();
    await page.waitForURL('**/account');
    await expect(themeToggle).toHaveCount(0);
  });

  test('account page can update display name and toggle top identity visibility', async ({
    page,
  }) => {
    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill(E2E_ADMIN_PASSWORD);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');

    // 默认显示头像
    await expect(page.getByTestId('account-menu-avatar')).toBeVisible();

    // 菜单展开后无头像
    await page.getByTestId('account-menu-trigger').click();
    await expect(page.locator('.data-table-menu [data-testid="account-menu-avatar"]')).toHaveCount(
      0,
    );

    await page.getByTestId('nav-account').click();
    await page.waitForURL('**/account');
    await page.getByTestId('account-display-name').fill('Root operator');
    await page.getByTestId('account-save').click();
    await expect(page.getByTestId('toast')).toContainText(/account saved|账户已保存/i);
    await expect(page.getByTestId('account-menu-trigger')).toContainText('Root operator');

    // 名称和头像都是账户页可独立控制的导航偏好。
    await page.getByTestId('account-show-nav-name').click();
    await expect(page.getByTestId('account-menu-trigger')).not.toContainText('Root operator');
    await page.getByTestId('account-show-nav-name').click();
    await expect(page.getByTestId('account-menu-trigger')).toContainText('Root operator');

    // 切换隐藏右上角头像
    await page.getByTestId('account-show-nav-avatar').click();
    await expect(page.getByTestId('account-menu-avatar')).toHaveCount(0);

    // 切换恢复右上角头像
    await page.getByTestId('account-show-nav-avatar').click();
    await expect(page.getByTestId('account-menu-avatar')).toBeVisible();
  });
});
