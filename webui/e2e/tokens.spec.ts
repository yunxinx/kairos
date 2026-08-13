import { authedTest as test, expect } from './fixtures';
import { E2E_ADMIN_KEY } from './helpers/gateway';
import { clickRowAction } from './helpers/table';

test.describe.configure({ mode: 'serial' });

test.describe('token resource page', () => {
  test('creates, searches, edits, adjusts balance, and deletes a token', async ({ page }) => {
    await page.goto('/token');
    await expect(page.getByRole('heading', { name: /tokens/i })).toBeVisible();

    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-key"]').fill('sk-e2e-alpha');
    await page.locator('[id^="token-editor-name"]').fill('Alpha token');
    await page.getByTestId('token-save').click();

    const row = page.locator('[data-testid="token-row"][data-token-key="sk-e2e-alpha"]');
    await expect(row).toBeVisible();
    await expect(row.getByTestId('token-balance')).toHaveText('$0');
    await expect(row.getByTestId('token-settled')).toHaveText('$0');

    await page.getByTestId('tokens-search').fill('Alpha');
    await expect(row).toBeVisible();
    await page.getByTestId('tokens-search').fill('no-such-token');
    await expect(row).toHaveCount(0);
    await page.getByTestId('tokens-search').fill('');
    await expect(row).toBeVisible();

    await clickRowAction(row, page, 'token-recharge');
    await page.locator('[id^="token-balance-amount"]').fill('1.5');
    await page.getByTestId('token-balance-save').click();
    await expect(row.getByTestId('token-balance')).toHaveText('$1.5');
    await expect(row.getByTestId('token-settled')).toHaveText('$0');

    await clickRowAction(row, page, 'token-deduct');
    await page.locator('[id^="token-balance-amount"]').fill('0.25');
    await page.getByTestId('token-balance-save').click();
    await expect(row.getByTestId('token-balance')).toHaveText('$1.25');

    const balanceResp = await page.request.post('/tokens/sk-e2e-alpha/balance', {
      headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
      data: { delta_usd_micros: 0 },
    });
    expect(balanceResp.ok()).toBeTruthy();
    const balance = (await balanceResp.json()) as {
      balance_usd_micros: number;
      settled_usd_micros: number;
    };
    expect(balance.balance_usd_micros).toBe(1_250_000);
    expect(balance.settled_usd_micros).toBe(0);

    await clickRowAction(row, page, 'token-edit');
    await page.locator('[id^="token-editor-name"]').fill('Alpha renamed');
    await page.getByTestId('token-save').click();
    await expect(row.getByText('Alpha renamed')).toBeVisible();
    await expect(row.getByTestId('token-balance')).toHaveText('$1.25');

    await clickRowAction(row, page, 'token-delete');
    await page.getByRole('dialog').getByTestId('token-delete-confirm').click();
    await expect(row).toHaveCount(0);
  });

  test('shows a readable conflict when creating a duplicate token key', async ({ page }) => {
    await page.goto('/token');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-key"]').fill('sk-e2e-dup');
    await page.locator('[id^="token-editor-name"]').fill('First');
    await page.getByTestId('token-save').click();
    await expect(
      page.locator('[data-testid="token-row"][data-token-key="sk-e2e-dup"]'),
    ).toBeVisible();

    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-key"]').fill('sk-e2e-dup');
    await page.locator('[id^="token-editor-name"]').fill('Second');
    await page.getByTestId('token-save').click();
    await expect(page.getByTestId('token-editor-error')).toContainText(/sk-e2e-dup/);
  });

  test('shows a readable not_found when adjusting a deleted token', async ({ page }) => {
    await page.goto('/token');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-key"]').fill('sk-e2e-gone');
    await page.locator('[id^="token-editor-name"]').fill('Gone');
    await page.getByTestId('token-save').click();
    const row = page.locator('[data-testid="token-row"][data-token-key="sk-e2e-gone"]');
    await expect(row).toBeVisible();

    const deleted = await page.request.delete('/tokens/sk-e2e-gone', {
      headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
    });
    expect(deleted.ok()).toBeTruthy();

    await clickRowAction(row, page, 'token-recharge');
    await page.locator('[id^="token-balance-amount"]').fill('1');
    await page.getByTestId('token-balance-save').click();
    await expect(page.getByTestId('token-balance-error')).toContainText(/sk-e2e-gone/);
  });
});
