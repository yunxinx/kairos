import { authedTest as test, expect } from './fixtures';
import { E2E_ADMIN_KEY, E2E_PROTOCOL_PORT } from './helpers/gateway';
import { clickRowAction } from './helpers/table';
import { startProbeUpstream } from './helpers/upstream';

/** 读已保存渠道的模型清单；渠道不存在时返回 undefined。 */
async function savedChannelModels(
  page: import('@playwright/test').Page,
  name: string,
): Promise<string[] | undefined> {
  const resp = await page.request.get('/channels', {
    headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
  });
  const channels = (await resp.json()) as Array<{ name: string; models: string[] }>;
  return channels.find((item) => item.name === name)?.models;
}

/** 用指定令牌请求协议面；未定价/无渠道时网关返回 503。 */
async function chatCompletionsStatus(
  page: import('@playwright/test').Page,
  tokenKey: string,
  model: string,
): Promise<{ status: number; message: string }> {
  const resp = await page.request.post(
    `http://127.0.0.1:${E2E_PROTOCOL_PORT}/v1/chat/completions`,
    {
      headers: { Authorization: `Bearer ${tokenKey}` },
      data: { model, messages: [{ role: 'user', content: 'hi' }] },
    },
  );
  const body = (await resp.json()) as { error?: { message?: string } };
  return { status: resp.status(), message: body.error?.message ?? '' };
}

test.describe.configure({ mode: 'serial' });

