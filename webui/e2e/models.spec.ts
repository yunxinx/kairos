import { authedTest as test, expect } from './fixtures';
import type { Locator, Page } from '@playwright/test';
import { e2eRootHeaders } from './helpers/session';
import {
  seedCatalog,
  seedChannel,
  seedChannelModelOrder,
  seedModelGroup,
  seedPrice,
  seedToken,
  seedUnifiedModel,
  updateChannel,
  deleteChannel,
} from './helpers/models';
import { seedRequestLogs } from './helpers/seed-logs';
import { clickRowAction } from './helpers/table';

test.describe.configure({ mode: 'serial' });

async function expectSourceStatus(
  host: Locator,
  kind: 'unlisted' | 'disabled' | 'gone',
  channel: string | null,
  chipTestId = 'unified-source-channel',
): Promise<void> {
  const status = host.getByTestId('member-source-status');
  await expect(status).toHaveAttribute('data-kind', kind);
  await expect(status).toHaveClass(/badge-danger/);
  if (channel === null) {
    await expect(host.getByTestId(chipTestId)).toHaveCount(0);
    return;
  }
  await expect(host.getByTestId(chipTestId)).toHaveText(channel);
  expect(
    await host.evaluate((el, testId) => {
      const chip = el.querySelector(`[data-testid="${testId}"]`);
      const badge = el.querySelector('[data-testid="member-source-status"]');
      if (chip === null || badge === null) return false;
      return (chip.compareDocumentPosition(badge) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
    }, chipTestId),
  ).toBe(true);
}

async function assertSourceKind(
  page: Page,
  kind: 'unlisted' | 'disabled' | 'gone',
  channel: string | null,
): Promise<void> {
  await page.getByTestId('models-tab-unified').click();
  const unifiedRow = page.locator('[data-testid="unified-row"][data-unified-id="e2e-src-u"]');
  await expectSourceStatus(unifiedRow.getByTestId('unified-member-line'), kind, channel);

  await unifiedRow.getByTestId('unified-edit').click();
  await expectSourceStatus(
    page.locator('[data-testid="unified-member"][data-member="e2e-src-m"]'),
    kind,
    channel,
  );
  await page.getByRole('button', { name: /^cancel$/i }).click();

  await page.getByTestId('models-tab-visible').click();
  const visibleRow = page.locator('[data-testid="visible-model"][data-model="e2e-src-u"]');
  await expectSourceStatus(visibleRow.getByTestId('unified-member-line'), kind, channel);
}

test.describe('models page', () => {
  test('parallel tabs default to inventory; selection does not cross tabs', async ({ page }) => {
    await seedChannel(page, { name: 'e2e-tab-channel', models: ['e2e-tab-model'] });
    await page.goto('/models');
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

  test('order tab lists only multi-channel names, lets dragging reorder, and persists', async ({
    page,
  }) => {
    const first = await seedChannel(page, {
      name: 'e2e-order-a',
      models: ['e2e-order-shared', 'e2e-order-single'],
    });
    const second = await seedChannel(page, {
      name: 'e2e-order-b',
      models: ['e2e-order-shared'],
    });
    await page.goto('/models');
    await page.getByTestId('models-tab-order').click();

    await page.getByTestId('order-search').fill('e2e-order-shared');
    await expect(page.locator('[data-testid="order-row"]')).toHaveCount(1);
    await page.getByTestId('order-search').fill('e2e-order-single');
    await expect(page.locator('[data-testid="order-row"]')).toHaveCount(0);
    await page.getByTestId('order-search').fill('');

    const sharedRow = page.locator('[data-testid="order-row"][data-model="e2e-order-shared"]');
    await expect(sharedRow).toBeVisible();
    await expect(
      page.locator('[data-testid="order-row"][data-model="e2e-order-single"]'),
    ).toHaveCount(0);
    const channels = sharedRow.getByTestId('order-channel');
    await expect(channels).toHaveCount(2);
    await expect(channels.nth(0)).toHaveAttribute('data-channel', 'e2e-order-a');
    await expect(channels.nth(1)).toHaveAttribute('data-channel', 'e2e-order-b');
    await expect(sharedRow.getByTestId('order-save')).toBeDisabled();

    const handle = channels.nth(0).getByTestId('order-drag-handle');
    const target = channels.nth(1);
    const targetBox = await target.boundingBox();
    if (!targetBox) throw new Error('drag target has no box');
    await handle.dragTo(target, {
      targetPosition: { x: targetBox.width / 2, y: targetBox.height - 2 },
    });

    await expect(channels.nth(0)).toHaveAttribute('data-channel', 'e2e-order-b');
    await expect(channels.nth(1)).toHaveAttribute('data-channel', 'e2e-order-a');
    await expect(sharedRow.getByTestId('order-save')).toBeEnabled();
    await sharedRow.getByTestId('order-save').click();
    await expect(sharedRow.getByTestId('order-save')).toHaveText(/Save order/);
    await expect(sharedRow.getByTestId('order-save')).toBeDisabled();

    const listed = await page.request.get('/api/channel-model-orders', {
      headers: await e2eRootHeaders(page),
    });
    const orders = (await listed.json()) as Array<{ model: string; channel_ids: number[] }>;
    expect(orders.find((order) => order.model === 'e2e-order-shared')?.channel_ids).toEqual([
      second.id,
      first.id,
    ]);

    await page.reload();
    await page.getByTestId('models-tab-order').click();
    const persisted = page.locator('[data-testid="order-row"][data-model="e2e-order-shared"]');
    await expect(persisted.getByTestId('order-channel').nth(0)).toHaveAttribute(
      'data-channel',
      'e2e-order-b',
    );
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
    await expect(canonical).toBeVisible();
    await expect(canonical.getByTestId('inventory-unpriced')).toBeVisible();
    await expect(
      page.locator('[data-testid="inventory-section"][data-channel="e2e-inv-channel"]'),
    ).toBeVisible();
    await expect(page.getByTestId('inventory-channels-head')).toHaveCount(0);
    await expect(canonical.getByTestId('inventory-alias')).toContainText('e2e-inv-fast');
    await expect(
      canonical.locator('[data-testid="inventory-alias-chip"][data-canonical="true"]'),
    ).toHaveCount(0);
    await canonical.getByTestId('inventory-model-name').click();
    await expect
      .poll(() => page.evaluate(() => navigator.clipboard.readText()))
      .toBe('e2e-inv-mini');
    await expect(
      page.locator('[data-testid="inventory-row"][data-model="e2e-inv-fast"]'),
    ).toHaveCount(0);

    await seedChannel(page, {
      name: 'e2e-alias-only-channel',
      models: ['e2e-alias-gpt', 'e2e-alias-mini'],
      model_aliases: { 'e2e-alias-gpt': 'e2e-alias-upstream' },
    });
    await page.reload();
    const aliasOnly = page.locator('[data-testid="inventory-row"][data-model="e2e-alias-gpt"]');
    await expect(aliasOnly).toBeVisible();
    await expect(
      aliasOnly.locator('[data-testid="inventory-alias-chip"][data-canonical="true"]'),
    ).toHaveText('e2e-alias-upstream');
    await expect(
      page.locator('[data-testid="inventory-row"][data-model="e2e-alias-upstream"]'),
    ).toHaveCount(0);
    await expect(
      page.locator('[data-testid="inventory-row"][data-model="e2e-alias-mini"]'),
    ).toBeVisible();

    await seedChannel(page, {
      name: 'e2e-fold-channel',
      models: ['e2e-fold-canon'],
      model_aliases: { 'e2e-fold-nick': 'e2e-fold-canon' },
    });
    await page.reload();
    const folded = page.locator('[data-testid="inventory-row"][data-model="e2e-fold-canon"]');
    await expect(folded).toBeVisible();
    await expect(folded.getByTestId('inventory-alias')).toContainText('e2e-fold-nick');
    await expect(
      page.locator('[data-testid="inventory-row"][data-model="e2e-fold-nick"]'),
    ).toHaveCount(0);

    await seedChannel(page, {
      name: 'e2e-multi-alias-channel',
      models: ['e2e-multi-canon'],
      model_aliases: {
        'e2e-multi-a': 'e2e-multi-canon',
        'e2e-multi-b': 'e2e-multi-canon',
      },
    });
    await page.reload();
    const multi = page.locator('[data-testid="inventory-row"][data-model="e2e-multi-canon"]');
    await expect(multi).toBeVisible();
    await expect(multi.getByTestId('inventory-alias')).toContainText('e2e-multi-a');
    await expect(multi.getByTestId('inventory-alias')).toContainText('e2e-multi-b');
    await expect(
      page.locator('[data-testid="inventory-row"][data-model="e2e-multi-a"]'),
    ).toHaveCount(0);
    await expect(
      page.locator('[data-testid="inventory-row"][data-model="e2e-multi-b"]'),
    ).toHaveCount(0);

    await canonical.getByTestId('pricing-edit-entry').click();
    await expect(page.locator('[id^="pricing-editor-model"]')).toHaveValue('e2e-inv-mini');
    await expect(page.locator('[id^="pricing-editor-model"]')).toBeDisabled();
    await page.locator('[id^="pricing-editor-input"]').fill('1.000001');
    await page.locator('[id^="pricing-editor-output"]').fill('2.5');
    await page.locator('[id^="pricing-editor-cache-read"]').fill('0.000001');
    await page.getByRole('textbox', { name: 'Cache write', exact: true }).fill('');
    await page.getByTestId('pricing-save-entry').click();

    await expect(canonical.getByTestId('inventory-unpriced')).toHaveCount(0);
    await expect(canonical.getByTestId('price-input')).toHaveText('1.000001');
    await expect(canonical.getByTestId('price-output')).toHaveText('2.5');
    await expect(canonical.getByTestId('price-cache-read')).toHaveText('0.000001');
    await expect(canonical.getByTestId('price-cache-write')).toHaveText('—');

    const listed = await page.request.get('/api/prices', {
      headers: await e2eRootHeaders(page),
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

    await canonical.getByTestId('pricing-edit-entry').click();
    await expect(page.locator('[id^="pricing-editor-input"]')).toHaveValue('1.000001');
    await page.locator('[id^="pricing-editor-output"]').fill('3.25');
    await page.getByTestId('pricing-save-entry').click();
    await expect(canonical.getByTestId('price-output')).toHaveText('3.25');

    await page.getByTestId('inventory-status-filter').click();
    await page
      .locator('[data-testid="inventory-status-filter-option"][data-value="unpriced"]')
      .click();
    await expect(canonical).toHaveCount(0);
    await page.getByTestId('inventory-status-filter-clear').click();
    await expect(canonical).toBeVisible();

    await clickRowAction(canonical, page, 'inventory-delete');
    await page.getByRole('dialog').getByTestId('inventory-delete-confirm').click();
    await expect(canonical).toHaveCount(0);
  });

  test('deleting an alias-only inventory row does not strip the canonical from another channel', async ({
    page,
  }) => {
    await seedChannel(page, {
      name: 'e2e-del-alias-ch',
      models: ['e2e-del-alias'],
      model_aliases: { 'e2e-del-alias': 'e2e-del-canonical' },
    });
    const canonChannel = await seedChannel(page, {
      name: 'e2e-del-canon-ch',
      models: ['e2e-del-canonical'],
    });
    await seedPrice(page, {
      channel_id: canonChannel.id,
      model: 'e2e-del-canonical',
      input_micros: 1_000_000,
      output_micros: 2_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await page.goto('/models');
    const aliasRow = page.locator(
      '[data-testid="inventory-row"][data-model="e2e-del-alias"][data-section-channel="e2e-del-alias-ch"]',
    );
    await clickRowAction(aliasRow, page, 'inventory-delete');
    await page.getByRole('dialog').getByTestId('inventory-delete-confirm').click();
    await expect(aliasRow).toHaveCount(0);
    const canonRow = page.locator(
      '[data-testid="inventory-row"][data-model="e2e-del-canonical"][data-section-channel="e2e-del-canon-ch"]',
    );
    await expect(canonRow).toBeVisible();
    await expect(canonRow.getByTestId('inventory-unpriced')).toHaveCount(0);
  });

  test('catalog fill previews, requires a provider pick, and fills blank tiers only', async ({
    page,
  }) => {
    const catChannel = await seedChannel(page, {
      name: 'e2e-cat-channel',
      models: ['e2e-cat-mini'],
    });
    await seedPrice(page, {
      channel_id: catChannel.id,
      model: 'e2e-cat-mini',
      input_micros: 9_000_000,
      output_micros: 8_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedCatalog(page, [
      {
        provider_id: 'openai',
        provider_name: 'OpenAI',
        model_id: 'e2e-cat-mini',
        input_micros: 150_000,
        output_micros: 600_000,
        cache_read_micros: 75_000,
        cache_write_micros: null,
      },
      {
        provider_id: 'cortecs',
        provider_name: 'Cortecs',
        model_id: 'e2e-cat-mini',
        input_micros: 159_000,
        output_micros: 638_000,
        cache_read_micros: 81_000,
        cache_write_micros: null,
      },
    ]);

    await page.goto('/models');
    const row = page.locator('[data-testid="inventory-row"][data-model="e2e-cat-mini"]');
    await row.getByTestId('inventory-select').click();
    await page.getByTestId('inventory-bulk-catalog').click();
    await expect(page.getByTestId('catalog-preview')).toBeVisible();
    await expect(page.getByTestId('catalog-preview-status')).toHaveText('Pick');
    await expect(page.getByTestId('catalog-confirm')).toBeDisabled();

    await page.getByTestId('catalog-pick-from-dir').click();
    await expect(page.getByTestId('catalog-browser')).toBeVisible();
    await expect(page.getByTestId('catalog-browser-search')).toHaveValue('e2e-cat-mini');
    await page.locator('[data-testid="catalog-browser-row"][data-provider="openai"]').click();
    await expect(page.getByTestId('catalog-host-name')).toHaveText('OpenAI');
    await expect(page.getByTestId('catalog-preview-status')).toHaveText('Write');
    await page.getByTestId('catalog-confirm').click();

    await expect(row.getByTestId('price-input')).toHaveText('9');
    await expect(row.getByTestId('price-output')).toHaveText('8');
    await expect(row.getByTestId('price-cache-read')).toHaveText('0.075');
    await expect(row.getByTestId('price-cache-write')).toHaveText('—');
  });

  test('coding token, group, and unified id can coexist; downstream tab and logs show outbound', async ({
    page,
  }) => {
    const codingChannel = await seedChannel(page, {
      name: 'e2e-coding-channel',
      models: ['e2e-code-mini', 'e2e-code-haiku'],
    });
    await seedPrice(page, {
      channel_id: codingChannel.id,
      model: 'e2e-code-mini',
      input_micros: 150_000,
      output_micros: 600_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedPrice(page, {
      channel_id: codingChannel.id,
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
    await expect(
      page
        .locator('[data-testid="unified-pick"][data-model="e2e-code-mini"]')
        .getByTestId('unified-source-channel'),
    ).toHaveText('e2e-coding-channel');
    await expect(
      page
        .locator('[data-testid="unified-pick"][data-model="e2e-code-mini"]')
        .getByTestId('unified-source-channel'),
    ).toHaveClass(/badge-info/);
    await page
      .locator('[data-testid="unified-pick"][data-model="e2e-code-mini"]')
      .getByTestId('unified-pick-check')
      .click();
    await page
      .locator('[data-testid="unified-pick"][data-model="e2e-code-haiku"]')
      .getByTestId('unified-pick-check')
      .click();
    await page.getByTestId('unified-member-down').first().click();
    await page.getByTestId('unified-hide-switch').click();
    await page.getByTestId('unified-save').click();
    const unifiedRow = page.locator('[data-testid="unified-row"][data-unified-id="coding"]');
    await expect(unifiedRow).toBeVisible();
    await page.getByTestId('unified-status-filter').click();
    await expect(
      page.locator(
        '[data-testid="unified-status-filter-option"][data-value="hidden"] .sync-filter-count',
      ),
    ).toBeVisible();
    await page.getByTestId('unified-status-filter').click();
    await expect(unifiedRow.locator('[data-testid="unified-member-line"]').nth(0)).toHaveAttribute(
      'data-member',
      'e2e-code-haiku',
    );
    await expect(unifiedRow.locator('[data-testid="unified-member-line"]').nth(1)).toHaveAttribute(
      'data-member',
      'e2e-code-mini',
    );
    await unifiedRow.getByTestId('unified-model-name').click();
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('coding');

    await unifiedRow.getByTestId('unified-edit').click();
    await expect(page.getByTestId('unified-editor-id')).toHaveValue('coding');
    await expect(page.getByTestId('unified-editor-id')).toBeDisabled();
    await page.getByRole('button', { name: /^cancel$/i }).click();

    await page.getByTestId('models-tab-visible').click();
    await expect(page.getByTestId('visible-model-count')).toHaveText(/^\d+$/);
    const visibleRow = page.locator('[data-testid="visible-model"][data-model="coding"]');
    await expect(visibleRow).toBeVisible();
    await visibleRow.getByTestId('visible-model-name').click();
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('coding');

    await page.getByTestId('models-tab-groups').click();
    await page.getByTestId('group-create').click();
    await page.getByTestId('group-editor-name').fill('coding');
    await page.getByTestId('group-pick-channel-filter').click();
    const unifiedSourceFilter = page.locator(
      '[data-testid="group-pick-channel-filter-option"][data-value="__unified__"]',
    );
    await expect(unifiedSourceFilter).toBeVisible();
    await expect(unifiedSourceFilter.locator('.sync-filter-count')).toBeVisible();
    await page.getByTestId('group-pick-channel-filter').click();
    await page
      .locator('[data-testid="group-pick"][data-model="coding"]')
      .getByTestId('group-pick-check')
      .click();
    await expect(
      page.locator('[data-testid="group-model-option"][data-model="coding"]'),
    ).toBeVisible();
    await expect(
      page
        .locator('[data-testid="group-model-option"][data-model="coding"]')
        .getByTestId('group-unified-chip'),
    ).toHaveText('coding');
    await expect(
      page
        .locator('[data-testid="group-model-option"][data-model="coding"]')
        .getByTestId('group-source-channel'),
    ).toHaveText('Unified model');
    await page.getByTestId('group-save').click();
    const codingGroup = page.locator('[data-testid="group-row"][data-group-name="coding"]');
    await expect(codingGroup).toBeVisible();
    await expect(codingGroup.getByTestId('group-member-line')).toHaveAttribute(
      'data-model',
      'coding',
    );
    await expect(codingGroup.getByTestId('group-unified-chip')).toHaveText('coding');
    await expect(codingGroup.getByTestId('group-source-channel')).toHaveCount(0);
    await expect(codingGroup.getByTestId('unified-source-channel')).toHaveCount(0);
    await expect(codingGroup.getByTestId('unified-member-line')).toHaveCount(0);

    await page.getByTestId('models-tab-visible').click();
    await page.getByTestId('visible-group-filter').click();
    await expect(
      page.locator(
        '[data-testid="visible-group-filter-option"][data-value="coding"] .sync-filter-count',
      ),
    ).toBeVisible();
    await page.locator('[data-testid="visible-group-filter-option"][data-value="coding"]').click();
    await expect(page.getByTestId('visible-model')).toHaveCount(1);
    await expect(page.locator('[data-testid="visible-model"][data-model="coding"]')).toBeVisible();
    await expect(page.getByTestId('visible-unified-order')).toContainText('e2e-code-haiku');

    await page.goto('/tokens');
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
    await page.goto('/logs');
    await page.locator('#logs-search').fill('sk-e2e-coding');
    const logRow = page.locator('[data-testid="log-row"][data-model="coding"]');
    await expect(logRow.getByTestId('log-model')).toHaveText('coding');
    await logRow.getByTestId('log-expand').click();
    await expect(page.getByTestId('log-outbound-model')).toHaveText('e2e-code-haiku');
    await expect(page.getByTestId('log-detail-channel')).toHaveText('e2e-coding-channel');
  });

  test('inventory groups a shared model under every member channel', async ({ page }) => {
    const layoutA = await seedChannel(page, {
      name: 'e2e-layout-a',
      models: ['e2e-shared', 'e2e-only-a'],
    });
    const layoutB = await seedChannel(page, {
      name: 'e2e-layout-b',
      models: ['e2e-shared', 'e2e-only-b'],
    });
    await seedChannelModelOrder(page, 'e2e-shared', [layoutB.id, layoutA.id]);
    await page.goto('/models');
    await expect(page.getByTestId('inventory-channels-head')).toHaveCount(0);
    await expect(page.getByTestId('inventory-channel-chip')).toHaveCount(0);
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

    await page.getByTestId('inventory-channel-filter').click();
    await page
      .locator('[data-testid="inventory-channel-filter-option"][data-value="e2e-layout-a"]')
      .click();
    await expect(
      page.locator('[data-testid="inventory-section"][data-channel="e2e-layout-a"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="inventory-section"][data-channel="e2e-layout-b"]'),
    ).toHaveCount(0);
    await expect(
      page.locator(
        '[data-testid="inventory-row"][data-model="e2e-shared"][data-section-channel="e2e-layout-a"]',
      ),
    ).toBeVisible();
    await expect(
      page.locator(
        '[data-testid="inventory-row"][data-model="e2e-shared"][data-section-channel="e2e-layout-b"]',
      ),
    ).toHaveCount(0);

    await page.getByTestId('models-tab-visible').click();
    const sharedVisible = page.locator('[data-testid="visible-model"][data-model="e2e-shared"]');
    await expect(sharedVisible.getByTestId('visible-callable-order')).toBeVisible();
    const routeLines = sharedVisible.locator('[data-testid="unified-member-line"]');
    await expect(routeLines).toHaveCount(2);
    await expect(routeLines.nth(0)).toHaveAttribute('data-channel', 'e2e-layout-b');
    await expect(routeLines.nth(1)).toHaveAttribute('data-channel', 'e2e-layout-a');
    await expect(sharedVisible.getByTestId('unified-member-index')).toHaveCount(2);

    const onlyA = page.locator('[data-testid="visible-model"][data-model="e2e-only-a"]');
    await expect(onlyA.getByTestId('visible-callable-order')).toBeVisible();
    await expect(onlyA.getByTestId('unified-member-index')).toHaveCount(0);
  });

  test('same model on two channels keeps independent prices', async ({ page }) => {
    const left = await seedChannel(page, { name: 'e2e-price-a', models: ['e2e-dup'] });
    const right = await seedChannel(page, { name: 'e2e-price-b', models: ['e2e-dup'] });
    await seedPrice(page, {
      channel_id: left.id,
      model: 'e2e-dup',
      input_micros: 1_000_000,
      output_micros: 2_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedPrice(page, {
      channel_id: right.id,
      model: 'e2e-dup',
      input_micros: 4_000_000,
      output_micros: 5_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await page.goto('/models');
    const rowA = page.locator(
      '[data-testid="inventory-row"][data-model="e2e-dup"][data-section-channel="e2e-price-a"]',
    );
    const rowB = page.locator(
      '[data-testid="inventory-row"][data-model="e2e-dup"][data-section-channel="e2e-price-b"]',
    );
    await expect(rowA.getByTestId('price-input')).toHaveText('1');
    await expect(rowB.getByTestId('price-input')).toHaveText('4');

    await rowA.getByTestId('pricing-edit-entry').click();
    await expect(page.getByTestId('pricing-editor-channel')).toHaveValue('e2e-price-a');
    await expect(page.getByTestId('pricing-editor-channel')).toBeDisabled();
    await page.locator('[id^="pricing-editor-input"]').fill('3');
    await page.locator('[id^="pricing-editor-output"]').fill('6');
    await page.getByTestId('pricing-save-entry').click();

    await expect(rowA.getByTestId('price-input')).toHaveText('3');
    await expect(rowA.getByTestId('price-output')).toHaveText('6');
    await expect(rowB.getByTestId('price-input')).toHaveText('4');
    await expect(rowB.getByTestId('price-output')).toHaveText('5');
  });

  test('deleting a row leaves the same model and independent prices on the other channel', async ({
    page,
  }) => {
    const left = await seedChannel(page, { name: 'e2e-del-price-a', models: ['e2e-keep'] });
    const right = await seedChannel(page, { name: 'e2e-del-price-b', models: ['e2e-keep'] });
    await seedPrice(page, {
      channel_id: left.id,
      model: 'e2e-keep',
      input_micros: 1_000_000,
      output_micros: 2_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedPrice(page, {
      channel_id: right.id,
      model: 'e2e-keep',
      input_micros: 4_000_000,
      output_micros: 5_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await page.goto('/models');
    const rowA = page.locator(
      '[data-testid="inventory-row"][data-model="e2e-keep"][data-section-channel="e2e-del-price-a"]',
    );
    const rowB = page.locator(
      '[data-testid="inventory-row"][data-model="e2e-keep"][data-section-channel="e2e-del-price-b"]',
    );
    await clickRowAction(rowA, page, 'inventory-delete');
    await page.getByRole('dialog').getByTestId('inventory-delete-confirm').click();
    await expect(rowA).toHaveCount(0);
    await expect(rowB).toBeVisible();
    await expect(rowB.getByTestId('price-input')).toHaveText('4');
    await expect(rowB.getByTestId('price-output')).toHaveText('5');

    const listed = await page.request.get('/api/prices', {
      headers: await e2eRootHeaders(page),
    });
    const prices = (await listed.json()) as Array<{
      channel_id: number;
      model: string;
      input_micros: number;
    }>;
    const leftPrice = prices.find(
      (item) => item.channel_id === left.id && item.model === 'e2e-keep',
    );
    const rightPrice = prices.find(
      (item) => item.channel_id === right.id && item.model === 'e2e-keep',
    );
    // 移除模型登记不级联删价：A 渠道的价格行保留，可单独取消定价。
    expect(leftPrice?.input_micros).toBe(1_000_000);
    expect(rightPrice?.input_micros).toBe(4_000_000);
  });

  test('source status sits after the channel chip and distinguishes unlisted, disabled, and gone', async ({
    page,
  }) => {
    const channel = await seedChannel(page, {
      name: 'e2e-src-ch',
      models: ['e2e-src-m'],
    });
    await seedUnifiedModel(page, {
      id: 'e2e-src-u',
      hide: false,
      models: [{ channel_id: channel.id, model: 'e2e-src-m' }],
    });
    await seedModelGroup(page, {
      name: 'e2e-src-g',
      models: [
        { kind: 'unified', id: 'e2e-src-u' },
        { kind: 'source', channel_id: channel.id, model: 'e2e-src-m' },
      ],
    });

    await updateChannel(page, channel.id, { enabled: false });
    await page.goto('/models');
    await assertSourceKind(page, 'disabled', 'e2e-src-ch');

    await page.getByTestId('models-tab-groups').click();
    const groupRow = page.locator('[data-testid="group-row"][data-group-name="e2e-src-g"]');
    await expect(
      groupRow.locator('[data-testid="group-member-line"][data-model="e2e-src-m"]'),
    ).toBeVisible();
    await expectSourceStatus(
      groupRow.locator('[data-testid="group-member-line"][data-model="e2e-src-m"]'),
      'disabled',
      'e2e-src-ch',
      'group-source-channel',
    );
    await expect(
      groupRow.locator('[data-testid="group-member-line"][data-model="e2e-src-u"]'),
    ).toBeVisible();
    await expect(
      groupRow.locator('[data-testid="group-member-line"][data-model="e2e-src-u"]'),
    ).toHaveAttribute('data-unified', 'true');
    await expect(
      groupRow
        .locator('[data-testid="group-member-line"][data-model="e2e-src-u"]')
        .getByTestId('unified-member-line'),
    ).toHaveCount(0);

    await updateChannel(page, channel.id, { enabled: true, models: [] });
    await page.reload();
    await assertSourceKind(page, 'unlisted', 'e2e-src-ch');

    await page.getByTestId('models-tab-groups').click();
    await expect(
      groupRow.locator('[data-testid="group-member-line"][data-model="e2e-src-u"]'),
    ).toBeVisible();
    await groupRow.getByTestId('group-edit').click();
    const unifiedOption = page.locator(
      '[data-testid="group-model-option"][data-model="e2e-src-u"]',
    );
    const ordinaryOption = page.locator(
      '[data-testid="group-model-option"][data-model="e2e-src-m"]',
    );
    await expect(unifiedOption.getByTestId('unified-member-line')).toHaveCount(0);
    await expect(unifiedOption.getByTestId('group-source-channel')).toHaveText('Unified model');
    await expectSourceStatus(ordinaryOption, 'unlisted', 'e2e-src-ch', 'group-source-channel');

    // 删除渠道同步清理统一模型与模型组引用：唯一成员所在渠道没了，
    // 空成员的统一模型整体移除；组保留，两个成员引用都被清掉。
    await deleteChannel(page, channel.id);
    await page.reload();
    await expect(
      page.locator('[data-testid="unified-row"][data-unified-id="e2e-src-u"]'),
    ).toHaveCount(0);
    await page.getByTestId('models-tab-groups').click();
    await expect(groupRow).toBeVisible();
    await expect(groupRow.locator('[data-testid="group-member-line"]')).toHaveCount(0);
  });

  test('same name on two channels is two independent group members', async ({ page }) => {
    await seedChannel(page, { name: 'e2e-ind-a', models: ['e2e-shared'] });
    await seedChannel(page, { name: 'e2e-ind-b', models: ['e2e-shared'] });
    await page.goto('/models');
    await page.getByTestId('models-tab-groups').click();
    await page.getByTestId('group-create').click();
    await page.getByTestId('group-editor-name').fill('e2e-ind');
    await page.getByTestId('group-editor-search').fill('e2e-shared');
    await page
      .locator('[data-testid="group-pick"][data-model="e2e-shared"][data-channel="e2e-ind-a"]')
      .getByTestId('group-pick-check')
      .click();
    await expect(
      page.locator(
        '[data-testid="group-model-option"][data-model="e2e-shared"][data-channel="e2e-ind-a"]',
      ),
    ).toBeVisible();
    await expect(
      page.locator(
        '[data-testid="group-model-option"][data-model="e2e-shared"][data-channel="e2e-ind-b"]',
      ),
    ).toHaveCount(0);
    await expect(
      page.locator('[data-testid="group-pick"][data-model="e2e-shared"][data-channel="e2e-ind-b"]'),
    ).toBeVisible();
    await page.getByTestId('group-save').click();
    const groupRow = page.locator('[data-testid="group-row"][data-group-name="e2e-ind"]');
    await expect(
      groupRow.locator(
        '[data-testid="group-member-line"][data-model="e2e-shared"][data-channel="e2e-ind-a"]',
      ),
    ).toBeVisible();
    await expect(
      groupRow.locator(
        '[data-testid="group-member-line"][data-model="e2e-shared"][data-channel="e2e-ind-b"]',
      ),
    ).toHaveCount(0);
  });

  test('deleting a group with bound tokens nulls the token group without rebind', async ({
    page,
  }) => {
    const forceChannel = await seedChannel(page, {
      name: 'e2e-force-channel',
      models: ['e2e-force-mini'],
    });
    await seedModelGroup(page, {
      name: 'e2e-force-group',
      models: [{ kind: 'source', channel_id: forceChannel.id, model: 'e2e-force-mini' }],
    });
    const token = await seedToken(page, {
      name: 'e2e-force-token',
      model_group: 'e2e-force-group',
    });

    await page.goto('/models');
    await page.getByTestId('models-tab-groups').click();
    await expect(page.locator('[data-testid="group-row"][data-group-name="default"]')).toHaveCount(
      0,
    );

    const groupRow = page.locator('[data-testid="group-row"][data-group-name="e2e-force-group"]');
    await clickRowAction(groupRow, page, 'group-delete');
    await page.getByRole('dialog').getByTestId('group-delete-confirm').click();
    await expect(groupRow).toHaveCount(0);

    const listed = await page.request.get('/api/tokens', {
      headers: await e2eRootHeaders(page),
    });
    const tokens = (await listed.json()) as Array<{ token_key: string; model_group: string }>;
    expect(tokens.find((item) => item.token_key === token.token_key)?.model_group).toBe('');
  });
});
