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

    await page.goto('/requests');
    await expect(page.getByRole('tab', { name: /request logs/i })).toBeVisible();

    await page.locator('#logs-search').fill('sk-e2e-logs-page');

    await expect(page.getByTestId('log-row')).toHaveCount(20);
    await expect(page.getByTestId('logs-pagination-summary')).toContainText('25');
    await page.getByTestId('logs-next').click();
    await expect(page.getByTestId('log-row')).toHaveCount(5);
    await page.getByTestId('logs-prev').click();
    await expect(page.getByTestId('log-row')).toHaveCount(20);

    const jsonRow = page.locator('[data-testid="log-row"][data-model="e2e-json-model"]');
    await expect(jsonRow.getByTestId('log-status')).toHaveText('200');
    await expect(jsonRow.getByTestId('log-channel')).toHaveText('e2e-page-channel');
    await expect(jsonRow.getByTestId('log-latency')).toHaveText('42 ms');
    await expect(jsonRow.getByTestId('log-cost')).toHaveText(usdLabel(1_500));

    await jsonRow.getByTestId('log-expand').click();
    const requestBody = page.getByTestId('log-request-body');
    await expect(requestBody).toContainText('"hello": "world"');
    await expect(requestBody).toContainText('"n": 1');
    await expect(page.getByTestId('log-response-body')).toContainText('"ok": true');

    await page.getByTestId('log-request-copy').click();
    await expect(page.getByTestId('log-request-copy')).toHaveText(/copied/i);

    await page.locator('#logs-search').fill('sk-e2e-logs-filter');
    await expect(page.getByTestId('log-row')).toHaveCount(1);
    const filterRow = page.getByTestId('log-row');
    await expect(filterRow.getByTestId('log-status')).toHaveText('429');
    await expect(filterRow.getByTestId('log-model')).toHaveText('e2e-filter-model');

    await filterRow.getByTestId('log-expand').click();
    await expect(page.getByTestId('log-request-body-binary')).toBeVisible();
    await expect(page.getByTestId('log-request-body-binary')).toContainText(/binary/i);
    await expect(page.getByTestId('log-response-body')).toHaveText('plain text body');

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

    await page.goto('/requests');
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

    await page.goto('/requests');
    await expect(page.getByTestId('logs-unsettled-total')).toContainText('1');
    await page.getByTestId('logs-settled-filter').click();
    await page
      .locator('[data-testid="logs-settled-filter-option"][data-value="false"]')
      .click();
    await expect(page.getByTestId('log-row')).toHaveCount(1);
    await expect(page.getByTestId('log-unsettled')).toBeVisible();
    await expect(page.locator('[data-testid="log-row"][data-model="e2e-unsettled-model"]')).toBeVisible();

    await page.getByTestId('logs-tab-system').click();
    await expect(page.getByTestId('system-log-row')).toHaveCount(2);
    await page.getByTestId('system-logs-level-filter').click();
    await page
      .locator('[data-testid="system-logs-level-filter-option"][data-value="warn"]')
      .click();
    await expect(page.getByTestId('system-log-row')).toHaveCount(1);
    await expect(page.getByTestId('system-log-target')).toHaveText('catalog');
    await expect(page.getByTestId('system-log-message')).toContainText('e2e catalog warning');

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
});
