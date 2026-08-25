import { authedTest as test, expect } from './fixtures';
import { seedModelGroup } from './helpers/models';
import { e2eRootHeaders } from './helpers/session';
import { clickRowAction } from './helpers/table';
import type { Page } from '@playwright/test';

/** 经 API 建档：表格能力的用例只关心列表里有几行，不必走一遍编辑器表单。 */
async function seedPlan(
  page: Page,
  body: { internal_name: string; display_name: string; audience?: 'user' | 'admin' },
): Promise<void> {
  const resp = await page.request.post('/api/plans', {
    headers: await e2eRootHeaders(page.request),
    data: {
      internal_name: body.internal_name,
      display_name: body.display_name,
      note: '',
      discount_bp: 10000,
      default_rpm: null,
      shared_rpm: null,
      audience: body.audience ?? 'user',
      groups: [],
    },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

test.describe.configure({ mode: 'serial' });

test.describe('plans page', () => {
  test('creates, edits, shares, and deletes a plan', async ({ page }) => {
    await page.goto('/plans');
    await expect(page.getByRole('heading', { name: /plans/i })).toBeVisible();

    await page.getByTestId('create-plan-admin').click();
    await page.getByTestId('plan-internal-name').fill('e2e-plan');
    await page.getByTestId('plan-display-name').fill('E2E Plan');
    await page.getByTestId('plan-discount').fill('80');
    await page.getByTestId('plan-grant').fill('1');
    await page.getByTestId('plan-shared-switch').click();
    await page.getByTestId('plan-save').click();

    const row = page.locator('[data-testid="plan-row"]', { hasText: 'e2e-plan' });
    await expect(row).toBeVisible();
    await expect(row).toContainText('E2E Plan');
    await expect(row.getByTestId('plan-discount-cell')).toContainText('80%');
    await expect(row.getByTestId('plan-shared-badge')).toBeVisible();

    await row.getByTestId('plan-edit').click();
    await page.getByTestId('plan-display-name').fill('E2E Plan Edited');
    await page.getByTestId('plan-save').click();
    await expect(row).toContainText('E2E Plan Edited');

    await clickRowAction(row, page, 'plan-delete');
    await page.getByRole('dialog').getByTestId('plan-delete-confirm').click();
    await expect(row).toHaveCount(0);
  });

  test('hides capability switches on user plans and moves the default flag', async ({ page }) => {
    await seedModelGroup(page, { name: 'e2e-plan-group', models: [] });
    await page.goto('/plans');

    // 用户档没有管理面：能力开关整块不该出现，否则运营会在「给用户的档」上误开
    // manage_users 之类的开关。
    await page.getByTestId('create-plan-user').click();
    await expect(page.getByTestId('plan-capability-manage_users')).toHaveCount(0);

    // 组名单是可搜索表格而不是复选框墙：搜到再勾，计数随之变化。
    await page.getByTestId('plan-group-search').fill('e2e-plan-group');
    await expect(page.getByTestId('plan-group-row')).toHaveCount(1);
    await page.getByTestId('plan-group-e2e-plan-group').click();
    await expect(page.getByTestId('plan-groups-count')).toContainText('1');

    await page.getByTestId('plan-internal-name').fill('e2e-user-plan');
    await page.getByTestId('plan-display-name').fill('E2E User Plan');
    await page.getByTestId('plan-save').click();

    const userPlan = page.locator('[data-testid="plan-row"]', { hasText: 'e2e-user-plan' });
    await expect(userPlan.getByTestId('plan-audience')).toContainText(/user/i);
    await expect(userPlan.getByTestId('plan-default-badge')).toHaveCount(0);

    // 已建套餐的受众和默认身份不属于通用编辑契约。
    await userPlan.getByTestId('plan-edit').click();
    await expect(page.getByTestId('plan-default-switch')).toHaveCount(0);
    await page.keyboard.press('Escape');

    // 默认身份是显式转移命令，不是可以关闭的普通开关。
    await clickRowAction(userPlan, page, 'plan-set-default');
    await expect(userPlan.getByTestId('plan-default-badge')).toBeVisible();

    // 每个受众只能有一个默认档：新档接手后，内置 standard 档必须让位。
    const standard = page.locator('[data-testid="plan-row"]', { hasText: 'standard' });
    await expect(standard.getByTestId('plan-default-badge')).toHaveCount(0);
    // admin 档属于另一个受众，不受用户档改默认的影响。
    const adminPlan = page.locator('[data-testid="plan-row"]').filter({ hasText: 'admin' });
    await expect(adminPlan.getByTestId('plan-default-badge')).toBeVisible();

    // 管理员档才有能力开关，且受众建后不可改——编辑用户档时同样看不到开关。
    await page.getByTestId('create-plan-admin').click();
    await expect(page.getByTestId('plan-capability-manage_users')).toBeVisible();
    await page.keyboard.press('Escape');

    await clickRowAction(standard, page, 'plan-set-default');
    await expect(standard.getByTestId('plan-default-badge')).toBeVisible();
    await expect(userPlan.getByTestId('plan-default-badge')).toHaveCount(0);

    await clickRowAction(userPlan, page, 'plan-delete');
    await page.getByRole('dialog').getByTestId('plan-delete-confirm').click();
    await expect(userPlan).toHaveCount(0);
  });

  test('searches, filters, and bulk deletes from the selection bar', async ({ page }) => {
    await seedPlan(page, { internal_name: 'e2e-bulk-a', display_name: 'E2E Bulk A' });
    await seedPlan(page, { internal_name: 'e2e-bulk-b', display_name: 'E2E Bulk B' });
    await page.goto('/plans');

    const rows = page.locator('[data-testid="plan-row"]');
    await expect(rows.filter({ hasText: 'e2e-bulk-a' })).toBeVisible();

    // 搜索按内部名/显示名/备注/组名收窄。
    await page.getByTestId('plans-search').fill('e2e-bulk');
    await expect(rows).toHaveCount(2);
    await page.getByTestId('plans-search').fill('no-such-plan');
    await expect(rows).toHaveCount(0);
    await page.getByTestId('plans-search').fill('');

    // 受众筛选：内置 admin 档属于另一受众，选 user 后不该留在表里。
    await page.getByTestId('plans-audience-filter').click();
    await page.locator('[data-testid="plans-audience-filter-option"][data-value="admin"]').click();
    await page.keyboard.press('Escape');
    await expect(rows.filter({ hasText: 'e2e-bulk-a' })).toHaveCount(0);
    await page.getByTestId('plans-audience-filter').click();
    await page.getByTestId('plans-audience-filter-clear').click();
    await page.keyboard.press('Escape');

    // 属性筛选：只留默认档，刚建的两档都不是默认，应被筛掉。
    await page.getByTestId('plans-flag-filter').click();
    await page.locator('[data-testid="plans-flag-filter-option"][data-value="default"]').click();
    await page.keyboard.press('Escape');
    await expect(rows.filter({ hasText: 'e2e-bulk' })).toHaveCount(0);
    await page.getByTestId('plans-flag-filter').click();
    await page.getByTestId('plans-flag-filter-clear').click();
    await page.keyboard.press('Escape');

    // 内置档不可删，所以复选框禁用，全选也不该把它算进去。
    const builtin = rows.filter({ hasText: 'standard' });
    await expect(builtin.getByTestId('plan-select')).toBeDisabled();

    await page.getByTestId('plans-search').fill('e2e-bulk');
    await page.getByTestId('plans-select-all').click();
    await expect(page.getByTestId('plans-bulk-bar')).toBeVisible();
    await expect(page.getByTestId('bulk-count')).toHaveText('2 selected');

    await page.getByTestId('plans-bulk-delete').click();
    await page.getByRole('dialog').getByTestId('plan-bulk-delete-confirm').click();
    await expect(page.getByTestId('plans-bulk-bar')).toHaveCount(0);
    await expect(rows).toHaveCount(0);

    // 内置档仍在：批量删除只作用于可选行。
    await page.getByTestId('plans-search').fill('');
    await expect(builtin).toBeVisible();
  });

  test('aligns inline switches with their labels in the editor', async ({ page }) => {
    await page.goto('/plans');
    await page.getByTestId('create-plan-admin').click();

    // 开关轨道与左侧标签必须同轴。块级控件盒里放 inline-flex 开关会生成行盒，
    // 行盒含下伸部比轨道高，会把轨道顶起约 1.5px——肉眼就是「没对齐」。
    for (const testId of ['plan-note-visible', 'plan-shared-switch', 'plan-default-switch']) {
      const offset = await page.evaluate((id) => {
        const input = document.querySelector(`[data-testid="${id}"]`);
        const field = input?.closest('[data-form-field]');
        const label = field?.querySelector('.form-field-label');
        const track = field?.querySelector('.toggle-track');
        if (!label || !track) return Number.NaN;
        const labelBox = label.getBoundingClientRect();
        const trackBox = track.getBoundingClientRect();
        return Math.abs(labelBox.top + labelBox.height / 2 - (trackBox.top + trackBox.height / 2));
      }, testId);
      expect(offset, `${testId} 与标签中轴偏移`).toBeLessThanOrEqual(0.5);
    }
  });
});
