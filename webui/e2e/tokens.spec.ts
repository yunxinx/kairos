import { authedTest as test, expect } from './fixtures';
import { clickRowAction } from './helpers/table';

test.describe.configure({ mode: 'serial' });

/** 系统生成 key 的形状：ks- 前缀 + 64 位大小写字母与数字。 */
const GENERATED_KEY_PATTERN = /^ks-[A-Za-z0-9]{64}$/;

test.describe('token resource page', () => {
  test('creates, edits definition fields, toggles status, and deletes a token', async ({
    page,
  }) => {
    await page.goto('/tokens');
    await expect(page.getByRole('heading', { name: /tokens/i })).toBeVisible();

    // 新建不接受指定 key：窗口内无 key 输入，key 由系统生成。
    await page.getByTestId('create-token').click();
    await expect(page.locator('[id^="token-editor-key"]')).toHaveCount(0);
    await page.locator('[id^="token-editor-name"]').fill('Alpha token');
    await page.getByTestId('token-editor-rpm').fill('60');
    await page.getByTestId('token-editor-initial-balance').fill('12');
    await page.getByTestId('token-save').click();

    const createdRow = page.locator('[data-testid="token-row"]', { hasText: 'Alpha token' });
    await expect(createdRow).toBeVisible();
    // 行内只展示掩码 key：完整 key 经复制按钮进剪贴板验证系统生成形状。
    await createdRow.getByTestId('token-copy-key').click();
    const tokenKey = await page.evaluate(() => navigator.clipboard.readText());
    expect(tokenKey).toMatch(GENERATED_KEY_PATTERN);
    // key 掩码展示：完整 key 不以明文出现在行内，且新建令牌未使用过。
    await expect(createdRow).not.toContainText(tokenKey);
    await expect(createdRow.getByTestId('token-toggle-enabled')).toHaveText('Enabled');
    await expect(createdRow.getByTestId('token-last-used')).toHaveText(/never used/i);
    await expect(createdRow.getByTestId('token-rpm')).toHaveText('60');

    await page.getByTestId('tokens-search').fill('Alpha');
    await expect(createdRow).toBeVisible();
    await page.getByTestId('tokens-search').fill('no-such-token');
    await expect(createdRow).toHaveCount(0);
    await page.getByTestId('tokens-search').fill('');
    await expect(createdRow).toBeVisible();

    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(tokenKey);
    await page.getByTestId('tokens-status-filter').click();
    await page
      .locator('[data-testid="tokens-status-filter-option"][data-value="disabled"]')
      .click();
    await expect(createdRow).toHaveCount(0);
    await page.getByTestId('tokens-status-filter-clear').click();
    await page.keyboard.press('Escape');
    await expect(createdRow).toBeVisible();

    await expect(createdRow.getByTestId('token-balance')).toHaveText('$12.00');

    // 编辑器包含余额调整面板
    await createdRow.getByTestId('token-edit').click();
    await expect(page.getByTestId('token-editor-rpm')).toHaveValue('60');
    await expect(page.getByTestId('token-editor-initial-balance')).toHaveCount(0);
    await expect(page.getByTestId('token-current-balance')).toHaveText('12');

    // 属性和相对余额调整由同一个 PUT 原子提交。
    await page.locator('[id^="token-editor-name"]').fill('Alpha renamed');
    await page.getByTestId('token-editor-rpm').fill('120');
    await page.getByTestId('token-balance-quick-add-5').click();
    const compoundUpdate = page.waitForRequest(
      (request) => request.method() === 'PUT' && /\/api\/tokens\/\d+$/.test(request.url()),
    );
    await page.getByTestId('token-save').click();
    const compoundBody = (await compoundUpdate).postDataJSON() as {
      name: string;
      rate_limit_rpm: number;
      balance_change: { action: string; delta_usd_micros: number };
    };
    expect(compoundBody).toMatchObject({
      name: 'Alpha renamed',
      rate_limit_rpm: 120,
      balance_change: { action: 'adjust', delta_usd_micros: 5_000_000 },
    });
    // 改名后按新名定位，避免名称变化导致定位器失效。
    const renamedRow = page.locator('[data-testid="token-row"]', { hasText: 'Alpha renamed' });
    await expect(renamedRow.getByTestId('token-rpm')).toHaveText('120');
    await expect(renamedRow.getByTestId('token-balance')).toHaveText('$17.00');

    // 禁用 → 状态徽章变更；再启用 → 恢复。
    await renamedRow.getByTestId('token-toggle-enabled').click();
    await expect(renamedRow.getByTestId('token-toggle-enabled')).toHaveText('Disabled');
    await renamedRow.getByTestId('token-toggle-enabled').click();
    await expect(renamedRow.getByTestId('token-toggle-enabled')).toHaveText('Enabled');

    await clickRowAction(renamedRow, page, 'token-delete');
    await page.getByRole('dialog').getByTestId('token-delete-confirm').click();
    await expect(renamedRow).toHaveCount(0);
  });

  test('creates each token with a unique system-generated key', async ({ page }) => {
    await page.goto('/tokens');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('First');
    await page.getByTestId('token-save').click();
    const firstRow = page.locator('[data-testid="token-row"]', { hasText: 'First' });
    await expect(firstRow).toBeVisible();
    await firstRow.getByTestId('token-copy-key').click();
    const firstKey = await page.evaluate(() => navigator.clipboard.readText());
    expect(firstKey).toMatch(GENERATED_KEY_PATTERN);

    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('Second');
    await page.getByTestId('token-save').click();
    const secondRow = page.locator('[data-testid="token-row"]', { hasText: 'Second' });
    await expect(secondRow).toBeVisible();
    await secondRow.getByTestId('token-copy-key').click();
    const secondKey = await page.evaluate(() => navigator.clipboard.readText());
    expect(secondKey).toMatch(GENERATED_KEY_PATTERN);
    expect(secondKey).not.toBe(firstKey);
  });

  test('adjusts token balance independently and switches finite modes explicitly', async ({
    page,
  }) => {
    await page.goto('/tokens');
    for (const name of ['LimitA', 'LimitB']) {
      await page.getByTestId('create-token').click();
      await page.locator('[id^="token-editor-name"]').fill(name);
      if (name === 'LimitA') {
        await page.getByTestId('token-editor-initial-balance').fill('10');
      }
      await page.getByTestId('token-save').click();
      await expect(page.locator('[data-testid="token-row"]', { hasText: name })).toBeVisible();
    }

    const rowA = page.locator('[data-testid="token-row"]', { hasText: 'LimitA' });
    const rowB = page.locator('[data-testid="token-row"]', { hasText: 'LimitB' });
    await expect(rowA.getByTestId('token-balance')).toHaveText('$10.00');
    await expect(rowB.getByTestId('token-balance')).toHaveText('Unlimited');

    await rowA.getByTestId('token-edit').click();
    await page.getByTestId('token-balance-quick-add-10').click();
    await expect(page.getByTestId('token-expected-balance')).toHaveText('20');
    await page.getByTestId('token-save').click();
    await expect(rowA.getByTestId('token-balance')).toHaveText('$20.00');
    await expect(rowB.getByTestId('token-balance')).toHaveText('Unlimited');

    await rowB.getByTestId('token-edit').click();
    await page.getByTestId('token-balance-mode-finite').click();
    await page.getByTestId('token-balance-amount').fill('7');
    await page.getByTestId('token-save').click();
    await expect(rowB.getByTestId('token-balance')).toHaveText('$7.00');

    await rowA.getByTestId('token-edit').click();
    await page.getByTestId('token-balance-mode-unlimited').click();
    await page.getByTestId('token-save').click();
    await expect(rowA.getByTestId('token-balance')).toHaveText('Unlimited');
    await expect(rowB.getByTestId('token-balance')).toHaveText('$7.00');
  });

  test('rotates the balance operation id only when the command changes', async ({ page }) => {
    await page.goto('/tokens');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('Retry balance');
    await page.getByTestId('token-editor-initial-balance').fill('10');
    await page.getByTestId('token-save').click();
    const row = page.locator('[data-testid="token-row"]', { hasText: 'Retry balance' });
    await expect(row).toBeVisible();

    const operationIds: string[] = [];
    let failedRequests = 0;
    await page.route('**/api/tokens/*', async (route) => {
      const request = route.request();
      if (request.method() !== 'PUT') {
        await route.continue();
        return;
      }
      const body = request.postDataJSON() as {
        balance_change?: { operation_id: string };
      };
      if (body.balance_change) operationIds.push(body.balance_change.operation_id);
      if (failedRequests < 2) {
        failedRequests += 1;
        await route.fulfill({
          status: 500,
          contentType: 'application/json',
          body: JSON.stringify({ error: { code: 'internal', message: 'retry test' } }),
        });
        return;
      }
      await route.continue();
    });

    await row.getByTestId('token-edit').click();
    await page.getByTestId('token-balance-amount').fill('1');
    await page.getByTestId('token-save').click();
    await expect(page.getByTestId('toast').getByText('retry test')).toBeVisible();

    await page.getByTestId('token-save').click();
    await expect.poll(() => operationIds.length).toBe(2);
    expect(operationIds[1]).toBe(operationIds[0]);

    await page.getByTestId('token-balance-amount').fill('2');
    await page.getByTestId('token-save').click();
    await expect(row.getByTestId('token-balance')).toHaveText('$12.00');
    expect(operationIds).toHaveLength(3);
    expect(operationIds[2]).not.toBe(operationIds[1]);
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
