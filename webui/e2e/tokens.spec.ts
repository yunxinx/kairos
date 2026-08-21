import { authedTest as test, expect } from './fixtures';
import { clickRowAction } from './helpers/table';

test.describe.configure({ mode: 'serial' });

/** 系统生成 key 的形状：ks- 前缀 + 64 位大小写字母与数字。 */
const GENERATED_KEY_PATTERN = /^ks-[A-Za-z0-9]{64}$/;

test.describe('token resource page', () => {
  test('creates, edits with balance draft, toggles status, and deletes a token', async ({
    page,
  }) => {
    await page.goto('/tokens');
    await expect(page.getByRole('heading', { name: /tokens/i })).toBeVisible();

    // 新建不接受指定 key：窗口内无 key 输入，key 由系统生成。
    await page.getByTestId('create-token').click();
    await expect(page.locator('[id^="token-editor-key"]')).toHaveCount(0);
    await page.locator('[id^="token-editor-name"]').fill('Alpha token');
    await page.getByTestId('token-editor-rpm').fill('60');
    await page.getByTestId('token-save').click();

    const createdRow = page.locator('[data-testid="token-row"]', { hasText: 'Alpha token' });
    await expect(createdRow).toBeVisible();
    const tokenKey = await createdRow.getAttribute('data-token-key');
    expect(tokenKey).toMatch(GENERATED_KEY_PATTERN);
    // 改名后按 key 定位，避免名称变化导致定位器失效。
    const row = page.locator(`[data-testid="token-row"][data-token-key="${tokenKey}"]`);
    // key 掩码展示：完整 key 不以明文出现在行内，且新建令牌未使用过。
    await expect(row).not.toContainText(tokenKey as string);
    await expect(row.getByTestId('token-toggle-enabled')).toHaveText('Enabled');
    await expect(row.getByTestId('token-last-used')).toHaveText(/never used/i);
    await expect(row.getByTestId('token-rpm')).toHaveText('60 RPM');

    await page.getByTestId('tokens-search').fill('Alpha');
    await expect(row).toBeVisible();
    await page.getByTestId('tokens-search').fill('no-such-token');
    await expect(row).toHaveCount(0);
    await page.getByTestId('tokens-search').fill('');
    await expect(row).toBeVisible();

    await row.getByTestId('token-copy-key').click();
    await expect(row.getByTestId('token-copy-key')).toHaveAttribute('aria-label', /copied/i);
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(tokenKey);

    await page.getByTestId('tokens-status-filter').click();
    await page
      .locator('[data-testid="tokens-status-filter-option"][data-value="disabled"]')
      .click();
    await expect(row).toHaveCount(0);
    await page.getByTestId('tokens-status-filter-clear').click();
    await page.keyboard.press('Escape');
    await expect(row).toBeVisible();

    // 快捷档位累计成差额：算式预览 `+5 = ...`，保存后才落库。
    await row.getByTestId('token-edit').click();
    await expect(page.getByTestId('token-editor-rpm')).toHaveValue('60');
    await page.getByTestId('token-quick-add-5').click();
    await expect(page.getByTestId('token-balance-delta')).toHaveText('+5');

    // 取消只清空快捷差额：算式消失。
    await page.getByTestId('token-quick-cancel').click();
    await expect(page.getByTestId('token-balance-delta')).toHaveCount(0);

    await page.getByTestId('token-quick-add-5').click();
    await expect(page.getByTestId('token-balance-result')).toBeVisible();

    // 修改名称与 RPM
    await page.locator('[id^="token-editor-name"]').fill('Alpha renamed');
    await page.getByTestId('token-editor-rpm').fill('120');
    await page.getByTestId('token-save').click();
    await expect(row.getByText('Alpha renamed')).toBeVisible();
    await expect(row.getByTestId('token-rpm')).toHaveText('120 RPM');

    // 禁用 → 状态徽章变更；再启用 → 恢复。
    await row.getByTestId('token-toggle-enabled').click();
    await expect(row.getByTestId('token-toggle-enabled')).toHaveText('Disabled');
    await row.getByTestId('token-toggle-enabled').click();
    await expect(row.getByTestId('token-toggle-enabled')).toHaveText('Enabled');

    await clickRowAction(row, page, 'token-delete');
    await page.getByRole('dialog').getByTestId('token-delete-confirm').click();
    await expect(row).toHaveCount(0);
  });

  test('creates each token with a unique system-generated key', async ({ page }) => {
    await page.goto('/tokens');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('First');
    await page.getByTestId('token-save').click();
    const firstRow = page.locator('[data-testid="token-row"]', { hasText: 'First' });
    await expect(firstRow).toBeVisible();
    const firstKey = await firstRow.getAttribute('data-token-key');
    expect(firstKey).toMatch(GENERATED_KEY_PATTERN);

    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('Second');
    await page.getByTestId('token-save').click();
    const secondRow = page.locator('[data-testid="token-row"]', { hasText: 'Second' });
    await expect(secondRow).toBeVisible();
    const secondKey = await secondRow.getAttribute('data-token-key');
    expect(secondKey).toMatch(GENERATED_KEY_PATTERN);
    expect(secondKey).not.toBe(firstKey);
  });

  test('rejects a negative target balance without saving', async ({ page }) => {
    await page.goto('/tokens');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('Negative');
    await page.getByTestId('token-save').click();
    const row = page.locator('[data-testid="token-row"]', { hasText: 'Negative' });
    await expect(row).toBeVisible();

    // 目标余额不允许为负：保存被校验拦截，窗口保持打开。
    await row.getByTestId('token-edit').click();
    await page.getByTestId('token-editor-amount').fill('-1');
    await page.getByTestId('token-save').click();
    await expect(page.locator('[data-form-field="amount"] .form-field-hint')).toBeVisible();
  });

  test('bulk selects tokens and deletes them from the floating bulk bar', async ({ page }) => {
    await page.goto('/tokens');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('Bulk A');
    await page.getByTestId('token-save').click();
    await expect(page.locator('[data-testid="token-row"]', { hasText: 'Bulk A' })).toBeVisible();

    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('Bulk B');
    await page.getByTestId('token-save').click();
    await expect(page.locator('[data-testid="token-row"]', { hasText: 'Bulk B' })).toBeVisible();

    // 搜索收敛到 Bulk 行，全选只作用于可见行。
    await page.getByTestId('tokens-search').fill('Bulk');
    await page
      .locator('[data-testid="token-row"]', { hasText: 'Bulk A' })
      .getByTestId('token-select')
      .click();
    await expect(page.getByTestId('tokens-bulk-bar')).toBeVisible();
    await expect(page.getByTestId('bulk-count')).toHaveText('1 selected');

    await page.getByTestId('tokens-select-all').click();
    await expect(page.getByTestId('bulk-count')).toHaveText('2 selected');

    // 浮动条删除 → 确认浮窗 → 行消失、浮动条收起。
    await page.getByTestId('tokens-bulk-delete').click();
    await page.getByRole('dialog').getByTestId('token-bulk-delete-confirm').click();
    await expect(page.locator('[data-testid="token-row"]')).toHaveCount(0);
    await expect(page.getByTestId('tokens-bulk-bar')).toHaveCount(0);
  });
});
