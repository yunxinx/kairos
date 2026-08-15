import { authedTest as test, expect } from './fixtures';
import { clickRowAction } from './helpers/table';
import { startProbeUpstream } from './helpers/upstream';

test.describe.configure({ mode: 'serial' });

test.describe('channel resource page', () => {
  test('creates, edits, probes, and deletes a channel', async ({ page }) => {
    const okUpstream = await startProbeUpstream(200, { models: ['gpt-4o-mini', 'gpt-4o'] });
    const failUpstream = await startProbeUpstream(500, { models: ['gpt-4o-mini'] });
    const errUpstream = await startProbeUpstream(404);
    try {
      await page.goto('/channel');
      await expect(page.getByRole('heading', { name: /channels/i })).toBeVisible();

      await page.getByTestId('create-channel').click();
      // Base URL/API key 缺失时同步按钮禁用。
      await expect(page.getByTestId('channel-sync-models')).toBeDisabled();
      await page.locator('[id^="channel-editor-name"]').fill('ok-channel');
      await page.locator('[id^="channel-editor-base-url"]').fill(okUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');

      // 同步上游模型：勾选 gpt-4o-mini，返回即写回清单草稿。
      await page.getByTestId('channel-sync-models').click();
      await expect(page.getByTestId('channel-sync-view')).toBeVisible();
      const miniRow = page.locator('[data-testid="channel-sync-row"][data-model="gpt-4o-mini"]');
      const fullRow = page.locator('[data-testid="channel-sync-row"][data-model="gpt-4o"]');
      await expect(miniRow.getByTestId('channel-sync-status-unselected')).toBeVisible();
      await miniRow.getByTestId('channel-sync-checkbox').click();
      await expect(miniRow.getByTestId('channel-sync-status-willAdd')).toBeVisible();
      await page.getByTestId('channel-sync-back').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(1);
      await expect(page.getByTestId('channel-model-count')).toHaveText('1');

      await page.getByTestId('channel-editor-tab-advanced').click();
      await page.locator('[id^="channel-editor-timeout-ms"]').fill('5000');
      await page.locator('[id^="channel-editor-max-retries"]').fill('1');
      await page.getByTestId('channel-save').click();

      const okRow = page.locator('[data-testid="channel-row"][data-channel-name="ok-channel"]');
      await expect(okRow).toBeVisible();
      await expect(okRow).toContainText('gpt-4o-mini');
      // 品牌图标以 mask 渲染：mask-image 为空会退化成纯色块。
      await expect(okRow.locator('.brand-icon')).toHaveCSS('mask-image', /url\(|image/);

      // 优先级/权重行内步进：新建缺省 0/1，步进后持久化；权重触底时减号禁用。
      const priorityStepper = okRow.getByTestId('channel-priority-stepper');
      await expect(priorityStepper.locator('input')).toHaveValue('0');
      await priorityStepper.hover();
      await priorityStepper.getByRole('button', { name: 'Increase' }).click();
      await expect(priorityStepper.locator('input')).toHaveValue('1');
      const weightStepper = okRow.getByTestId('channel-weight-stepper');
      await expect(weightStepper.locator('input')).toHaveValue('1');
      await weightStepper.hover();
      await expect(weightStepper.getByRole('button', { name: 'Decrease' })).toBeDisabled();

      await page.getByTestId('channels-search').fill('ok-channel');
      await expect(okRow).toBeVisible();
      await page.getByTestId('channels-search').fill('gpt-4o-mini');
      await expect(okRow).toBeVisible();
      await page.getByTestId('channels-search').fill('no-such-channel');
      await expect(okRow).toHaveCount(0);
      await page.getByTestId('channels-search').fill('');
      await expect(okRow).toBeVisible();

      // 二次同步：已入清单显示「已选择」；取消勾选变「将取消」；全选/反选作用于可见行。
      await okRow.getByTestId('channel-edit').click();
      await page.getByTestId('channel-sync-models').click();
      await expect(miniRow.getByTestId('channel-sync-status-selected')).toBeVisible();
      await expect(fullRow.getByTestId('channel-sync-status-unselected')).toBeVisible();
      await miniRow.click();
      await expect(miniRow.getByTestId('channel-sync-status-willRemove')).toBeVisible();
      await page.getByTestId('channel-sync-select-all').click();
      await expect(miniRow.getByTestId('channel-sync-status-selected')).toBeVisible();
      await expect(fullRow.getByTestId('channel-sync-status-willAdd')).toBeVisible();
      await page.getByTestId('channel-sync-invert').click();
      await expect(miniRow.getByTestId('channel-sync-status-willRemove')).toBeVisible();
      await expect(fullRow.getByTestId('channel-sync-status-unselected')).toBeVisible();
      // 状态筛选：勾「将取消」只剩 mini；清除筛选恢复，勾选即时生效。
      await page.getByTestId('channel-sync-filter').click();
      await page.getByTestId('channel-sync-filter-willRemove').click();
      await expect(miniRow).toBeVisible();
      await expect(fullRow).toHaveCount(0);
      await page.getByTestId('channel-sync-filter-clear').click();
      await expect(fullRow).toBeVisible();
      // 搜索过滤行；清空恢复。
      await page.getByTestId('channel-sync-search').fill('mini');
      await expect(fullRow).toHaveCount(0);
      await expect(miniRow).toBeVisible();
      await page.getByTestId('channel-sync-search').fill('');
      await expect(fullRow).toBeVisible();
      // 表头复选框全选可见行，返回后清单为两个模型。
      await page.getByTestId('channel-sync-select-all-head').click();
      await expect(miniRow.getByTestId('channel-sync-status-selected')).toBeVisible();
      await expect(fullRow.getByTestId('channel-sync-status-willAdd')).toBeVisible();
      await page.getByTestId('channel-sync-back').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);

      await page.getByTestId('channel-editor-tab-advanced').click();
      await page.locator('[id^="channel-editor-aliases"]').fill('mini=gpt-4o-mini');
      await page.getByTestId('channel-save').click();
      await expect(okRow).toContainText('gpt-4o');

      await clickRowAction(okRow, page, 'channel-test');
      await expect(okRow.getByTestId('channel-probe-result')).toHaveText(/Success · 200 · \d+ ms/);

      // 新建渠道缺省启用：禁用 → 状态徽章变更；再启用 → 恢复。
      await expect(okRow.getByTestId('channel-toggle-enabled')).toHaveText('Enabled');
      await okRow.getByTestId('channel-toggle-enabled').click();
      await expect(okRow.getByTestId('channel-toggle-enabled')).toHaveText('Disabled');
      await okRow.getByTestId('channel-toggle-enabled').click();
      await expect(okRow.getByTestId('channel-toggle-enabled')).toHaveText('Enabled');

      // 同步错误以独立浮窗展示：3s 自动消失；鼠标悬浮暂停计时。
      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-base-url"]').fill(errUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');
      await page.getByTestId('channel-sync-models').click();
      const syncError = page.getByTestId('channel-sync-error');
      await expect(syncError).toBeVisible();
      // 悬浮期间超过 3s 仍不消失；移开后按剩余时长消失。
      await syncError.hover();
      await page.waitForTimeout(3_500);
      await expect(syncError).toBeVisible();
      await page.mouse.move(0, 0);
      await expect(syncError).toBeHidden({ timeout: 5_000 });
      await page.getByTestId('channel-sync-back').click();
      await page.getByRole('button', { name: 'Cancel' }).click();

      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-name"]').fill('fail-channel');
      await page.locator('[id^="channel-editor-base-url"]').fill(failUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');
      await page.getByTestId('channel-sync-models').click();
      await page.locator('[data-testid="channel-sync-row"][data-model="gpt-4o-mini"]').click();
      await page.getByTestId('channel-sync-back').click();
      await page.getByTestId('channel-save').click();

      const failRow = page.locator('[data-testid="channel-row"][data-channel-name="fail-channel"]');
      await expect(failRow).toBeVisible();
      await clickRowAction(failRow, page, 'channel-test');
      await expect(failRow.getByTestId('channel-probe-result')).toHaveText(/Failed · 500 · \d+ ms/);

      // 三开窗校验：chip 可点击复制、可删除；高级设置字段保持；改名保存。
      await okRow.getByTestId('channel-edit').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);
      // 点击 chip 复制模型名到剪贴板。
      await page.locator('[data-testid="channel-model-chip"][data-model="gpt-4o-mini"]').click();
      await expect
        .poll(() => page.evaluate(() => navigator.clipboard.readText()))
        .toBe('gpt-4o-mini');
      await page
        .locator('[data-testid="channel-model-chip"][data-model="gpt-4o"]')
        .getByTestId('channel-model-remove')
        .click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(1);
      await expect(page.getByTestId('channel-model-count')).toHaveText('1');
      await expect(page.locator('[id^="channel-editor-priority"]')).toHaveCount(0);
      await expect(page.locator('[id^="channel-editor-weight"]')).toHaveCount(0);
      await page.getByTestId('channel-editor-tab-advanced').click();
      await expect(page.locator('[id^="channel-editor-aliases"]')).toHaveValue('mini=gpt-4o-mini');
      await expect(page.locator('[id^="channel-editor-timeout-ms"]')).toHaveValue('5000');
      await expect(page.locator('[id^="channel-editor-max-retries"]')).toHaveValue('1');

      // 改名：保存后旧行消失，新名行接替其位。
      await page.getByTestId('channel-editor-tab-basic').click();
      await page.locator('[id^="channel-editor-name"]').fill('ok-channel-v2');
      await page.getByTestId('channel-save').click();
      await expect(okRow).toHaveCount(0);
      const renamedRow = page.locator(
        '[data-testid="channel-row"][data-channel-name="ok-channel-v2"]',
      );
      await expect(renamedRow).toBeVisible();

      await clickRowAction(renamedRow, page, 'channel-delete');
      await page.getByRole('dialog').getByTestId('channel-delete-confirm').click();
      await expect(renamedRow).toHaveCount(0);
    } finally {
      await okUpstream.close();
      await failUpstream.close();
      await errUpstream.close();
    }
  });
});
