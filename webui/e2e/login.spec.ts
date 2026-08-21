import { expect, test } from './fixtures';
import { E2E_ADMIN_EMAIL, E2E_ADMIN_PASSWORD } from './helpers/gateway';

const ADMIN_KEY_STORAGE = 'kairos-admin-key';

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
    expect(await page.evaluate((key) => localStorage.getItem(key), ADMIN_KEY_STORAGE)).toBeNull();
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
    const stored = await page.evaluate((key) => localStorage.getItem(key), ADMIN_KEY_STORAGE);
    expect(stored?.startsWith('ksess_')).toBe(true);
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
    expect(await page.evaluate((key) => localStorage.getItem(key), ADMIN_KEY_STORAGE)).toBeNull();
    await page.reload();
    await expect(page).toHaveURL(/\/login/);
  });

  test('account page can update display name', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-email').fill(E2E_ADMIN_EMAIL);
    await page.locator('#login-password').fill(E2E_ADMIN_PASSWORD);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await page.getByTestId('account-menu-trigger').click();
    await page.getByTestId('nav-account').click();
    await page.waitForURL('**/account');
    await page.getByTestId('account-display-name').fill('Root operator');
    await page.getByTestId('account-save').click();
    await expect(page.getByText(/account saved|账户已保存/i)).toBeVisible();
    await expect(page.getByTestId('account-menu-trigger')).toContainText('Root operator');
  });
});