test.describe('channel resource page', () => {
  test('creates, edits, probes, and deletes a channel', async ({ page }) => {
    const okUpstream = await startProbeUpstream(200, { models: ['gpt-4o-mini', 'gpt-4o'] });
    const failUpstream = await startProbeUpstream(500, { models: ['gpt-4o-mini'] });
    const errUpstream = await startProbeUpstream(404);
    try {
      await page.goto('/channel');
      await expect(page.getByRole('heading', { name: /channels/i })).toBeVisible();

      await page.getByTestId('create-channel').click();
      // Base URL/API key 缺失时同步按钮禁用。
      await expect(page.getByTestId('channel-sync-models')).toBeDisabled();
      await page.locator('[id^="channel-editor-name"]').fill('ok-channel');
      await page.locator('[id^="channel-editor-base-url"]').fill(okUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');

      // 同步视图进入后不自动请求：先为空，点「同步」才拉取上游模型。
      await page.getByTestId('channel-sync-models').click();
      await expect(page.getByTestId('channel-sync-view')).toBeVisible();
      await expect(page.getByTestId('channel-sync-row')).toHaveCount(0);
      await expect(
        page.getByTestId('channel-sync-view').getByRole('columnheader', { name: 'Model' }),
      ).toBeVisible();
      await expect(page.getByTestId('channel-sync-view').getByText('Not synced yet')).toBeVisible();
      await page.getByTestId('channel-sync-run').click();
      const miniRow = page.locator('[data-testid="channel-sync-row"][data-model="gpt-4o-mini"]');
      const fullRow = page.locator('[data-testid="channel-sync-row"][data-model="gpt-4o"]');
      await expect(miniRow.getByTestId('channel-sync-status-unselected')).toBeVisible();

      // 别名列常显输入：为 gpt-4o-mini 填别名，勾选后主名与别名一并入清单。
      await miniRow.getByTestId('channel-sync-alias-input').fill('mini');
      await miniRow.getByTestId('channel-sync-checkbox').click();
      await expect(miniRow.getByTestId('channel-sync-status-willAdd')).toBeVisible();
      await page.getByTestId('channel-sync-back').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);
      await expect(page.getByTestId('channel-model-count')).toHaveText('2');
      // 带别名的主名与别名 chip 均染别名底色。
      const canonicalChip = page.locator(
        '[data-testid="channel-model-chip"][data-model="gpt-4o-mini"]',
      );
      const aliasChip = page.locator('[data-testid="channel-model-chip"][data-model="mini"]');
      await expect(canonicalChip).toHaveClass(/model-chip-alias/);
      await expect(aliasChip).toHaveClass(/model-chip-alias/);
      // tooltip：悬浮主名显示别名，悬浮别名显示主名。
      await canonicalChip.hover();
      await expect(page.locator('.tooltip-content')).toContainText('mini');
      await aliasChip.hover();
      await expect(page.locator('.tooltip-content')).toContainText('gpt-4o-mini');

      await page.getByTestId('channel-editor-tab-advanced').click();
      // 「模型别名」文本框已移除。
      await expect(page.locator('[id^="channel-editor-aliases"]')).toHaveCount(0);
      await page.locator('[id^="channel-editor-timeout-ms"]').fill('5000');
      await page.locator('[id^="channel-editor-max-retries"]').fill('1');
      await page.getByTestId('channel-save').click();

      const okRow = page.locator('[data-testid="channel-row"][data-channel-name="ok-channel"]');
      await expect(okRow).toBeVisible();
      await expect(
        okRow.locator('[data-testid="channel-models-chip"][data-model="gpt-4o-mini"]'),
      ).toBeVisible();
      await expect(
        okRow.locator(
          '[data-testid="channel-models-chip"][data-model="mini"][data-canonical="true"]',
        ),
      ).toBeVisible();
      // 品牌图标以 mask 渲染：mask-image 为空会退化成纯色块。
      await expect(okRow.locator('.brand-icon')).toHaveCSS('mask-image', /url\(|image/);

      // 优先级/权重行内步进：新建缺省 0/1，步进后持久化；权重触底时减号禁用。
      const priorityStepper = okRow.getByTestId('channel-priority-stepper');
      await expect(priorityStepper.locator('input')).toHaveValue('0');
      await priorityStepper.hover();
      await priorityStepper.getByRole('button', { name: 'Increase' }).click();
      await expect(priorityStepper.locator('input')).toHaveValue('1');
      const weightStepper = okRow.getByTestId('channel-weight-stepper');
      await expect(weightStepper.locator('input')).toHaveValue('1');
      await weightStepper.hover();
      await expect(weightStepper.getByRole('button', { name: 'Decrease' })).toBeDisabled();

      await page.getByTestId('channels-search').fill('ok-channel');
      await expect(okRow).toBeVisible();
      await page.getByTestId('channels-search').fill('gpt-4o-mini');
      await expect(okRow).toBeVisible();
      await page.getByTestId('channels-search').fill('no-such-channel');
      await expect(okRow).toHaveCount(0);
      await page.getByTestId('channels-search').fill('');
      await expect(okRow).toBeVisible();

      // 二次同步：别名保留、主名已选择；别名维度筛选可用；搜索/反选作用于可见行。
      await okRow.getByTestId('channel-edit').click();
      await page.getByTestId('channel-sync-models').click();
      await page.getByTestId('channel-sync-run').click();
      await expect(miniRow.getByTestId('channel-sync-status-selected')).toBeVisible();
      await expect(miniRow.getByTestId('channel-sync-alias-input')).toHaveValue('mini');
      await expect(fullRow.getByTestId('channel-sync-status-unselected')).toBeVisible();
      // 别名筛选：存在别名仅 gpt-4o-mini；清除后恢复。
      await page.getByTestId('channel-sync-filter').click();
      await expect(page.getByTestId('channel-sync-filter-menu')).toBeVisible();
      await expect(page.getByTestId('channel-sync-filter-hasAlias')).toContainText('1');
      await expect(page.getByTestId('channel-sync-filter-noAlias')).toContainText('1');
      await page.getByTestId('channel-sync-filter-hasAlias').click();
      await expect(miniRow).toBeVisible();
      await expect(fullRow).toHaveCount(0);
      await page.getByTestId('channel-sync-filter-clear').click();
      await expect(fullRow).toBeVisible();
      await page.getByTestId('channel-sync-filter').click();
      await expect(page.getByTestId('channel-sync-filter-menu')).toHaveCount(0);
      // 搜索框聚焦时铺到行尾，动作按钮让位；Esc 只失焦不关窗；失焦后叉号清空且不展开。
      const invertBtn = page.getByTestId('channel-sync-invert');
      const syncSearch = page.getByTestId('channel-sync-search');
      const collapsedSearch = await syncSearch.boundingBox();
      await expect(invertBtn).toBeVisible();
      await syncSearch.click();
      await expect(invertBtn).toBeHidden();
      const expandedSearch = await syncSearch.boundingBox();
      expect(collapsedSearch).toBeTruthy();
      expect(expandedSearch).toBeTruthy();
      expect(expandedSearch!.width).toBeGreaterThan(collapsedSearch!.width);
      await page.keyboard.press('Escape');
      await expect(invertBtn).toBeVisible();
      await expect(page.getByTestId('channel-sync-view')).toBeVisible();
      // 搜索过滤行；失焦后点叉号清空，不进入输入态。
      await syncSearch.fill('no-such-model');
      await expect(page.getByTestId('channel-sync-row')).toHaveCount(0);
      await expect(
        page.getByTestId('channel-sync-view').getByRole('columnheader', { name: 'Model' }),
      ).toBeVisible();
      await expect(
        page.getByTestId('channel-sync-view').getByText('No matching models'),
      ).toBeVisible();
      await syncSearch.fill('mini');
      await expect(fullRow).toHaveCount(0);
      await expect(miniRow).toBeVisible();
      await syncSearch.blur();
      await expect(invertBtn).toBeVisible();
      await page.getByTestId('channel-sync-view').getByTestId('search-input-clear').click();
      await expect(syncSearch).toHaveValue('');
      await expect(invertBtn).toBeVisible();
      await expect(fullRow).toBeVisible();
      // 反选再反选：勾选状态往返，别名随主名勾选保留。
      await invertBtn.click();
      await expect(miniRow.getByTestId('channel-sync-status-willRemove')).toBeVisible();
      await expect(fullRow.getByTestId('channel-sync-status-willAdd')).toBeVisible();
      await page.getByTestId('channel-sync-invert').click();
      await expect(miniRow.getByTestId('channel-sync-status-selected')).toBeVisible();
      await expect(fullRow.getByTestId('channel-sync-status-unselected')).toBeVisible();
      await page.getByTestId('channel-sync-back').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);

      await page.getByTestId('channel-save').click();

      await clickRowAction(okRow, page, 'channel-test');
      const probeView = page.getByTestId('channel-probe-view');
      await expect(probeView).toBeVisible();
      // 主名+别名去重：只显示主模型名一行。
      await expect(page.getByTestId('channel-probe-row')).toHaveCount(1);
      const probeMini = page.locator('[data-testid="channel-probe-row"][data-model="gpt-4o-mini"]');
      await expect(probeMini.getByTestId('channel-probe-status-idle')).toBeVisible();
      await page.getByTestId('channel-probe-search').fill('no-such-model');
      await expect(page.getByTestId('channel-probe-row')).toHaveCount(0);
      await expect(
        page.getByTestId('channel-probe-view').getByRole('columnheader', { name: 'Model' }),
      ).toBeVisible();
      await expect(
        page.getByTestId('channel-probe-view').getByText('No matching models'),
      ).toBeVisible();
      await page.getByTestId('channel-probe-search').fill('');
      await expect(probeMini).toBeVisible();
      await expect(page.getByTestId('channel-probe-test-selected')).toBeDisabled();
      await probeMini.getByTestId('channel-probe-run').click();
      await expect(probeMini.getByTestId('channel-probe-status-success')).toBeVisible();
      await expect(probeMini.getByTestId('channel-probe-latency')).toHaveText(/ms|s/);
      const probeDetail = page.getByTestId('channel-probe-detail');
      await expect(probeDetail).toBeVisible();
      await expect(probeDetail).toContainText('200');
      await expect(probeDetail).toContainText('/chat/completions');
      await probeDetail.hover();
      await page.waitForTimeout(3_500);
      await expect(probeDetail).toBeVisible();
      await page.mouse.move(0, 0);
      await expect(probeDetail).toBeHidden({ timeout: 5_000 });
      await page
        .getByRole('dialog', { name: /test channel/i })
        .getByRole('button', { name: 'Close' })
        .click();
      await expect(probeView).toHaveCount(0);

      // 新建渠道缺省启用：禁用 → 状态徽章变更；再启用 → 恢复。
      await expect(okRow.getByTestId('channel-toggle-enabled')).toHaveText('Enabled');
      await okRow.getByTestId('channel-toggle-enabled').click();
      await expect(okRow.getByTestId('channel-toggle-enabled')).toHaveText('Disabled');
      await okRow.getByTestId('channel-toggle-enabled').click();
      await expect(okRow.getByTestId('channel-toggle-enabled')).toHaveText('Enabled');

      // 同步错误以独立浮窗展示：进入不自动请求，点「同步」触发；3s 自动消失、悬浮暂停。
      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-base-url"]').fill(errUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');
      await page.getByTestId('channel-sync-models').click();
      await page.getByTestId('channel-sync-run').click();
      const syncError = page.getByTestId('channel-sync-error');
      await expect(syncError).toBeVisible();
      // 悬浮期间超过 3s 仍不消失；移开后按剩余时长消失。
      await syncError.hover();
      await page.waitForTimeout(3_500);
      await expect(syncError).toBeVisible();
      await page.mouse.move(0, 0);
      await expect(syncError).toBeHidden({ timeout: 5_000 });
      await page.getByTestId('channel-sync-back').click();
      await page.getByRole('button', { name: 'Cancel' }).click();

      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-name"]').fill('fail-channel');
      await page.locator('[id^="channel-editor-base-url"]').fill(failUpstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');
      await page.getByTestId('channel-sync-models').click();
      await page.getByTestId('channel-sync-run').click();
      await page
        .locator('[data-testid="channel-sync-row"][data-model="gpt-4o-mini"]')
        .getByTestId('channel-sync-checkbox')
        .click();
      await page.getByTestId('channel-sync-back').click();
      await page.getByTestId('channel-save').click();

      const failRow = page.locator('[data-testid="channel-row"][data-channel-name="fail-channel"]');
      await expect(failRow).toBeVisible();
      await clickRowAction(failRow, page, 'channel-test');
      const failProbeRow = page.locator(
        '[data-testid="channel-probe-row"][data-model="gpt-4o-mini"]',
      );
      await failProbeRow.getByTestId('channel-probe-run').click();
      await expect(failProbeRow.getByTestId('channel-probe-status-failure')).toBeVisible();
      const failDetail = page.getByTestId('channel-probe-detail');
      await expect(failDetail).toBeVisible();
      await expect(failDetail).toContainText('500');
      await page
        .getByRole('dialog', { name: /test channel/i })
        .getByRole('button', { name: 'Close' })
        .click();

      // 三开窗校验：chip 复制、删主名保留别名（同步视图呈「仅别名生效」虚线态）。
      await okRow.getByTestId('channel-edit').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);
      // 点击 chip 复制别名到剪贴板。
      await aliasChip.click();
      await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('mini');
      await expect(page.locator('[id^="channel-editor-priority"]')).toHaveCount(0);
      await expect(page.locator('[id^="channel-editor-weight"]')).toHaveCount(0);
      // 删除主模型名 chip：别名保留在清单。
      await canonicalChip.getByTestId('channel-model-remove').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(1);
      await expect(page.getByTestId('channel-model-count')).toHaveText('1');
      // 同步视图中该主名「仅别名生效」：勾选保留、虚线边框、别名列仍在。
      await page.getByTestId('channel-sync-models').click();
      await page.getByTestId('channel-sync-run').click();
      await expect(miniRow.getByTestId('channel-sync-status-selected')).toBeVisible();
      await expect(miniRow.locator('.model-name-deleted')).toBeVisible();
      await expect(miniRow).toHaveClass(/sync-row-alias-only/);
      await expect(miniRow.getByTestId('channel-sync-alias-input')).toHaveValue('mini');
      await page.getByTestId('channel-sync-back').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(1);
      await page.getByTestId('channel-editor-tab-advanced').click();
      await expect(page.locator('[id^="channel-editor-timeout-ms"]')).toHaveValue('5000');
      await expect(page.locator('[id^="channel-editor-max-retries"]')).toHaveValue('1');

      // 改名：保存后旧行消失，新名行接替其位。
      await page.getByTestId('channel-editor-tab-basic').click();
      await page.locator('[id^="channel-editor-name"]').fill('ok-channel-v2');
      await page.getByTestId('channel-save').click();
      await expect(okRow).toHaveCount(0);
      const renamedRow = page.locator(
        '[data-testid="channel-row"][data-channel-name="ok-channel-v2"]',
      );
      await expect(renamedRow).toBeVisible();

      await clickRowAction(renamedRow, page, 'channel-delete');
      await page.getByRole('dialog').getByTestId('channel-delete-confirm').click();
      await expect(renamedRow).toHaveCount(0);
    } finally {
      await okUpstream.close();
      await failUpstream.close();
      await errUpstream.close();
    }
  });
});

