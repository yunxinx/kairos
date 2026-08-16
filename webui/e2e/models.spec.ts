import { authedTest as test, expect } from './fixtures';
import { E2E_ADMIN_KEY } from './helpers/gateway';
import { seedChannel, seedModelGroup, seedPrice, seedToken } from './helpers/models';
import { seedRequestLogs } from './helpers/seed-logs';
import { clickRowAction } from './helpers/table';
import { MODELS_DEV_CATALOG_URL } from '../src/lib/catalog';

test.describe.configure({ mode: 'serial' });

test.describe('models page', () => {
  test('four parallel tabs default to inventory; selection does not cross tabs', async ({
    page,
  }) => {
    await seedChannel(page, { name: 'e2e-tab-channel', models: ['e2e-tab-model'] });
    await page.goto('/models');
    await expect(page.getByRole('heading', { name: /^models$/i })).toBeVisible();
    await expect(page.getByTestId('models-tab-inventory')).toHaveAttribute('data-state', 'active');
    await expect(page.getByTestId('pricing-create-entry')).toHaveCount(0);

    const row = page.locator('[data-testid="inventory-row"][data-model="e2e-tab-model"]');
    await expect(row).toBeVisible();
    await row.getByTestId('inventory-select').click();
    await expect(page.getByTestId('inventory-bulk-bar')).toBeVisible();

    await page.getByTestId('models-tab-unified').click();
    await expect(page.getByTestId('models-tab-unified')).toHaveAttribute('data-state', 'active');
    await expect(page.getByTestId('inventory-bulk-bar')).toHaveCount(0);
    await expect(page.getByTestId('unified-bulk-bar')).toHaveCount(0);

    await page.getByTestId('models-tab-groups').click();
    await expect(page.getByTestId('models-tab-groups')).toHaveAttribute('data-state', 'active');
    await page.reload();
    await expect(page.getByTestId('models-tab-inventory')).toHaveAttribute('data-state', 'active');
    await expect(page.getByTestId('models-tab-groups')).not.toHaveAttribute('data-state', 'active');
  });

  test('inventory is derived from channels; unpriced is highlighted; prices edit without typing a new row', async ({
    page,
  }) => {
    await seedChannel(page, {
      name: 'e2e-inv-channel',
      models: ['e2e-inv-mini'],
      model_aliases: { 'e2e-inv-fast': 'e2e-inv-mini' },
    });
    await page.goto('/models');

    const canonical = page.locator('[data-testid="inventory-row"][data-model="e2e-inv-mini"]');
    const alias = page.locator('[data-testid="inventory-row"][data-model="e2e-inv-fast"]');
    await expect(canonical).toBeVisible();
    await expect(canonical.getByTestId('inventory-unpriced')).toBeVisible();
    await expect(canonical.getByTestId('inventory-channel-chip')).toHaveText('e2e-inv-channel');
    await expect(alias.getByTestId('inventory-alias')).toContainText('e2e-inv-fast → e2e-inv-mini');
    await expect(alias.getByTestId('inventory-unpriced')).toBeVisible();

    await clickRowAction(canonical, page, 'pricing-edit-entry');
    await expect(page.locator('[id^="pricing-editor-model"]')).toHaveValue('e2e-inv-mini');
    await expect(page.locator('[id^="pricing-editor-model"]')).toBeDisabled();
    await page.locator('[id^="pricing-editor-input"]').fill('1.000001');
    await page.locator('[id^="pricing-editor-output"]').fill('2.5');
    await page.locator('[id^="pricing-editor-cache-read"]').fill('0.000001');
    await page.locator('[id^="pricing-editor-cache-write"]').fill('');
    await page.getByTestId('pricing-save-entry').click();

    await expect(canonical.getByTestId('inventory-unpriced')).toHaveCount(0);
    await expect(canonical.getByTestId('price-input')).toHaveText('$1.000001');
    await expect(canonical.getByTestId('price-output')).toHaveText('$2.5');
    await expect(canonical.getByTestId('price-cache-read')).toHaveText('$0.000001');
    await expect(canonical.getByTestId('price-cache-write')).toHaveText('—');

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
    const saved = prices.find((item) => item.model === 'e2e-inv-mini');
    expect(saved?.input_micros).toBe(1_000_001);
    expect(saved?.output_micros).toBe(2_500_000);
    expect(saved?.cache_read_micros).toBe(1);
    expect(saved?.cache_write_micros).toBeNull();

    await clickRowAction(canonical, page, 'pricing-edit-entry');
    await expect(page.locator('[id^="pricing-editor-input"]')).toHaveValue('1.000001');
    await page.locator('[id^="pricing-editor-output"]').fill('3.25');
    await page.getByTestId('pricing-save-entry').click();
    await expect(canonical.getByTestId('price-output')).toHaveText('$3.25');

    await clickRowAction(canonical, page, 'pricing-delete-entry');
    await page.getByRole('dialog').getByTestId('pricing-delete-confirm').click();
    await expect(canonical).toBeVisible();
    await expect(canonical.getByTestId('inventory-unpriced')).toBeVisible();
  });

  test('catalog fill previews, requires a host pick, and only writes empty tiers', async ({
    page,
  }) => {
    await seedChannel(page, { name: 'e2e-cat-channel', models: ['e2e-cat-mini'] });
    await seedPrice(page, {
      model: 'e2e-cat-mini',
      input_micros: 9_000_000,
      output_micros: 8_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await page.route(MODELS_DEV_CATALOG_URL, async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          openai: {
            id: 'openai',
            name: 'OpenAI',
            models: {
              'e2e-cat-mini': {
                id: 'e2e-cat-mini',
                cost: { input: 0.15, output: 0.6, cache_read: 0.075 },
              },
            },
          },
          cortecs: {
            id: 'cortecs',
            name: 'Cortecs',
            models: {
              'e2e-cat-mini': {
                id: 'e2e-cat-mini',
                cost: { input: 0.159, output: 0.638, cache_read: 0.081 },
              },
            },
          },
        }),
      });
    });

    await page.goto('/models');
    const row = page.locator('[data-testid="inventory-row"][data-model="e2e-cat-mini"]');
    await row.getByTestId('inventory-select').click();
    await page.getByTestId('inventory-catalog-fill').click();
    await expect(page.getByTestId('catalog-preview')).toBeVisible();
    await expect(page.getByTestId('catalog-preview-status')).toHaveText(/pick a host/i);
    await expect(page.getByTestId('catalog-confirm')).toBeDisabled();

    await page.getByTestId('catalog-host-select').click();
    await page.getByRole('option', { name: 'OpenAI', exact: true }).click();
    await expect(page.getByTestId('catalog-preview-status')).toHaveText(/will write/i);
    await page.getByTestId('catalog-confirm').click();

    await expect(row.getByTestId('price-input')).toHaveText('$9');
    await expect(row.getByTestId('price-output')).toHaveText('$8');
    await expect(row.getByTestId('price-cache-read')).toHaveText('$0.075');
    await expect(row.getByTestId('price-cache-write')).toHaveText('—');
  });

  test('coding token, group, and unified id can coexist; downstream tab and logs show outbound', async ({
    page,
  }) => {
    await seedChannel(page, {
      name: 'e2e-coding-channel',
      models: ['e2e-code-mini', 'e2e-code-haiku'],
    });
    await seedPrice(page, {
      model: 'e2e-code-mini',
      input_micros: 150_000,
      output_micros: 600_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedPrice(page, {
      model: 'e2e-code-haiku',
      input_micros: 250_000,
      output_micros: 1_250_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });

    await page.goto('/models');
    await page.getByTestId('models-tab-unified').click();
    await page.getByTestId('unified-create').click();
    await page.getByTestId('unified-editor-id').fill('coding');
    await page.getByTestId('unified-add-select').click();
    await page.getByRole('option', { name: 'e2e-code-mini', exact: true }).click();
    await page.getByTestId('unified-add-member').click();
    await page.getByTestId('unified-add-select').click();
    await page.getByRole('option', { name: 'e2e-code-haiku', exact: true }).click();
    await page.getByTestId('unified-add-member').click();
    await page.getByTestId('unified-member-down').first().click();
    await page.getByTestId('unified-hide-switch').click();
    await page.getByTestId('unified-save').click();
    const unifiedRow = page.locator('[data-testid="unified-row"][data-unified-id="coding"]');
    await expect(unifiedRow).toBeVisible();
    await expect(unifiedRow.getByTestId('unified-members')).toHaveText(
      'e2e-code-haiku → e2e-code-mini',
    );

    await page.getByTestId('models-tab-visible').click();
    await expect(page.locator('[data-testid="visible-model"][data-model="coding"]')).toBeVisible();
    await expect(page.getByTestId('visible-hidden-members')).toContainText('e2e-code-haiku');
    await expect(page.getByTestId('visible-hidden-members')).toContainText('e2e-code-mini');

    await page.getByTestId('models-tab-groups').click();
    await page.getByTestId('group-create').click();
    await page.getByTestId('group-editor-name').fill('coding');
    await page
      .locator('[data-testid="group-model-option"][data-model="coding"]')
      .getByTestId('group-model-check')
      .click();
    await page.getByTestId('group-save').click();
    await expect(page.locator('[data-testid="group-row"][data-group-name="coding"]')).toBeVisible();

    await page.getByTestId('models-tab-visible').click();
    await page.getByTestId('visible-group-select').click();
    await page.getByRole('option', { name: 'coding', exact: true }).click();
    await expect(page.getByTestId('visible-model')).toHaveCount(1);
    await expect(page.locator('[data-testid="visible-model"][data-model="coding"]')).toBeVisible();
    await expect(page.getByTestId('visible-unified-order')).toContainText('e2e-code-haiku');

    await page.goto('/token');
    await page.getByTestId('create-token').click();
    await page.locator('[id^="token-editor-name"]').fill('coding');
    await page.getByTestId('token-editor-group').click();
    await page.getByRole('option', { name: 'coding', exact: true }).click();
    await page.getByTestId('token-save').click();
    const tokenRow = page.locator('[data-testid="token-row"]', { hasText: 'coding' });
    await expect(tokenRow).toBeVisible();
    await expect(tokenRow.getByTestId('token-model-group')).toHaveText('coding');

    seedRequestLogs([
      {
        created_at: Date.now(),
        token_key: 'sk-e2e-coding',
        token_name: 'coding',
        model: 'coding',
        outbound_model: 'e2e-code-haiku',
        channel: 'e2e-coding-channel',
        status_code: 200,
      },
    ]);
    await page.goto('/requests');
    await page.locator('#logs-search').fill('sk-e2e-coding');
    const logRow = page.locator('[data-testid="log-row"][data-model="coding"]');
    await expect(logRow.getByTestId('log-model')).toHaveText('coding');
    await logRow.getByTestId('log-expand').click();
    await expect(page.getByTestId('log-outbound-model')).toHaveText('e2e-code-haiku');
    await expect(page.getByTestId('log-detail-channel')).toHaveText('e2e-coding-channel');
  });

  test('by-channel layout lists a shared model under every member channel', async ({ page }) => {
    await seedChannel(page, {
      name: 'e2e-layout-a',
      models: ['e2e-shared', 'e2e-only-a'],
    });
    await seedChannel(page, {
      name: 'e2e-layout-b',
      models: ['e2e-shared', 'e2e-only-b'],
    });
    await page.goto('/models');
    await page.getByTestId('inventory-layout-by-channel').click();
    await expect(
      page.locator('[data-testid="inventory-section"][data-channel="e2e-layout-a"]'),
    ).toBeVisible();
    await expect(
      page.locator(
        '[data-testid="inventory-row"][data-model="e2e-shared"][data-section-channel="e2e-layout-a"]',
      ),
    ).toBeVisible();
    await expect(
      page.locator(
        '[data-testid="inventory-row"][data-model="e2e-shared"][data-section-channel="e2e-layout-b"]',
      ),
    ).toBeVisible();
    await expect(
      page.locator(
        '[data-testid="inventory-row"][data-model="e2e-only-a"][data-section-channel="e2e-layout-b"]',
      ),
    ).toHaveCount(0);
  });

  test('deleting a group with bound tokens asks to force; force rebinds tokens to default', async ({
    page,
  }) => {
    await seedChannel(page, { name: 'e2e-force-channel', models: ['e2e-force-mini'] });
    await seedModelGroup(page, { name: 'e2e-force-group', models: ['e2e-force-mini'] });
    const token = await seedToken(page, {
      name: 'e2e-force-token',
      model_group: 'e2e-force-group',
    });

    await page.goto('/models');
    await page.getByTestId('models-tab-groups').click();
    const defaultRow = page.locator('[data-testid="group-row"][data-group-name="default"]');
    await expect(defaultRow.getByTestId('group-delete')).toHaveCount(0);

    const groupRow = page.locator('[data-testid="group-row"][data-group-name="e2e-force-group"]');
    await clickRowAction(groupRow, page, 'group-delete');
    await page.getByRole('dialog').getByTestId('group-delete-confirm').click();
    await expect(page.getByTestId('group-force-delete-confirm')).toBeVisible();
    await page.getByTestId('group-force-delete-confirm').click();
    await expect(groupRow).toHaveCount(0);

    const listed = await page.request.get('/tokens', {
      headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
    });
    const tokens = (await listed.json()) as Array<{ token_key: string; model_group: string }>;
    expect(tokens.find((item) => item.token_key === token.token_key)?.model_group).toBe('default');
  });
});
