import { authedTest as test, expect } from './fixtures';
import { seedRequestLogs, seedSystemLogs, utf8Bytes } from './helpers/seed-logs';
import { usdLabel } from './helpers/usd';

test.describe.configure({ mode: 'serial' });

function toDatetimeLocal(millis: number): string {
  const date = new Date(millis);
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

test.describe('request logs page', () => {
  test('filters by keyword and time range, paginates, and decodes bodies', async ({ page }) => {
    const now = Date.now();
    const older = now - 3_600_000;
    const jsonRequest = utf8Bytes('{"hello":"world","n":1}');
    const jsonResponse = utf8Bytes('{"ok":true}');
    const binaryBody = new Uint8Array([0xff, 0xfe, 0x00, 0x01]);

    seedRequestLogs([
      ...Array.from({ length: 24 }, (_, index) => ({
        created_at: now - (index + 1) * 1_000,
        token_key: 'sk-e2e-logs-page',
        token_name: 'Pager',
        model: 'e2e-page-model',
        channel: 'e2e-page-channel',
        status_code: 200,
        cost_usd_micros: 100,
      })),
      {
        created_at: now,
        token_key: 'sk-e2e-logs-page',
        token_name: 'Pager',
        model: 'e2e-json-model',
        channel: 'e2e-page-channel',
        status_code: 200,
        latency_ms: 42,
        cost_usd_micros: 1_500,
        request_body: jsonRequest,
        response_body: jsonResponse,
      },
      {
        created_at: older,
        token_key: 'sk-e2e-logs-filter',
        token_name: 'Filter token',
        model: 'e2e-filter-model',
        channel: 'e2e-filter-channel',
        status_code: 429,
        latency_ms: 9,
        cost_usd_micros: 0,
        request_body: binaryBody,
        response_body: utf8Bytes('plain text body'),
      },
    ]);

    await page.goto('/logs');
    await expect(page.getByRole('tab', { name: /request logs/i })).toBeVisible();

    await page.locator('#logs-search').fill('sk-e2e-logs-page');

    await expect(page.getByTestId('log-row')).toHaveCount(20);
    await expect(page.getByTestId('logs-pagination-summary')).toContainText('25');
    await page.getByTestId('logs-next').click();
    await expect(page.getByTestId('log-row')).toHaveCount(5);
    await page.getByTestId('logs-prev').click();
    await expect(page.getByTestId('log-row')).toHaveCount(20);

    const jsonRow = page.locator('[data-testid="log-row"][data-model="e2e-json-model"]');
    await expect(jsonRow).toHaveAttribute('data-status-code', '200');
    await expect(jsonRow.getByTestId('log-channel')).toHaveText('e2e-page-channel');
    await expect(jsonRow.getByTestId('log-latency')).toHaveText('42ms');
    await expect(jsonRow.getByTestId('log-speed')).toHaveText('—');
    await expect(jsonRow.getByTestId('log-cost')).toHaveText(usdLabel(1_500));

    await jsonRow.getByTestId('log-expand').click();
    const requestBody = page.getByTestId('log-request-body');
    await expect(requestBody).toContainText('"hello": "world"');
    await expect(requestBody).toContainText('"n": 1');
    await expect(page.getByTestId('log-response-body')).toContainText('"ok": true');

    await page.getByTestId('log-request-copy').click();
    await expect(page.getByTestId('log-request-copy')).toHaveText(/copied/i);
    await page.keyboard.press('Escape');

    await page.locator('#logs-search').fill('sk-e2e-logs-filter');
    await expect(page.getByTestId('log-row')).toHaveCount(1);
    const filterRow = page.getByTestId('log-row');
    await expect(filterRow).toHaveAttribute('data-status-code', '429');
    await expect(filterRow.getByTestId('log-model')).toHaveText('e2e-filter-model');

    await filterRow.getByTestId('log-expand').click();
    await expect(page.getByTestId('log-request-body-binary')).toBeVisible();
    await expect(page.getByTestId('log-request-body-binary')).toContainText(/binary/i);
    await expect(page.getByTestId('log-response-body')).toHaveText('plain text body');
    await page.keyboard.press('Escape');

    // 时间范围改经复合选择器：打开弹层 → 快速选择「今天」→ 精调起止 → 确认。
    await page.getByTestId('logs-time-range').click();
    await page.getByTestId('date-range-quick-today').click();
    await expect(page.locator('#logs-from')).not.toHaveValue('');
    await page.locator('#logs-from').fill(toDatetimeLocal(now - 60_000));
    await page.locator('#logs-to').fill(toDatetimeLocal(now + 60_000));
    await page.getByTestId('date-range-confirm').click();
    await page.locator('#logs-search').fill('sk-e2e-logs-page');
    await expect(page.getByTestId('log-row').first()).toBeVisible();
    await expect(page.getByTestId('log-row')).toHaveCount(20);

    await page.getByTestId('logs-clear-filters').click();
    await page.locator('#logs-search').fill('sk-e2e-logs-filter');
    await page.getByTestId('logs-time-range').click();
    await page.locator('#logs-from').fill(toDatetimeLocal(now - 60_000));
    await page.locator('#logs-to').fill(toDatetimeLocal(now + 60_000));
    await page.getByTestId('date-range-confirm').click();
    await expect(page.getByTestId('logs-empty')).toBeVisible();
  });

  test('list shows inbound model; details show outbound model and channel', async ({ page }) => {
    seedRequestLogs([
      {
        created_at: Date.now(),
        token_key: 'sk-e2e-logs-alias',
        token_name: 'Alias token',
        model: 'fast',
        outbound_model: 'gpt-4o-mini',
        channel: 'alias-channel',
        status_code: 200,
      },
    ]);

    await page.goto('/logs');
    await page.locator('#logs-search').fill('sk-e2e-logs-alias');
    const row = page.locator('[data-testid="log-row"][data-model="fast"]');
    await expect(row.getByTestId('log-model')).toHaveText('fast');
    await expect(row.getByTestId('log-channel')).toHaveText('alias-channel');

    await row.getByTestId('log-expand').click();
    await expect(page.getByTestId('log-outbound-model')).toHaveText('gpt-4o-mini');
    await expect(page.getByTestId('log-detail-channel')).toHaveText('alias-channel');
  });

  test('filters unsettled request logs and lists system logs on a separate tab', async ({
    page,
  }) => {
    seedRequestLogs([
      {
        created_at: Date.now(),
        token_key: 'sk-e2e-settled',
        token_name: 'Settled',
        model: 'e2e-settled-model',
        channel: 'e2e-settled-channel',
        status_code: 200,
        cost_usd_micros: 100,
        settled: true,
      },
      {
        created_at: Date.now() - 1_000,
        token_key: 'sk-e2e-unsettled',
        token_name: 'Unsettled',
        model: 'e2e-unsettled-model',
        channel: 'e2e-unsettled-channel',
        status_code: 200,
        cost_usd_micros: 200,
        settled: false,
      },
    ]);
    seedSystemLogs([
      {
        level: 'error',
        target: 'billing',
        message: 'e2e settlement failed',
      },
      {
        level: 'warn',
        target: 'catalog',
        message: 'e2e catalog warning',
      },
    ]);

    await page.goto('/logs');
    await expect(page.getByTestId('logs-unsettled-total')).toContainText('1');
    await page.getByTestId('logs-settled-filter').click();
    await page.locator('[data-testid="logs-settled-filter-option"][data-value="false"]').click();
    await expect(page.getByTestId('log-row')).toHaveCount(1);
    await expect(page.getByTestId('log-unsettled')).toBeVisible();
    await expect(
      page.locator('[data-testid="log-row"][data-model="e2e-unsettled-model"]'),
    ).toBeVisible();

    await page.getByTestId('logs-tab-system').click();
    // 不断言总数：审计日志（登录成功等 info 行）也落在这张表里，条数随用例增长。
    await expect(
      page.getByTestId('system-log-row').filter({ hasText: 'e2e settlement failed' }),
    ).toBeVisible();
    await expect(
      page.getByTestId('system-log-row').filter({ hasText: 'e2e catalog warning' }),
    ).toBeVisible();
    await page.getByTestId('system-logs-level-filter').click();
    await page
      .locator('[data-testid="system-logs-level-filter-option"][data-value="warn"]')
      .click();
    // 同样不断言总数：登录失败也记 warn（审计），整个 e2e 跑共用一个库。
    const warned = page
      .getByTestId('system-log-row')
      .filter({ hasText: 'e2e catalog warning' });
    await expect(warned).toHaveCount(1);
    await expect(warned.getByTestId('system-log-target')).toHaveText('catalog');
    await expect(
      page.getByTestId('system-log-row').filter({ hasText: 'e2e settlement failed' }),
    ).toHaveCount(0, { timeout: 5000 });

    await page.keyboard.press('Escape');
    await page.getByTestId('system-logs-clear-filters').click();
    await page.getByTestId('system-logs-target-filter').click();
    await page
      .locator('[data-testid="system-logs-target-filter-option"][data-value="billing"]')
      .click();
    await expect(page.getByTestId('system-log-row')).toHaveCount(1);
    await expect(page.getByTestId('system-log-target')).toHaveText('billing');
    await expect(page.getByTestId('system-log-message')).toContainText('e2e settlement failed');
  });

  test('hides request-log columns from the toolbar', async ({ page }) => {
    seedRequestLogs([
      {
        created_at: Date.now(),
        token_key: 'sk-e2e-logs-columns',
        token_name: 'Columns token',
        model: 'e2e-columns-model',
        channel: 'e2e-columns-channel',
        status_code: 200,
      },
    ]);

    await page.addInitScript(() => {
      localStorage.removeItem('kairos-logs-columns');
    });
    await page.goto('/logs');
    await page.locator('#logs-search').fill('sk-e2e-logs-columns');
    const row = page.locator('[data-testid="log-row"][data-model="e2e-columns-model"]');
    await expect(row).toBeVisible();
    await expect(page.getByRole('columnheader', { name: 'Cache', exact: true })).toHaveCount(0);

    await page.getByTestId('logs-columns').click();
    await page.locator('[data-testid="logs-columns-option"][data-value="cache"]').click();
    await expect(page.getByRole('button', { name: 'Cache', exact: true })).toBeVisible();
    await page.locator('[data-testid="logs-columns-option"][data-value="token"]').click();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Token', exact: true })).toHaveCount(0);
    await expect(row.getByText('Columns token')).toHaveCount(0);
  });

  test('hides system-log columns from the toolbar', async ({ page }) => {
    seedSystemLogs([
      {
        level: 'info',
        target: 'e2e-columns',
        message: 'e2e column visibility',
      },
    ]);

    await page.addInitScript(() => {
      localStorage.removeItem('kairos-system-logs-columns');
    });
    await page.goto('/logs');
    await page.getByTestId('logs-tab-system').click();
    const row = page.getByTestId('system-log-row').filter({ hasText: 'e2e column visibility' });
    await expect(row).toBeVisible();
    await expect(row.getByTestId('system-log-level')).toBeVisible();

    await page.getByTestId('system-logs-columns').click();
    await page.locator('[data-testid="system-logs-columns-option"][data-value="level"]').click();
    await page.keyboard.press('Escape');
    await expect(page.getByTestId('system-log-level')).toHaveCount(0);
  });

  test('sorts request logs from the column header', async ({ page }) => {
    seedRequestLogs([
      {
        created_at: Date.now(),
        token_key: 'sk-e2e-logs-sort',
        token_name: 'Sort token',
        model: 'e2e-sort-expensive',
        channel: 'e2e-sort-channel',
        status_code: 200,
        cost_usd_micros: 5_000,
      },
      {
        created_at: Date.now() - 1_000,
        token_key: 'sk-e2e-logs-sort',
        token_name: 'Sort token',
        model: 'e2e-sort-cheap',
        channel: 'e2e-sort-channel',
        status_code: 200,
        cost_usd_micros: 100,
      },
    ]);

    await page.goto('/logs');
    await page.locator('#logs-search').fill('sk-e2e-logs-sort');
    await expect(page.getByTestId('log-row')).toHaveCount(2);
    await expect(page.getByTestId('log-row').first()).toHaveAttribute(
      'data-model',
      'e2e-sort-expensive',
    );

    await page.getByRole('button', { name: 'Cost', exact: true }).click();
    await page.getByTestId('column-sort-asc').click();
    await expect(page.getByTestId('log-row').first()).toHaveAttribute(
      'data-model',
      'e2e-sort-cheap',
    );
    await expect(page.getByTestId('log-row').nth(1)).toHaveAttribute(
      'data-model',
      'e2e-sort-expensive',
    );

    await page.getByTestId('column-clear-sort').click();
    await expect(page.getByTestId('log-row').first()).toHaveAttribute(
      'data-model',
      'e2e-sort-expensive',
    );
    await expect(page.getByTestId('column-clear-sort')).toHaveCount(0);
  });

  test('filters request logs by inbound protocol instead of sorting it', async ({ page }) => {
    seedRequestLogs([
      {
        created_at: Date.now(),
        token_key: 'sk-e2e-logs-protocol',
        token_name: 'Protocol token',
        model: 'e2e-protocol-chat',
        channel: 'e2e-protocol-channel',
        inbound_protocol: 'openai_chat',
        status_code: 200,
      },
      {
        created_at: Date.now() - 1_000,
        token_key: 'sk-e2e-logs-protocol',
        token_name: 'Protocol token',
        model: 'e2e-protocol-anthropic',
        channel: 'e2e-protocol-channel',
        inbound_protocol: 'anthropic_messages',
        status_code: 200,
      },
    ]);

    await page.goto('/logs');
    await page.locator('#logs-search').fill('sk-e2e-logs-protocol');
    await expect(page.getByTestId('log-row')).toHaveCount(2);
    await expect(
      page.getByRole('columnheader', { name: 'Inbound Protocol', exact: true }),
    ).toHaveCount(0);

    await page.getByTestId('logs-protocol-filter').click();
    await page
      .locator('[data-testid="logs-protocol-filter-option"][data-value="anthropic_messages"]')
      .click();
    await expect(page.getByTestId('log-row')).toHaveCount(1);
    await expect(page.getByTestId('log-row')).toHaveAttribute(
      'data-model',
      'e2e-protocol-anthropic',
    );
  });

  test('row filter matches the column exactly, not a keyword substring', async ({ page }) => {
    seedRequestLogs([
      {
        created_at: Date.now(),
        token_key: 'sk-e2e-exact-filter',
        token_name: 'Exact token',
        model: 'e2e-exact-model',
        channel: 'e2e-exact-channel',
        status_code: 200,
      },
      {
        created_at: Date.now() - 1_000,
        token_key: 'sk-e2e-exact-filter',
        token_name: 'Exact token',
        model: 'e2e-exact-model-plus',
        channel: 'e2e-exact-channel',
        status_code: 200,
      },
    ]);

    await page.goto('/logs');
    await page.locator('#logs-search').fill('sk-e2e-exact-filter');
    await expect(page.getByTestId('log-row')).toHaveCount(2);

    const row = page.locator('[data-testid="log-row"][data-model="e2e-exact-model"]');
    await row.getByTestId('log-filter-model').click();
    await expect(page.getByTestId('log-row')).toHaveCount(1);
    await expect(page.getByTestId('log-row')).toHaveAttribute('data-model', 'e2e-exact-model');
    await expect(page.getByTestId('logs-exact-model')).toContainText('e2e-exact-model');
  });
});

test('details show base price, discount, and charged amount; list filters by discount', async ({ page }) => {
  seedRequestLogs([
    {
      created_at: Date.now(),
      token_key: 'sk-e2e-discount',
      token_name: 'Discount token',
      model: 'e2e-discount-model',
      channel: 'e2e-discount-channel',
      status_code: 200,
      base_cost_usd_micros: 1_000_000,
      discount_bp: 8000,
      cost_usd_micros: 800_000,
    },
  ]);

  await page.goto('/logs');
  await page.locator('#logs-search').fill('sk-e2e-discount');
  const row = page.locator('[data-testid="log-row"][data-model="e2e-discount-model"]');
  await expect(row).toBeVisible();

  await row.getByTestId('log-expand').click();
  await expect(page.getByTestId('log-base-cost')).toHaveText(usdLabel(1_000_000));
  await expect(page.getByTestId('log-discount-rate')).toContainText('80%');
  await expect(page.getByTestId('log-charged-cost')).toHaveText(usdLabel(800_000));
  await page.keyboard.press('Escape');

  await page.getByTestId('logs-discount-filter').click();
  await page.locator('[data-testid="logs-discount-filter-option"][data-value="8000"]').click();
  await page.keyboard.press('Escape');
  await expect(page.locator('[data-testid="log-row"]')).toHaveCount(1);
});
