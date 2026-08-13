import { authedTest as test, expect } from './fixtures';
import { E2E_ADMIN_KEY } from './helpers/gateway';
import { clickRowAction } from './helpers/table';

test.describe.configure({ mode: 'serial' });

test.describe('pricing resource page', () => {
  test('creates, edits without precision loss, and deletes a price', async ({ page }) => {
    await page.goto('/pricing');
    await expect(page.getByRole('heading', { name: /pricing/i })).toBeVisible();

    await page.getByTestId('pricing-create-entry').click();
    await page.locator('[id^="pricing-editor-model"]').fill('gpt-4o-mini');
    await page.locator('[id^="pricing-editor-input"]').fill('1.000001');
    await page.locator('[id^="pricing-editor-output"]').fill('2.5');
    await page.locator('[id^="pricing-editor-cache-read"]').fill('0.000001');
    await page.locator('[id^="pricing-editor-cache-write"]').fill('');
    await page.getByTestId('pricing-save-entry').click();

    const row = page.locator('[data-testid="price-row"][data-price-model="gpt-4o-mini"]');
    await expect(row).toBeVisible();
    await expect(row.getByTestId('price-input')).toHaveText('$1.000001');
    await expect(row.getByTestId('price-output')).toHaveText('$2.5');
    await expect(row.getByTestId('price-cache-read')).toHaveText('$0.000001');
    await expect(row.getByTestId('price-cache-write')).toHaveText('—');

    const listed = await page.request.get('/prices', {
      headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
    });
    const prices = (await listed.json()) as Array<{
      model: string;
      input_micros: number;
      output_micros: number;
      cache_read_micros: number | null;
      cache_write_micros: number | null;
    }>;
    const saved = prices.find((item) => item.model === 'gpt-4o-mini');
    expect(saved?.input_micros).toBe(1_000_001);
    expect(saved?.output_micros).toBe(2_500_000);
    expect(saved?.cache_read_micros).toBe(1);
    expect(saved?.cache_write_micros).toBeNull();

    await clickRowAction(row, page, 'pricing-edit-entry');
    await expect(page.locator('[id^="pricing-editor-input"]')).toHaveValue('1.000001');
    await expect(page.locator('[id^="pricing-editor-output"]')).toHaveValue('2.5');
    await expect(page.locator('[id^="pricing-editor-cache-read"]')).toHaveValue('0.000001');
    await expect(page.locator('[id^="pricing-editor-cache-write"]')).toHaveValue('');
    await page.locator('[id^="pricing-editor-output"]').fill('3.25');
    await page.getByTestId('pricing-save-entry').click();
    await expect(row.getByTestId('price-output')).toHaveText('$3.25');

    await clickRowAction(row, page, 'pricing-delete-entry');
    await page.getByRole('dialog').getByTestId('pricing-delete-confirm').click();
    await expect(row).toHaveCount(0);
  });
});
