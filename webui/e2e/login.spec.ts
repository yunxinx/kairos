import { expect, test } from './fixtures';
import { E2E_ADMIN_KEY } from './helpers/gateway';

const ADMIN_KEY_STORAGE = 'kairos-admin-key';

test.describe.configure({ mode: 'serial' });

test.describe('admin key login', () => {
  test('redirects the admin root to the login page', async ({ page }) => {
    await page.goto('/');
    await page.waitForURL('**/login');
    await expect(page.locator('#login-admin-key')).toBeVisible();
  });

  test('accepts a valid admin key and reaches overview', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-admin-key').fill(E2E_ADMIN_KEY);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await expect(page.getByRole('navigation')).toBeVisible();
  });

  test('rejects an invalid admin key', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-admin-key').fill('sk-wrong');
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await expect(page.getByText(/invalid admin key|admin key 无效/i)).toBeVisible();
    await expect(page).toHaveURL(/\/login/);
    expect(await page.evaluate((key) => localStorage.getItem(key), ADMIN_KEY_STORAGE)).toBeNull();
  });

  test('keeps the admin key across refresh', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-admin-key').fill(E2E_ADMIN_KEY);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await page.reload();
    await expect(page).toHaveURL(/\/overview/);
    await expect(page.getByRole('navigation')).toBeVisible();
    expect(await page.evaluate((key) => localStorage.getItem(key), ADMIN_KEY_STORAGE)).toBe(
      E2E_ADMIN_KEY,
    );
  });

  test('clears the admin key on sign out', async ({ page }) => {
    await page.goto('/login');
    await page.locator('#login-admin-key').fill(E2E_ADMIN_KEY);
    await page.getByRole('button', { name: /sign in|登录/i }).click();
    await page.waitForURL('**/overview');
    await page.getByRole('button', { name: /sign out|退出/i }).click();
    await page.waitForURL('**/login');
    expect(await page.evaluate((key) => localStorage.getItem(key), ADMIN_KEY_STORAGE)).toBeNull();
    await page.reload();
    await expect(page).toHaveURL(/\/login/);
  });
});
