import { authedTest as test, expect } from './fixtures';
import { E2E_ADMIN_EMAIL, E2E_ADMIN_ORIGIN, E2E_ADMIN_PASSWORD } from './helpers/gateway';
import { e2eRootHeaders, loginViaApi } from './helpers/session';
import { seedModelGroup } from './helpers/models';
import { clickRowAction } from './helpers/table';
import { seedUser } from './helpers/users';

test.describe.configure({ mode: 'serial' });

test.describe('users page', () => {
  test('lists /users and supports create, edit, recharge, groups, token toggles, and archive', async ({
    page,
  }) => {
    await seedModelGroup(page, { name: 'e2e-user-group', models: [] });
    await page.goto('/users');
    await expect(page.getByRole('heading', { name: /users/i })).toBeVisible();

    await page.getByTestId('create-user').click();
    await expect(page.getByTestId('user-editor-plan')).toHaveAttribute('tabindex', '0');
    await expect(page.getByTestId('user-editor-plan')).toHaveAttribute('aria-label', 'Plan');
    await page.getByTestId('user-editor-role').click();
    await page.getByRole('option', { name: 'Administrator' }).click();
    await expect(page.getByTestId('user-editor-plan')).toBeEnabled();
    await page.getByTestId('user-editor-role').click();
    await page.getByRole('option', { name: 'User', exact: true }).click();
    await page.getByTestId('user-editor-email').fill('e2e-user@example.com');
    await page.getByTestId('user-editor-display-name').fill('E2E User');
    await page.getByTestId('user-editor-password').fill('password1');
    await page.getByTestId('user-save').click();

    const row = page.locator('[data-testid="user-row"]', { hasText: 'e2e-user@example.com' });
    await expect(row).toBeVisible();

    // 编辑用户资料
    await row.getByTestId('user-edit').click();
    const profilePlan = page.getByTestId('user-editor-plan');
    await expect(profilePlan).toBeEnabled();
    await page.getByTestId('user-editor-role').click();
    await page.getByRole('option', { name: 'Administrator' }).click();
    await expect(profilePlan).toBeDisabled();
    await page.getByTestId('user-editor-role').click();
    await page.getByRole('option', { name: 'User', exact: true }).click();
    await expect(profilePlan).toBeEnabled();
    await page.getByTestId('user-editor-display-name').fill('E2E User Renamed');
    await page.getByTestId('user-save').click();
    await expect(row).toContainText('E2E User Renamed');

    await row.getByTestId('user-edit').click();
    await page.getByTestId('user-tab-recharge').click();
    await expect(page.getByTestId('user-current-balance')).toBeVisible();
    await expect(page.getByTestId('user-quick-add-5')).toBeVisible();
    await expect(page.getByTestId('user-quick-sub-5')).toBeVisible();
    await page.getByTestId('user-recharge-amount').fill('3.5');
    await expect(page.getByTestId('user-balance-result')).toHaveText('3.5');
    await page.getByTestId('user-recharge-save').click();
    await expect(row).toContainText('3.5');

    await row.getByTestId('user-edit').click();
    await expect(page.getByTestId('user-editor-plan')).toBeVisible();
    await page.keyboard.press('Escape');

    await row.getByTestId('user-toggle-enabled').click();
    await expect(row.getByTestId('user-toggle-enabled')).toHaveText(/disabled/i);

    const created = await seedUser(page, { email: 'e2e-tokens@example.com', role: 'user' });
    const tokenResp = await page.request.post('/api/tokens', {
      headers: await e2eRootHeaders(page),
      data: { name: 'will-not-belong', balance_usd_micros: null, enabled: true },
    });
    expect(tokenResp.ok()).toBeTruthy();

    await loginViaApi(page, 'e2e-tokens@example.com', 'password1');
    const own = await page.request.post('/api/tokens', {
      headers: { Origin: E2E_ADMIN_ORIGIN },
      data: { name: 'owned', balance_usd_micros: null, enabled: true },
    });
    expect(own.ok()).toBeTruthy();
    // 运营视图按库生成 id 定位：他人令牌的 key 只给脱敏形态。
    const owned = (await own.json()) as { id: number; token_key: string };
    // 以第二用户身份建完令牌后切回 root：/users 是 admin-only 页面。
    await loginViaApi(page, E2E_ADMIN_EMAIL, E2E_ADMIN_PASSWORD);

    await page.goto('/users');
    const tokenOwner = page.locator(`[data-testid="user-row"][data-user-id="${created.id}"]`);
    await tokenOwner.getByTestId('user-edit').click();
    await page.getByTestId('user-tab-tokens').click();
    const tokenRow = page.locator(`[data-testid="user-token-row"][data-token-id="${owned.id}"]`);
    await expect(tokenRow).toBeVisible();
    await expect(tokenRow).not.toContainText(owned.token_key);
    // 余额列与用户列表同款：纯 mono 数值，不再绘制额度进度条。
    await expect(tokenRow.getByTestId('token-balance')).toHaveText('Unlimited');
    await expect(tokenRow.locator('[data-testid="token-quota-track"]')).toHaveCount(0);
    await tokenRow.getByTestId('user-token-toggle-enabled').click();
    await expect(tokenRow).toContainText(/disabled/i);
    await tokenRow.getByTestId('user-token-toggle-enabled').click();
    await expect(tokenRow).toContainText(/enabled/i);

    // 删除用户
    await page.keyboard.press('Escape');
    await clickRowAction(row, page, 'user-delete');
    await page.getByRole('dialog').getByTestId('user-delete-confirm').click();
    await expect(row).toHaveCount(0);
  });

  test('toggles user column visibility, filters, and sorts user list', async ({ page }) => {
    await seedUser(page, {
      email: 'sort-low@example.com',
      display_name: 'Low RPM User',
      role: 'user',
      rate_limit_rpm: 10,
    });
    await seedUser(page, {
      email: 'sort-high@example.com',
      display_name: 'High RPM User',
      role: 'admin',
      rate_limit_rpm: 100,
    });

    await page.addInitScript(() => {
      localStorage.removeItem('kairos-users-columns');
    });

    await page.goto('/users');
    await expect(page.locator('[data-testid="user-row"]').first()).toBeVisible();

    // 检查列隐藏：隐藏 Rate Limit 列
    await page.getByTestId('users-columns').click();
    await page.locator('[data-testid="users-columns-option"][data-value="rateLimitRpm"]').click();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: /rate limit|rpm/i })).toHaveCount(0);

    // 恢复 Rate Limit 列
    await page.getByTestId('users-columns').click();
    await page.locator('[data-testid="users-columns-option"][data-value="rateLimitRpm"]').click();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: /rate limit|rpm/i })).toBeVisible();

    // 筛选特定测试用户
    await page.locator('#users-search').fill('sort-');
    await expect(page.locator('[data-testid="user-row"]')).toHaveCount(2);

    // 排序测试：按 Rate Limit RPM 升序
    await page.getByRole('button', { name: /rate limit|rpm/i }).click();
    await page.getByTestId('column-sort-asc').click();
    const rowsAfterAsc = await page.locator('[data-testid="user-row"]').allInnerTexts();
    expect(rowsAfterAsc[0]).toContain('Low RPM User');
    expect(rowsAfterAsc[1]).toContain('High RPM User');

    // 排序测试：降序
    await page.getByTestId('column-clear-sort').click();
    await page.getByRole('button', { name: /rate limit|rpm/i }).click();
    await page.getByTestId('column-sort-desc').click();
    const rowsAfterDesc = await page.locator('[data-testid="user-row"]').allInnerTexts();
    expect(rowsAfterDesc[0]).toContain('High RPM User');
    expect(rowsAfterDesc[1]).toContain('Low RPM User');

    // 分面筛选测试：按角色筛选 user
    await page.getByTestId('users-role-filter').click();
    await page.locator('[data-testid="users-role-filter-option"][data-value="user"]').click();
    await page.keyboard.press('Escape');
    await expect(page.locator('[data-testid="user-row"]')).toHaveCount(1);
    await expect(page.locator('[data-testid="user-row"]')).toContainText('Low RPM User');

    // 清除角色筛选与搜索
    await page.getByTestId('users-role-filter').click();
    await page.locator('[data-testid="users-role-filter-option"][data-value="user"]').click();
    await page.keyboard.press('Escape');
    await page.locator('#users-search').fill('');

    // 查看 Root 用户
    const rootRow = page.locator('[data-testid="user-row"]', { hasText: 'root@localhost' });
    await expect(rootRow).toContainText(/unlimited|无限制/i);
    await rootRow.getByTestId('user-edit').click();
    await expect(page.getByTestId('user-tab-tokens')).toHaveCount(0);
    await expect(page.getByTestId('user-tab-profile')).toBeVisible();
    await page.keyboard.press('Escape');
  });
});
