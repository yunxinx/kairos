import { expect, test } from './fixtures';

test.describe.configure({ mode: 'serial' });

test.describe('locale and theme', () => {
  test('switches the login chrome between English and Chinese', async ({ page }) => {
    await page.goto('/login');
    await expect(page.getByRole('button', { name: /^sign in$/i })).toBeVisible();
    await page.getByRole('button', { name: '中 / EN' }).click();
    await expect(page.getByRole('button', { name: /^登录$/ })).toBeVisible();
  });

  test('toggles dark mode and keeps it across refresh', async ({ page }) => {
    await page.goto('/login');
    const wasDark = await page.evaluate(() => document.documentElement.classList.contains('dark'));
    await page.getByRole('button', { name: /^(dark|light)$/i }).click();
    const isDark = await page.evaluate(() => document.documentElement.classList.contains('dark'));
    expect(isDark).toBe(!wasDark);
    expect(await page.evaluate(() => localStorage.getItem('kairos-theme'))).toBe(
      isDark ? 'dark' : 'light',
    );
    await page.reload();
    await expect(page.locator('#login-email')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.classList.contains('dark'))).toBe(
      isDark,
    );
  });
});
