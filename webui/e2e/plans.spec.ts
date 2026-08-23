import { authedTest as test, expect } from './fixtures';
import { clickRowAction } from './helpers/table';

test.describe.configure({ mode: 'serial' });

test.describe('plans page', () => {
  test('creates, edits, shares, and deletes a plan', async ({ page }) => {
    await page.goto('/plans');
    await expect(page.getByRole('heading', { name: /plans/i })).toBeVisible();

    await page.getByTestId('create-plan').click();
    await page.getByTestId('plan-internal-name').fill('e2e-plan');
    await page.getByTestId('plan-display-name').fill('E2E Plan');
    await page.getByTestId('plan-discount').fill('80');
    await page.getByTestId('plan-grant').fill('1');
    await page.getByTestId('plan-shared-switch').click();
    await page.getByTestId('plan-save').click();

    const row = page.locator('[data-testid="plan-row"]', { hasText: 'e2e-plan' });
    await expect(row).toBeVisible();
    await expect(row).toContainText('E2E Plan');
    await expect(row.getByTestId('plan-discount-cell')).toContainText('80%');
    await expect(row.getByTestId('plan-shared-badge')).toBeVisible();

    await row.getByTestId('plan-edit').click();
    await page.getByTestId('plan-display-name').fill('E2E Plan Edited');
    await page.getByTestId('plan-save').click();
    await expect(row).toContainText('E2E Plan Edited');

    await clickRowAction(row, page, 'plan-delete');
    await page.getByRole('dialog').getByTestId('plan-delete-confirm').click();
    await expect(row).toHaveCount(0);
  });
});
