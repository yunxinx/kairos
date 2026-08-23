import { expect, test } from './fixtures';
import { e2eRootHeaders } from './helpers/session';
import { seedModelGroup } from './helpers/models';
import type { Page } from '@playwright/test';
import { openSession, seedUser } from './helpers/users';


async function createPlan(
  page: Page,
  body: { internal_name: string; display_name: string; groups: string[] },
): Promise<number> {
  const resp = await page.request.post('/api/plans', {
    headers: await e2eRootHeaders(page.request),
    data: {
      internal_name: body.internal_name,
      display_name: body.display_name,
      note: '',
      note_visible_to_admin: false,
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

    await page.goto('/channels');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/users');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/models');
    await expect(page).toHaveURL(/\/overview/);
    await page.goto('/settings');
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
      internal_name: 'e2e-assigned-plan',
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
      internal_name: 'e2e-withdraw-plan',
      display_name: 'Withdraw Plan',
      groups: ['e2e-withdraw'],
    });
    await assignPlan(page, user.id, planId);

    const session = await page.request.post('/api/login', {
      data: { email: 'nav-withdraw@example.com', password: 'password1' },
    });
    expect(session.ok(), await session.text()).toBeTruthy();
    const sessionBody = (await session.json()) as { token: string };
    const created = await page.request.post('/api/tokens', {
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

    const planResp = await page.request.get(`/api/plans`, {
      headers: await e2eRootHeaders(page.request),
    });
    expect(planResp.ok(), await planResp.text()).toBeTruthy();
    const plans = (await planResp.json()) as Array<{ id: number; internal_name?: string }>;
    const withdrawPlan = plans.find((plan) => plan.id === planId);
    expect(withdrawPlan).toBeTruthy();
    const withdraw = await page.request.put(`/api/plans/${planId}`, {
      headers: await e2eRootHeaders(page.request),
      data: {
        internal_name: 'e2e-withdraw-plan',
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
      internal_name: 'e2e-admin-plan',
      display_name: 'Admin Shared Plan',
      groups: ['e2e-admin-assign'],
    });

    await openSession(page, 'nav-admin-ops@example.com');
    await page.goto('/users');
    await expect(page.locator('[data-testid="user-row"]', { hasText: 'root@' })).toHaveCount(0);
    await expect(page.getByTestId('users-role-filter')).toHaveCount(0);
    const row = page.locator(`[data-testid="user-row"][data-user-id="${target.id}"]`);
    await row.getByTestId('user-edit').click();
    await page.getByTestId('user-tab-plan').click();
    await page.getByTestId('user-plan-select').click();
    await page.getByRole('option', { name: 'Admin Shared Plan' }).click();
    await page.getByTestId('user-plan-save').click();
    await expect(page.getByTestId('user-plan-select')).toHaveCount(0);
  });
});
