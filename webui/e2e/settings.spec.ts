import { authedTest as test, expect } from './fixtures';
import { E2E_PROTOCOL_PORT } from './helpers/gateway';

test.describe.configure({ mode: 'serial' });

test.describe('settings page', () => {
  test('saves max request bytes and the new limit takes effect immediately', async ({
    page,
    request,
  }) => {
    await page.goto('/config');
    await expect(page.getByRole('heading', { name: /settings/i })).toBeVisible();

    await page.locator('#settings-max-request-bytes').fill('100');
    await page.getByTestId('settings-save').click();
    await expect(page.getByTestId('settings-save-success')).toBeVisible();

    const oversized = 'x'.repeat(2000);
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
    await expect(page.getByTestId('settings-save-error')).toContainText(
      /max_request_bytes 必须大于 0/,
    );
  });
});
