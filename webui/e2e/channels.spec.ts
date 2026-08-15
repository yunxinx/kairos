import { authedTest as test, expect } from './fixtures';
import { clickRowAction } from './helpers/table';
import { startProbeUpstream } from './helpers/upstream';

test.describe.configure({ mode: 'serial' });

test.describe('channel resource page', () => {
  test('creates, edits, probes, and deletes a channel', async ({ page }) => {
    const okUpstream = await startProbeUpstream(200);
    const failUpstream = await startProbeUpstream(500);
    try {
      await page.goto('/channel');
      await expect(page.getByRole('heading', { name: /channels/i })).toBeVisible();

      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-name"]').fill('ok-channel');
      await page.locator('[id^="channel-editor-base-url"]').fill(okUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');
      await page.locator('[id^="channel-editor-models"]').fill('gpt-4o-mini');
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

      await okRow.getByTestId('channel-edit').click();
      await page.locator('[id^="channel-editor-models"]').fill('gpt-4o-mini\ngpt-4o');
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

      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-name"]').fill('fail-channel');
      await page.locator('[id^="channel-editor-base-url"]').fill(failUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');
      await page.locator('[id^="channel-editor-models"]').fill('gpt-4o-mini');
      await page.getByTestId('channel-save').click();

      const failRow = page.locator('[data-testid="channel-row"][data-channel-name="fail-channel"]');
      await expect(failRow).toBeVisible();
      await clickRowAction(failRow, page, 'channel-test');
      await expect(failRow.getByTestId('channel-probe-result')).toHaveText(/Failed · 500 · \d+ ms/);

      await okRow.getByTestId('channel-edit').click();
      await expect(page.locator('[id^="channel-editor-models"]')).toHaveValue(
        'gpt-4o-mini\ngpt-4o',
      );
      await expect(page.locator('[id^="channel-editor-aliases"]')).toHaveValue('mini=gpt-4o-mini');
      await expect(page.locator('[id^="channel-editor-priority"]')).toHaveCount(0);
      await expect(page.locator('[id^="channel-editor-weight"]')).toHaveCount(0);
      await expect(page.locator('[id^="channel-editor-timeout-ms"]')).toHaveValue('5000');
      await expect(page.locator('[id^="channel-editor-max-retries"]')).toHaveValue('1');

      // 改名：保存后旧行消失，新名行接替其位。
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
    }
  });
});
