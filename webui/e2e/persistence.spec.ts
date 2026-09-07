import { authedTest as test, expect } from './fixtures';

// URL 状态与 localStorage 偏好的持久化回归：tab/筛选刷新可恢复，列宽拖动落盘。
test.describe('page state persistence', () => {
  test('models tab and filters survive reload via URL params', async ({ page }) => {
    await page.goto('/models?tab=order');
    await expect(page.getByTestId('models-tab-order')).toHaveAttribute('data-state', 'active');

    // tab 切换写回 URL（replace），默认值不落参数。
    await page.getByTestId('models-tab-inventory').click();
    await expect(page).toHaveURL(/\/models\?tab=inventory/);

    // 未知 tab 值回落默认，不渲染空内容。
    await page.goto('/models?tab=nonsense');
    await expect(page.getByTestId('models-tab-inventory')).toHaveAttribute('data-state', 'active');
  });

  test('users filters ride the URL and survive reload', async ({ page }) => {
    await page.goto('/users');
    await page.getByTestId('users-search').fill('nobody-matches');
    // 搜索词防抖写回 URL。
    await expect(page).toHaveURL(/\/users\?q=nobody-matches/, { timeout: 5_000 });
    await page.reload();
    await expect(page.getByTestId('users-search')).toHaveValue('nobody-matches');

    // 清空后参数摘掉，地址栏干净。
    await page.getByTestId('users-search').fill('');
    await expect(page).toHaveURL(/\/users$/, { timeout: 5_000 });
  });

  test('logs page size persists in localStorage', async ({ page }) => {
    await page.goto('/logs');
    const pageSizeSelect = page.locator('#logs-page-size');
    await pageSizeSelect.click();
    await page.getByRole('option', { name: '50' }).click();
    expect(await page.evaluate(() => localStorage.getItem('kairos-logs-page-size'))).toBe('50');
    await page.reload();
    await expect(page.locator('#logs-page-size')).toContainText('50');
  });

  test('column widths persist after drag and reset from the column menu', async ({ page }) => {
    await page.goto('/users');
    const handle = page.getByTestId('column-resize-role');
    await expect(handle).toBeVisible();

    // 拖拽加宽 role 列 40px。
    const box = await handle.boundingBox();
    if (box === null) throw new Error('missing bounding box');
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 40, box.y + box.height / 2, { steps: 4 });
    await page.mouse.up();

    const stored = await page.evaluate(() => localStorage.getItem('kairos-users-column-widths'));
    expect(stored).toContain('"role":136');
    await page.reload();
    const storedAfter = await page.evaluate(() =>
      localStorage.getItem('kairos-users-column-widths'),
    );
    expect(storedAfter).toBe(stored);

    // 第二轮拖拽从当前实测宽度起算（而非默认宽），再加 24px 不会跳回。
    const box2 = await handle.boundingBox();
    if (box2 === null) throw new Error('missing bounding box');
    await page.mouse.move(box2.x + box2.width / 2, box2.y + box2.height / 2);
    await page.mouse.down();
    await page.mouse.move(box2.x + box2.width / 2 + 24, box2.y + box2.height / 2, { steps: 4 });
    await page.mouse.up();
    await expect
      .poll(async () => page.evaluate(() => localStorage.getItem('kairos-users-column-widths')))
      .toContain('"role":160');

    // 列菜单重置列宽：拖宽过的 role 列回到默认 96px。
    await page.getByTestId('users-columns').click();
    await page.getByTestId('users-columns-reset-widths').click();
    await expect
      .poll(async () => page.evaluate(() => localStorage.getItem('kairos-users-column-widths')))
      .toContain('"role":96');
  });

  test('tables without column resize show no reset-widths menu item', async ({ page }) => {
    await page.goto('/plans');
    await page.getByTestId('plans-columns').click();
    await expect(page.getByTestId('plans-columns-reset-widths')).toHaveCount(0);
    await page.keyboard.press('Escape');
  });
});
