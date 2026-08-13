import { authedTest as test, expect } from './fixtures';

test.describe.configure({ mode: 'serial' });

test.describe('pages outside the shipped admin surface', () => {
  test('setup, me, users, audit, and metrics paths render the not-found page', async ({ page }) => {
    for (const path of ['/setup', '/me', '/admin/users', '/admin/audit', '/metrics']) {
      await page.goto(path);
      await expect(page.getByRole('heading', { name: /page not found/i })).toBeVisible();
    }
  });
});
