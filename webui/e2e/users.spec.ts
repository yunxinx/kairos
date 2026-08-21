import { authedTest as test, expect } from './fixtures';
import { e2eRootHeaders } from './helpers/session';
import { seedModelGroup } from './helpers/models';
import { clickRowAction } from './helpers/table';
import { seedUser } from './helpers/users';

test.describe.configure({ mode: 'serial' });

test.describe('users page', () => {
  test('lists /admin/users and supports create, recharge, groups, and disable', async ({
    page,
  }) => {
    await seedModelGroup(page, { name: 'e2e-user-group', models: [] });
    await page.goto('/admin/users');
    await expect(page.getByRole('heading', { name: /users/i })).toBeVisible();

    await page.getByTestId('create-user').click();
    await page.getByTestId('user-editor-email').fill('e2e-user@example.com');
    await page.getByTestId('user-editor-display-name').fill('E2E User');
    await page.getByTestId('user-editor-password').fill('password1');
    await page.getByTestId('user-save').click();

    const row = page.locator('[data-testid="user-row"]', { hasText: 'e2e-user@example.com' });
    await expect(row).toBeVisible();

    await clickRowAction(row, page, 'user-recharge');
    await page.getByTestId('user-recharge-amount').fill('3.5');
    await page.getByTestId('user-recharge-save').click();
    await expect(row).toContainText('3.5');

    await clickRowAction(row, page, 'user-groups');
    await page.getByTestId('user-group-e2e-user-group').click();
    await page.getByTestId('user-groups-save').click();
    await expect(
      row.getByTestId('user-group-chip').filter({ hasText: 'e2e-user-group' }),
    ).toBeVisible();

    await row.getByTestId('user-toggle-enabled').click();
    await expect(row.getByTestId('user-toggle-enabled')).toHaveText(/disabled/i);

    const created = await seedUser(page, { email: 'e2e-tokens@example.com', role: 'user' });
    const tokenResp = await page.request.post('/tokens', {
      headers: await e2eRootHeaders(page.request),
      data: { name: 'will-not-belong', limit_usd_micros: null, enabled: true },
    });
    expect(tokenResp.ok()).toBeTruthy();

    const session = await page.request.post('/login', {
      data: { email: 'e2e-tokens@example.com', password: 'password1' },
    });
    expect(session.ok()).toBeTruthy();
    const sessionBody = (await session.json()) as { token: string };
    const own = await page.request.post('/tokens', {
      headers: { Authorization: `Bearer ${sessionBody.token}` },
      data: { name: 'owned', limit_usd_micros: null, enabled: true },
    });
    expect(own.ok()).toBeTruthy();
    const owned = (await own.json()) as { token_key: string };

    await page.goto('/admin/users');
    const tokenOwner = page.locator(`[data-testid="user-row"][data-user-id="${created.id}"]`);
    await clickRowAction(tokenOwner, page, 'user-tokens');
    const tokenRow = page.locator(
      `[data-testid="user-token-row"][data-token-key="${owned.token_key}"]`,
    );
    await expect(tokenRow).toBeVisible();
    await tokenRow.getByTestId('user-token-disable').click();
    await expect(tokenRow).toContainText(/disabled/i);
  });
});
