import { expect, test } from './fixtures';
import { E2E_ADMIN_ORIGIN } from './helpers/gateway';
import { e2eRootHeaders } from './helpers/session';
import { seedModelGroup } from './helpers/models';
import type { Page } from '@playwright/test';
import { openSession, seedUser } from './helpers/users';

async function createPlan(
  page: Page,
  body: { display_name: string; groups: string[]; note?: string },
): Promise<number> {
  const resp = await page.request.post('/api/plans', {
    headers: await e2eRootHeaders(page.request),
    data: {
      display_name: body.display_name,
      note: body.note ?? '',
      // 备注在管理员侧受 note_visible_to_admin 管控：选档浮窗只呈现 API 给出的可见备注。
      note_visible_to_admin: Boolean(body.note),
      discount_bp: 10000,
      default_rpm: null,
      shared_rpm: null,
      initial_grant_usd_micros: 0,
      capabilities: {},
      shared_with_admin: true,
      groups: body.groups,
    },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  return ((await resp.json()) as { id: number }).id;
}

async function assignPlan(page: Page, userId: number, planId: number): Promise<void> {
  const resp = await page.request.put(`/api/users/${userId}/plan`, {
    headers: await e2eRootHeaders(page.request),
    data: { plan_id: planId },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

test.describe.configure({ mode: 'serial' });

test.describe('role navigation', () => {
  test('user cannot see channels, users, or settings but keeps a read-only models page', async ({
    page,
  }) => {
    await seedUser(page, { email: 'nav-user@example.com', role: 'user' });
    await openSession(page, 'nav-user@example.com');

    const nav = page.getByRole('navigation');
    await expect(nav.getByRole('link', { name: /^tokens$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^logs$/i })).toBeVisible();
    // 模型页对普通用户是自助入口：他得知道自己能调什么，否则只能拿令牌打 /v1/models。
    await expect(nav.getByRole('link', { name: /^models$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^channels$/i })).toHaveCount(0);
    await expect(nav.getByRole('link', { name: /^users$/i })).toHaveCount(0);
    await expect(nav.getByRole('link', { name: /^settings$/i })).toHaveCount(0);

    await page.goto('/channels');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/users');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/settings');
    await expect(page).toHaveURL(/\/overview/);

    // 只读视图：不出运营标签页，也不出任何渠道信息。
    await page.goto('/models');
    await expect(page).toHaveURL(/\/models/);
    await expect(page.getByTestId('my-models-table')).toBeVisible();
    await expect(page.getByTestId('models-tab-inventory')).toHaveCount(0);
    await expect(page.getByTestId('models-tab-groups')).toHaveCount(0);
  });

  test('admin can see users and models but not channels or settings', async ({ page }) => {
    await seedUser(page, { email: 'nav-admin@example.com', role: 'admin' });
    await openSession(page, 'nav-admin@example.com');

    const nav = page.getByRole('navigation');
    await expect(nav.getByRole('link', { name: /^users$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^models$/i })).toBeVisible();
    await expect(nav.getByRole('link', { name: /^channels$/i })).toHaveCount(0);
    await expect(nav.getByRole('link', { name: /^settings$/i })).toHaveCount(0);

    await page.goto('/channels');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/settings');
    await expect(page).toHaveURL(/\/overview/);
  });

  test('user token editor only lists assigned groups', async ({ page }) => {
    await seedModelGroup(page, { name: 'e2e-assigned', models: [] });
    await seedModelGroup(page, { name: 'e2e-hidden', models: [] });
    const user = await seedUser(page, { email: 'nav-groups@example.com', role: 'user' });
    const planId = await createPlan(page, {
      display_name: 'Assigned Plan',
      groups: ['e2e-assigned'],
    });
    await assignPlan(page, user.id, planId);

    await openSession(page, 'nav-groups@example.com');
    await page.goto('/tokens');
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
    const planId = await createPlan(page, {
      display_name: 'Withdraw Plan',
      groups: ['e2e-withdraw'],
    });
    await assignPlan(page, user.id, planId);

    const session = await page.request.post('/api/login', {
      data: { email: 'nav-withdraw@example.com', password: 'password1' },
    });
    expect(session.ok(), await session.text()).toBeTruthy();
    const created = await page.request.post('/api/tokens', {
      headers: { Origin: E2E_ADMIN_ORIGIN },
      data: {
        name: 'withdraw-me',
        balance_usd_micros: null,
        enabled: true,
        model_group: 'e2e-withdraw',
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    const owned = (await created.json()) as { token_key: string };

    const planResp = await page.request.get(`/api/plans`, {
      headers: await e2eRootHeaders(page.request),
    });
    expect(planResp.ok(), await planResp.text()).toBeTruthy();
    const plans = (await planResp.json()) as Array<{ id: number }>;
    const withdrawPlan = plans.find((plan) => plan.id === planId);
    expect(withdrawPlan).toBeTruthy();
    const withdraw = await page.request.put(`/api/plans/${planId}`, {
      headers: await e2eRootHeaders(page.request),
      data: {
        display_name: 'Withdraw Plan',
        note: '',
        note_visible_to_admin: false,
        discount_bp: 10000,
        default_rpm: null,
        shared_rpm: null,
        initial_grant_usd_micros: 0,
        capabilities: {},
        shared_with_admin: true,
        groups: [],
      },
    });
    expect(withdraw.ok(), await withdraw.text()).toBeTruthy();

    await openSession(page, 'nav-withdraw@example.com');
    await page.goto('/tokens');
    const row = page.locator(`[data-testid="token-row"][data-token-key="${owned.token_key}"]`);
    await expect(row.getByTestId('token-group-unusable')).toBeVisible();
    await row.getByTestId('token-edit').click();
    await expect(page.getByTestId('token-group-unusable-hint')).toBeVisible();
  });

  test('admin can open users and assign a shared plan', async ({ page }) => {
    await seedModelGroup(page, { name: 'e2e-admin-assign', models: [] });
    await seedUser(page, { email: 'nav-admin-ops@example.com', role: 'admin' });
    const target = await seedUser(page, { email: 'nav-admin-target@example.com', role: 'user' });
    const planId = await createPlan(page, {
      display_name: 'Admin Shared Plan',
      note: 'Shared plan note',
      groups: ['e2e-admin-assign'],
    });

    await openSession(page, 'nav-admin-ops@example.com');
    await page.goto('/users');
    await expect(page.locator('[data-testid="user-row"]', { hasText: 'root@' })).toHaveCount(0);
    await expect(page.getByTestId('users-role-filter')).toHaveCount(0);
    const row = page.locator(`[data-testid="user-row"][data-user-id="${target.id}"]`);
    await row.getByTestId('user-edit').click();
    await expect(page.getByTestId('user-editor-plan')).toBeVisible();
    await page.getByTestId('user-editor-plan').click();
    const planSearch = page.getByRole('combobox', { name: 'Plan' });
    await planSearch.fill('missing plan');
    await expect(page.getByText('No results found.')).toBeVisible();
    await page.keyboard.press('Escape');
    await page.getByTestId('user-editor-plan').click();
    await expect(planSearch).toHaveValue('');
    // 选项行同时呈现说明（备注）与默认档徽章，便于运营在套餐间做出选择。
    const sharedOption = page.getByRole('option', { name: 'Admin Shared Plan' });
    await expect(sharedOption).toContainText('Shared plan note');
    await expect(page.getByRole('option', { name: 'Standard' })).toContainText(/default/i);
    await planSearch.fill('Admin Shared Plan');
    await page.keyboard.press('Home');
    await page.keyboard.press('Enter');
    await expect(page.getByTestId('user-editor-plan')).toContainText('Admin Shared Plan');
    await page.getByTestId('user-save').click();
    await expect(page.getByTestId('user-editor-plan')).toHaveCount(0);
  });
});
