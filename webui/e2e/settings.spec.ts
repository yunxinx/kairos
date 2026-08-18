import { authedTest as test, expect } from './fixtures';
import { E2E_ADMIN_KEY, E2E_PROTOCOL_PORT } from './helpers/gateway';

test.describe.configure({ mode: 'serial' });

test.describe('settings page', () => {
  test('saves max request bytes and the new limit takes effect immediately', async ({
    page,
    request,
  }) => {
    await page.goto('/config');
    await expect(page.getByRole('tablist', { name: /settings/i })).toBeVisible();

    await page.locator('#settings-max-request-bytes').fill('0.01');
    await page.getByTestId('settings-save').click();
    await expect(page.getByTestId('toast')).toContainText(/saved|已保存/);

    const oversized = 'x'.repeat(20_000);
    const resp = await request.post(`http://127.0.0.1:${E2E_PROTOCOL_PORT}/v1/chat/completions`, {
      data: {
        model: 'gpt-4o',
        messages: [{ role: 'user', content: oversized }],
      },
    });
    expect(resp.status()).toBe(413);
  });

  test('shows a readable invalid_body error for a zero body limit', async ({ page }) => {
    await page.goto('/config');
    await page.locator('#settings-max-request-bytes').fill('0');
    await page.getByTestId('settings-save').click();
    await expect(page.locator('[data-form-field="maxRequestBytes"]')).toContainText(
      /greater than 0|大于 0/,
    );
  });

  test('saves the full_body switch and keeps it after refresh', async ({ page, request }) => {
    await page.goto('/config');
    const checkbox = page.getByTestId('settings-full-body');
    await expect(checkbox).toBeVisible();
    if (await checkbox.isChecked()) {
      await checkbox.uncheck();
      await page.getByTestId('settings-save').click();
      await expect(page.getByTestId('toast')).toContainText(/saved|已保存/);
    }
    await checkbox.check();
    await page.getByTestId('settings-save').click();
    await expect(page.getByTestId('toast')).toContainText(/saved|已保存/);

    const resp = await request.get('/settings', {
      headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
    });
    expect(resp.ok()).toBeTruthy();
    expect((await resp.json()).full_body).toBe(true);

    await page.reload();
    await expect(page.getByTestId('settings-full-body')).toBeChecked();
    await expect(page.getByTestId('settings-log-body-max-bytes')).toBeVisible();
    await page
      .locator('[data-form-field="fullBody"]')
      .getByRole('button', { name: /format and requirements|格式与填写说明/ })
      .click();
    await expect(page.locator('#settings-full-body-guide')).toContainText(
      /plaintext in sqlite|明文写入 sqlite/i,
    );
  });
});
