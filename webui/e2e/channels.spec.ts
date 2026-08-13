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
      await page.locator('#channel-editor-name').fill('ok-channel');
      await page.locator('#channel-editor-base-url').fill(okUpstream.baseUrl);
      await page.locator('#channel-editor-api-key').fill('sk-upstream');
      await page.locator('#channel-editor-models').fill('gpt-4o-mini');
      await page.locator('#channel-editor-priority').fill('10');
      await page.locator('#channel-editor-weight').fill('2');
      await page.locator('#channel-editor-timeout-ms').fill('5000');
      await page.locator('#channel-editor-max-retries').fill('1');
      await page.getByTestId('channel-save').click();

      const okRow = page.locator('[data-testid="channel-row"][data-channel-name="ok-channel"]');
      await expect(okRow).toBeVisible();
      await expect(okRow).toContainText('gpt-4o-mini');

      await page.getByTestId('channels-search').fill('ok-channel');
      await expect(okRow).toBeVisible();
      await page.getByTestId('channels-search').fill('gpt-4o-mini');
      await expect(okRow).toBeVisible();
      await page.getByTestId('channels-search').fill('no-such-channel');
      await expect(okRow).toHaveCount(0);
      await page.getByTestId('channels-search').fill('');
      await expect(okRow).toBeVisible();

      await clickRowAction(okRow, page, 'channel-edit');
      await page.locator('#channel-editor-models').fill('gpt-4o-mini\ngpt-4o');
      await page.locator('#channel-editor-aliases').fill('mini=gpt-4o-mini');
      await page.getByTestId('channel-save').click();
      await expect(okRow).toContainText('gpt-4o');

      await okRow.getByTestId('channel-test').click();
      await expect(okRow.getByTestId('channel-probe-result')).toHaveText(/Success · 200 · \d+ ms/);

      await page.getByTestId('create-channel').click();
      await page.locator('#channel-editor-name').fill('fail-channel');
      await page.locator('#channel-editor-base-url').fill(failUpstream.baseUrl);
      await page.locator('#channel-editor-api-key').fill('sk-upstream');
      await page.locator('#channel-editor-models').fill('gpt-4o-mini');
      await page.getByTestId('channel-save').click();

      const failRow = page.locator('[data-testid="channel-row"][data-channel-name="fail-channel"]');
      await expect(failRow).toBeVisible();
      await failRow.getByTestId('channel-test').click();
      await expect(failRow.getByTestId('channel-probe-result')).toHaveText(/Failed · 500 · \d+ ms/);

      await clickRowAction(okRow, page, 'channel-edit');
      await expect(page.locator('#channel-editor-models')).toHaveValue('gpt-4o-mini\ngpt-4o');
      await expect(page.locator('#channel-editor-aliases')).toHaveValue('mini=gpt-4o-mini');
      await expect(page.locator('#channel-editor-priority')).toHaveValue('10');
      await expect(page.locator('#channel-editor-weight')).toHaveValue('2');
      await expect(page.locator('#channel-editor-timeout-ms')).toHaveValue('5000');
      await expect(page.locator('#channel-editor-max-retries')).toHaveValue('1');
      await page
        .locator('dialog')
        .getByRole('button', { name: /cancel/i })
        .click();

      await clickRowAction(okRow, page, 'channel-delete');
      await okRow.getByTestId('channel-delete-confirm').click();
      await expect(okRow).toHaveCount(0);
    } finally {
      await okUpstream.close();
      await failUpstream.close();
    }
  });
});
