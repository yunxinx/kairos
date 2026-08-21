import { expect, test } from './fixtures';
import { e2eRootHeaders } from './helpers/session';
import { seedModelGroup } from './helpers/models';
import { clickRowAction } from './helpers/table';
import { openSession, seedUser } from './helpers/users';

test.describe.configure({ mode: 'serial' });

test.describe('role navigation', () => {
  test('user cannot see channels, models, users, or settings', async ({ page }) => {
    await seedUser(page, { email: 'nav-user@example.com', role: 'user' });
    await openSession(page, 'nav-user@example.com');

    const nav = page.getByRole('navigation');
    await expect(nav.getByRole('link', { name: /^tokens$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^logs$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^channels$/i })).toHaveCount(0);
    await expect(nav.getByRole('link', { name: /^models$/i })).toHaveCount(0);
    await expect(nav.getByRole('link', { name: /^users$/i })).toHaveCount(0);
    await expect(nav.getByRole('link', { name: /^settings$/i })).toHaveCount(0);

    await page.goto('/channel');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/admin/users');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/models');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/config');
    await expect(page).toHaveURL(/\/overview/);
  });

  test('admin can see users and models but not channels or settings', async ({ page }) => {
    await seedUser(page, { email: 'nav-admin@example.com', role: 'admin' });
    await openSession(page, 'nav-admin@example.com');

    const nav = page.getByRole('navigation');
    await expect(nav.getByRole('link', { name: /^users$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^models$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^channels$/i })).toHaveCount(0);
    await expect(nav.getByRole('link', { name: /^settings$/i })).toHaveCount(0);

    await page.goto('/channel');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/config');
    await expect(page).toHaveURL(/\/overview/);
  });

  test('user token editor only lists assigned groups', async ({ page }) => {
    await seedModelGroup(page, { name: 'e2e-assigned', models: [] });
    await seedModelGroup(page, { name: 'e2e-hidden', models: [] });
    const user = await seedUser(page, { email: 'nav-groups@example.com', role: 'user' });
    const assign = await page.request.put(`/users/${user.id}/model-groups`, {
      headers: await e2eRootHeaders(page.request),
      data: { groups: ['e2e-assigned'] },
    });
    expect(assign.ok(), await assign.text()).toBeTruthy();

    await openSession(page, 'nav-groups@example.com');
    await page.goto('/token');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('assigned-token');
    await page.getByTestId('token-editor-group').click();
    await expect(page.getByRole('option', { name: 'e2e-assigned', exact: true })).toBeVisible();
    await expect(page.getByRole('option', { name: 'e2e-hidden', exact: true })).toHaveCount(0);
    await expect(page.getByRole('option', { name: 'Ungrouped', exact: true })).toHaveCount(0);
    await page.getByRole('option', { name: 'e2e-assigned', exact: true }).click();
    await page.getByTestId('token-save').click();
    const createdRow = page.locator('[data-testid="token-row"]', { hasText: 'assigned-token' });
    await expect(createdRow.getByTestId('token-model-group')).toContainText('e2e-assigned');
  });

  test('withdrawn group marks the bound token unusable', async ({ page }) => {
    await seedModelGroup(page, { name: 'e2e-withdraw', models: [] });
    const user = await seedUser(page, { email: 'nav-withdraw@example.com', role: 'user' });
    const assign = await page.request.put(`/users/${user.id}/model-groups`, {
      headers: await e2eRootHeaders(page.request),
      data: { groups: ['e2e-withdraw'] },
    });
    expect(assign.ok(), await assign.text()).toBeTruthy();

    const session = await page.request.post('/login', {
      data: { email: 'nav-withdraw@example.com', password: 'password1' },
    });
    expect(session.ok(), await session.text()).toBeTruthy();
    const sessionBody = (await session.json()) as { token: string };
    const created = await page.request.post('/tokens', {
      headers: { Authorization: `Bearer ${sessionBody.token}` },
      data: {
        name: 'withdraw-me',
        limit_usd_micros: null,
        enabled: true,
        model_group: 'e2e-withdraw',
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    const owned = (await created.json()) as { token_key: string };

    const withdraw = await page.request.put(`/users/${user.id}/model-groups`, {
      headers: await e2eRootHeaders(page.request),
      data: { groups: [] },
    });
    expect(withdraw.ok(), await withdraw.text()).toBeTruthy();

    await openSession(page, 'nav-withdraw@example.com');
    await page.goto('/token');
    const row = page.locator(`[data-testid="token-row"][data-token-key="${owned.token_key}"]`);
    await expect(row.getByTestId('token-group-unusable')).toBeVisible();
    await row.getByTestId('token-edit').click();
    await expect(page.getByTestId('token-group-unusable-hint')).toBeVisible();
  });

  test('admin can open models pricing and assign user groups', async ({ page }) => {
    await seedModelGroup(page, { name: 'e2e-admin-assign', models: [] });
    await seedUser(page, { email: 'nav-admin-ops@example.com', role: 'admin' });
    const target = await seedUser(page, { email: 'nav-admin-target@example.com', role: 'user' });

    await openSession(page, 'nav-admin-ops@example.com');
    await page.goto('/models');
    await expect(page.getByRole('tablist', { name: /model workspace/i })).toBeVisible();
    await expect(page.getByRole('columnheader', { name: 'Input ($)', exact: true })).toBeVisible();

    await page.goto('/admin/users');
    const row = page.locator(`[data-testid="user-row"][data-user-id="${target.id}"]`);
    await clickRowAction(row, page, 'user-groups');
    await page.getByTestId('user-group-e2e-admin-assign').click();
    await page.getByTestId('user-groups-save').click();
    await expect(
      row.getByTestId('user-group-chip').filter({ hasText: 'e2e-admin-assign' }),
    ).toBeVisible();
  });
});
