import { authedTest as test, expect } from './fixtures';
import { E2E_PROTOCOL_PORT } from './helpers/gateway';
import { e2eRootHeaders } from './helpers/session';
import { seedToken } from './helpers/models';

test.describe.configure({ mode: 'serial' });

test.describe('settings page', () => {
  test('saves max request bytes and the new limit takes effect immediately', async ({
    page,
    request,
  }) => {
    await page.goto('/settings');
    await expect(page.getByRole('tablist', { name: /settings/i })).toBeVisible();

    await page.locator('#settings-max-request-bytes').fill('0.01');
    await page.getByTestId('settings-save').click();
    await expect(page.getByTestId('toast')).toContainText(/saved|已保存/);

    const token = await seedToken(page, { name: 'e2e-settings-body-limit' });
    const oversized = 'x'.repeat(20_000);
    const resp = await request.post(`http://127.0.0.1:${E2E_PROTOCOL_PORT}/v1/chat/completions`, {
      headers: { Authorization: `Bearer ${token.token_key}` },
      data: {
        model: 'gpt-4o',
        messages: [{ role: 'user', content: oversized }],
      },
    });
    expect(resp.status()).toBe(413);
  });

  test('shows a readable invalid_body error for a zero body limit', async ({ page }) => {
    await page.goto('/settings');
    await page.locator('#settings-max-request-bytes').fill('0');
    await page.getByTestId('settings-save').click();
    await expect(page.locator('[data-form-field="maxRequestBytes"]')).toContainText(
      /greater than 0|大于 0/,
    );
  });

  test('saves the full_body switch and keeps it after refresh', async ({ page }) => {
    await page.goto('/settings');
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

    const resp = await page.request.get('/api/settings', {
      headers: await e2eRootHeaders(page),
    });
    expect(resp.ok()).toBeTruthy();
    expect((await resp.json()).full_body).toBe(true);

    await page.reload();
    await expect(page.getByTestId('settings-full-body')).toBeChecked();
    await expect(page.getByTestId('settings-log-body-max-bytes')).toBeVisible();
    await page.locator('[data-form-field="fullBody"] .field-info-hint-trigger').click();
    await expect(
      page.getByRole('dialog', { name: /format and requirements|格式与填写说明/i }),
    ).toContainText(/plaintext in sqlite|明文写入 sqlite/i);
  });
});