test.describe('channel manual model add', () => {
  test('adds a trimmed id to the draft, rejects empty/duplicate/alias, and keeps it across sync', async ({
    page,
  }) => {
    const upstream = await startProbeUpstream(200, { models: ['gpt-4o-mini'] });
    const channelName = 'manual-add-channel';
    const manualId = 'manual-only-id';
    try {
      const tokenResp = await page.request.post('/tokens', {
        headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
        data: { name: 'manual-add-token', limit_usd_micros: null, enabled: true },
      });
      expect(tokenResp.ok()).toBeTruthy();
      const token = (await tokenResp.json()) as { token_key: string };

      await page.goto('/channel');
      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-name"]').fill(channelName);
      await page.locator('[id^="channel-editor-base-url"]').fill(upstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');

      await expect(page.getByTestId('channel-add-model')).toBeVisible();
      await expect(page.getByTestId('channel-add-model')).toHaveText('Add');
      await expect(page.getByTestId('channel-models-empty')).toBeVisible();
      await expect(page.getByTestId('channel-add-model-clear')).toHaveCount(0);
      await page.getByTestId('channel-add-model-input').fill(manualId);
      await page.getByTestId('channel-add-model-clear').click();
      await expect(page.getByTestId('channel-add-model-input')).toHaveValue('');
      await expect(page.getByTestId('channel-add-model-clear')).toHaveCount(0);
      await page.getByTestId('channel-add-model-input').fill(manualId);
      await page.getByTestId('channel-add-model').click();
      await expect(
        page.locator(`[data-testid="channel-model-chip"][data-model="${manualId}"]`),
      ).toBeVisible();
      await page
        .locator(`[data-testid="channel-model-chip"][data-model="${manualId}"]`)
        .getByTestId('channel-model-remove')
        .click();
      await expect(page.getByTestId('channel-models-empty')).toBeVisible();

      await page.getByTestId('channel-sync-models').click();
      await page.getByTestId('channel-sync-run').click();
      const miniRow = page.locator('[data-testid="channel-sync-row"][data-model="gpt-4o-mini"]');
      await miniRow.getByTestId('channel-sync-alias-input').fill('mini');
      await miniRow.getByTestId('channel-sync-checkbox').click();
      await page.getByTestId('channel-sync-back').click();
      await page.getByTestId('channel-save').click();

      const channelRow = page.locator(
        `[data-testid="channel-row"][data-channel-name="${channelName}"]`,
      );
      await expect(channelRow).toBeVisible();
      expect(await savedChannelModels(page, channelName)).toEqual(
        expect.arrayContaining(['gpt-4o-mini']),
      );
      expect(await savedChannelModels(page, channelName)).not.toContain('mini');
      expect(await savedChannelModels(page, channelName)).not.toContain(manualId);

      const beforeSave = await chatCompletionsStatus(page, token.token_key, manualId);
      expect(beforeSave.status).toBe(503);
      expect(beforeSave.message).toContain('渠道');

      await page.goto('/models');
      await expect(
        page.locator(`[data-testid="inventory-row"][data-model="${manualId}"]`),
      ).toHaveCount(0);

      await page.goto('/channel');
      await channelRow.getByTestId('channel-edit').click();

      await page.getByTestId('channel-add-model').click();
      await expect(page.locator('[data-form-field="addModel"]')).toContainText('Enter a model ID');

      await page.getByTestId('channel-add-model-input').fill('   ');
      await page.getByTestId('channel-add-model').click();
      await expect(page.locator('[data-form-field="addModel"]')).toContainText('Enter a model ID');
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);

      await page.getByTestId('channel-add-model-input').fill('gpt-4o-mini');
      await page.getByTestId('channel-add-model').click();
      await expect(page.locator('[data-form-field="addModel"]')).toContainText(
        'Already in the list',
      );
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);

      await page.getByTestId('channel-add-model-input').fill('mini');
      await page.getByTestId('channel-add-model').click();
      await expect(page.locator('[data-form-field="addModel"]')).toContainText(
        'This ID is already an alias',
      );
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(2);

      await page.getByTestId('channel-add-model-input').fill(`  ${manualId}  `);
      await page.getByTestId('channel-add-model').click();
      await expect(
        page.locator(`[data-testid="channel-model-chip"][data-model="${manualId}"]`),
      ).toBeVisible();
      await expect(page.getByTestId('channel-model-count')).toHaveText('3');
      await expect(page.getByTestId('channel-add-model-input')).toHaveValue('');

      expect(await savedChannelModels(page, channelName)).not.toContain(manualId);
      const stillDraft = await chatCompletionsStatus(page, token.token_key, manualId);
      expect(stillDraft.status).toBe(503);
      expect(stillDraft.message).toContain('渠道');

      await page.getByTestId('channel-save').click();
      await expect(page.getByTestId('channel-form')).toHaveCount(0);
      await expect
        .poll(() => savedChannelModels(page, channelName))
        .toEqual(expect.arrayContaining([manualId]));
      await channelRow.getByTestId('overflow-more').click();
      await expect(
        page
          .getByTestId('overflow-chip-menu')
          .locator('[data-testid="channel-models-chip"][data-model="mini"][data-canonical="true"]'),
      ).toBeVisible();
      await page.keyboard.press('Escape');
      const afterSave = await chatCompletionsStatus(page, token.token_key, manualId);
      expect(afterSave.status).toBe(503);
      expect(afterSave.message).toContain('价格');

      await channelRow.getByTestId('channel-edit').click();
      await expect(
        page.locator(`[data-testid="channel-model-chip"][data-model="${manualId}"]`),
      ).toBeVisible();

      await page.getByTestId('channel-sync-models').click();
      await page.getByTestId('channel-sync-run').click();
      const manualSyncRow = page.locator(
        `[data-testid="channel-sync-row"][data-model="${manualId}"]`,
      );
      await expect(manualSyncRow).toBeVisible();
      await expect(manualSyncRow.getByTestId('channel-sync-status-selected')).toBeVisible();
      await page.getByTestId('channel-sync-back').click();
      await expect(
        page.locator(`[data-testid="channel-model-chip"][data-model="${manualId}"]`),
      ).toBeVisible();
      await page.getByTestId('channel-save').click();

      await page.goto('/models');
      const unpriced = page.locator(`[data-testid="inventory-row"][data-model="${manualId}"]`);
      await expect(unpriced).toBeVisible();
      await expect(unpriced.getByTestId('inventory-unpriced')).toBeVisible();

      await page.goto('/channel');
      await page.getByRole('button', { name: '中 / EN' }).click();
      await channelRow.getByTestId('channel-edit').click();
      await expect(page.getByTestId('channel-add-model')).toHaveText('添加');
      await page.getByTestId('channel-add-model').click();
      await expect(page.locator('[data-form-field="addModel"]')).toContainText('请输入模型 ID');
      await page.getByTestId('channel-add-model-input').fill(manualId);
      await page.getByTestId('channel-add-model').click();
      await expect(page.locator('[data-form-field="addModel"]')).toContainText('已在清单中');
      await page.getByTestId('channel-add-model-input').fill('mini');
      await page.getByTestId('channel-add-model').click();
      await expect(page.locator('[data-form-field="addModel"]')).toContainText('已是别名');
    } finally {
      await upstream.close();
    }
  });
});

test.describe('channel alias occupancy', () => {
  test('refuses an alias that occupies another selected model, then keeps it as a nickname after uncheck', async ({
    page,
  }) => {
    const alpha = 'e2e-occ-alpha';
    const beta = 'e2e-occ-beta';
    const gamma = 'e2e-occ-gamma';
    const channelName = 'e2e-occ-channel';
    const upstream = await startProbeUpstream(200, { models: [alpha, beta, gamma] });
    try {
      await page.goto('/channel');
      await page.getByTestId('create-channel').click();
      await page.locator('[id^="channel-editor-name"]').fill(channelName);
      await page.locator('[id^="channel-editor-base-url"]').fill(upstream.baseUrl);
      await page.locator('[id^="channel-editor-api-key"]').fill('sk-upstream');
      await page.getByTestId('channel-sync-models').click();
      await page.getByTestId('channel-sync-run').click();

      const alphaRow = page.locator(`[data-testid="channel-sync-row"][data-model="${alpha}"]`);
      const betaRow = page.locator(`[data-testid="channel-sync-row"][data-model="${beta}"]`);
      const gammaRow = page.locator(`[data-testid="channel-sync-row"][data-model="${gamma}"]`);
      await alphaRow.getByTestId('channel-sync-checkbox').click();
      await betaRow.getByTestId('channel-sync-checkbox').click();
      await gammaRow.getByTestId('channel-sync-checkbox').click();
      await alphaRow.getByTestId('channel-sync-alias-input').fill(beta);

      const conflict = page.getByTestId('channel-sync-alias-conflict');
      await expect(conflict).toBeVisible();
      await expect(conflict).toContainText(beta);
      await expect(alphaRow.getByTestId('channel-sync-alias-input')).toHaveAttribute(
        'aria-invalid',
        'true',
      );
      await page.getByTestId('channel-sync-back').click();
      await expect(page.getByTestId('channel-sync-view')).toBeVisible();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(0);

      await alphaRow.getByTestId('channel-sync-alias-input').fill('e2e-occ-nick');
      await betaRow.getByTestId('channel-sync-alias-input').fill('e2e-occ-nick');
      await expect(conflict).toContainText('e2e-occ-nick');
      await betaRow.getByTestId('channel-sync-alias-input').fill('');
      await alphaRow.getByTestId('channel-sync-alias-input').fill(beta);
      await expect(conflict).toContainText(beta);

      await betaRow.getByTestId('channel-sync-checkbox').click();
      await expect(conflict).toHaveCount(0);
      await page.getByTestId('channel-sync-back').click();
      await expect(page.getByTestId('channel-model-chip')).toHaveCount(3);
      await expect(
        page.locator(`[data-testid="channel-model-chip"][data-model="${alpha}"]`),
      ).toBeVisible();
      await expect(
        page.locator(`[data-testid="channel-model-chip"][data-model="${beta}"]`),
      ).toHaveClass(/model-chip-alias/);
      await expect(
        page.locator(`[data-testid="channel-model-chip"][data-model="${gamma}"]`),
      ).toBeVisible();

      await page.getByTestId('channel-save').click();
      const channelRow = page.locator(
        `[data-testid="channel-row"][data-channel-name="${channelName}"]`,
      );
      await expect(channelRow).toBeVisible();

      await page.goto('/models');
      await expect(
        page.locator(
          `[data-testid="inventory-row"][data-model="${alpha}"][data-section-channel="${channelName}"]`,
        ),
      ).toBeVisible();
      await expect(
        page.locator(
          `[data-testid="inventory-row"][data-model="${gamma}"][data-section-channel="${channelName}"]`,
        ),
      ).toBeVisible();
      await expect(page.locator(`[data-testid="inventory-row"][data-model="${beta}"]`)).toHaveCount(
        0,
      );
      await expect(
        page.locator(
          `[data-testid="inventory-row"][data-model="${alpha}"] [data-testid="inventory-alias-chip"]`,
        ),
      ).toHaveText(beta);
    } finally {
      await upstream.close();
    }
  });
});
