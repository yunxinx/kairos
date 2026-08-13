import type { APIRequestContext, Page } from '@playwright/test';
import { authedTest as test, expect } from './fixtures';
import { E2E_ADMIN_KEY } from './helpers/gateway';
import { MS_PER_DAY, seedRequestLogs, utcDayStart } from './helpers/seed-logs';
import { usdLabel } from './helpers/usd';
import type { StatsView } from '../src/api/types';
import { HEATMAP_WEEK_COUNT } from '../src/features/overview/heatmap';
import { formatCount } from '../src/lib/format';

test.describe.configure({ mode: 'serial' });

async function fetchStats(request: APIRequestContext, days: number): Promise<StatsView> {
  const resp = await request.get(`/stats?days=${days}`, {
    headers: { Authorization: `Bearer ${E2E_ADMIN_KEY}` },
  });
  expect(resp.ok()).toBeTruthy();
  return (await resp.json()) as StatsView;
}

function seedOverviewLogs(now: number): { today: string; yesterday: string; eightDaysAgo: string } {
  const todayStart = utcDayStart(now);
  const yesterday = todayStart - MS_PER_DAY;
  const eightDaysAgo = todayStart - 8 * MS_PER_DAY;
  seedRequestLogs([
    {
      created_at: todayStart + 1,
      token_key: 'sk-e2e-ov',
      model: 'e2e-ov-gpt',
      channel: 'e2e-ov-primary',
      status_code: 200,
      input_tokens: 10,
      output_tokens: 4,
      cost_usd_micros: 1_000,
    },
    {
      created_at: todayStart + 2,
      token_key: 'sk-e2e-ov',
      model: 'e2e-ov-gpt',
      channel: 'e2e-ov-primary',
      status_code: 200,
      input_tokens: 20,
      output_tokens: 8,
      cost_usd_micros: 2_000,
    },
    {
      created_at: todayStart + 3,
      token_key: 'sk-e2e-ov',
      model: 'e2e-ov-gpt',
      channel: 'e2e-ov-primary',
      status_code: 500,
      input_tokens: 0,
      output_tokens: 0,
      cost_usd_micros: 999_999,
    },
    {
      created_at: yesterday + 1,
      token_key: 'sk-e2e-ov',
      model: 'e2e-ov-mini',
      channel: 'e2e-ov-other',
      status_code: 200,
      input_tokens: 5,
      output_tokens: 1,
      cost_usd_micros: 500,
    },
    {
      created_at: eightDaysAgo + 1,
      token_key: 'sk-e2e-ov',
      model: 'e2e-ov-gpt',
      channel: 'e2e-ov-primary',
      status_code: 200,
      input_tokens: 1,
      output_tokens: 1,
      cost_usd_micros: 100,
    },
  ]);
  const iso = (millis: number) => new Date(millis).toISOString().slice(0, 10);
  return { today: iso(todayStart), yesterday: iso(yesterday), eightDaysAgo: iso(eightDaysAgo) };
}

async function expectCardsMatchStats(page: Page, stats: StatsView): Promise<void> {
  await expect(page.getByTestId('overview-request-count')).toHaveText(
    formatCount(stats.summary.request_count, 'en'),
  );
  await expect(page.getByTestId('overview-success-count')).toHaveText(
    formatCount(stats.summary.success_count, 'en'),
  );
  await expect(page.getByTestId('overview-input-tokens')).toHaveText(
    formatCount(stats.summary.input_tokens, 'en'),
  );
  await expect(page.getByTestId('overview-output-tokens')).toHaveText(
    formatCount(stats.summary.output_tokens, 'en'),
  );
  await expect(page.getByTestId('overview-cost')).toHaveText(
    usdLabel(stats.summary.cost_usd_micros),
  );
  await expect(page.getByTestId('overview-token-count')).toHaveText(
    formatCount(stats.summary.token_count, 'en'),
  );
  await expect(page.getByTestId('overview-channel-count')).toHaveText(
    formatCount(stats.summary.channel_count, 'en'),
  );
}

test.describe('overview page', () => {
  test('renders summary cards, daily points, and shares matching /stats', async ({
    page,
    request,
  }) => {
    const { today, yesterday } = seedOverviewLogs(Date.now());
    await page.goto('/overview');
    await expect(page.getByRole('heading', { name: /overview/i })).toBeVisible();

    const stats = await fetchStats(request, 7);
    await expectCardsMatchStats(page, stats);

    const expectedToday = stats.daily.find((point) => point.date === today);
    expect(expectedToday).toBeDefined();
    const todayPoint = page.locator(`[data-testid="overview-daily-point"][data-date="${today}"]`);
    await expect(todayPoint).toHaveAttribute(
      'data-request-count',
      String(expectedToday?.request_count),
    );
    await expect(todayPoint).toHaveAttribute(
      'data-input-tokens',
      String(expectedToday?.input_tokens),
    );
    await expect(todayPoint).toHaveAttribute(
      'data-output-tokens',
      String(expectedToday?.output_tokens),
    );
    await expect(todayPoint).toHaveAttribute(
      'data-cost-usd-micros',
      String(expectedToday?.cost_usd_micros),
    );

    const expectedYesterday = stats.daily.find((point) => point.date === yesterday);
    expect(expectedYesterday).toBeDefined();
    const yesterdayPoint = page.locator(
      `[data-testid="overview-daily-point"][data-date="${yesterday}"]`,
    );
    await expect(yesterdayPoint).toHaveAttribute(
      'data-request-count',
      String(expectedYesterday?.request_count),
    );

    await expect(page.getByTestId('overview-trend-chart').locator('canvas')).toBeVisible();

    const expectedGpt = stats.by_model.find((share) => share.model === 'e2e-ov-gpt');
    expect(expectedGpt).toBeDefined();
    const gptShare = page.locator('[data-testid="overview-model-share"][data-model="e2e-ov-gpt"]');
    await expect(gptShare).toHaveAttribute(
      'data-request-count',
      String(expectedGpt?.request_count),
    );
    await expect(gptShare).toHaveAttribute(
      'data-cost-usd-micros',
      String(expectedGpt?.cost_usd_micros),
    );
    await expect(
      page.locator('[data-testid="overview-model-share"][data-model="e2e-ov-mini"]'),
    ).toBeVisible();

    await page.getByTestId('overview-share-tab-channel').click();
    const expectedPrimary = stats.by_channel.find((share) => share.channel === 'e2e-ov-primary');
    expect(expectedPrimary).toBeDefined();
    await expect(
      page.locator('[data-testid="overview-channel-share"][data-channel="e2e-ov-primary"]'),
    ).toHaveAttribute('data-request-count', String(expectedPrimary?.request_count));
    await expect(
      page.locator('[data-testid="overview-channel-share"][data-channel="e2e-ov-primary"]'),
    ).toBeVisible();

    await expect(page.getByTestId('overview-heatmap-cell')).toHaveCount(HEATMAP_WEEK_COUNT * 7);
    const todayHeat = page.locator(`[data-testid="overview-heatmap-cell"][data-date="${today}"]`);
    await expect(todayHeat).toHaveAttribute(
      'data-request-count',
      String(expectedToday?.request_count),
    );
    await page.getByTestId('overview-heatmap').scrollIntoViewIfNeeded();
    await todayHeat.hover();
    const heatTip = page.getByTestId('overview-heatmap-tooltip');
    await expect(heatTip).toBeVisible();
    await expect(heatTip).toContainText(String(expectedToday?.request_count));
    await expect(heatTip).toContainText(usdLabel(expectedToday?.cost_usd_micros ?? 0));
  });

  test('lets the overview page scroll when content exceeds the viewport', async ({ page }) => {
    seedOverviewLogs(Date.now());
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.goto('/overview');
    await expect(page.getByRole('heading', { name: /overview/i })).toBeVisible();

    const main = page.locator('#main-content');
    const overflow = await main.evaluate((el) => el.scrollHeight - el.clientHeight);
    expect(overflow).toBeGreaterThan(0);

    await main.evaluate((el) => {
      el.scrollTop = 160;
    });
    expect(await main.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  });

  test('switches the days window and updates cards to match /stats', async ({ page, request }) => {
    const { eightDaysAgo } = seedOverviewLogs(Date.now());
    await page.goto('/overview');

    const stats7 = await fetchStats(request, 7);
    await expectCardsMatchStats(page, stats7);
    await expect(
      page.locator(`[data-testid="overview-daily-point"][data-date="${eightDaysAgo}"]`),
    ).toHaveCount(0);
    await expect(page.getByTestId('overview-heatmap-cell')).toHaveCount(HEATMAP_WEEK_COUNT * 7);
    await expect(
      page.locator(`[data-testid="overview-heatmap-cell"][data-date="${eightDaysAgo}"]`),
    ).toHaveAttribute('data-request-count', '0');

    await page.locator('#overview-days').click();
    await page.getByRole('option', { name: /90 days/i }).click();

    const stats90 = await fetchStats(request, 90);
    expect(stats90.summary.request_count).toBeGreaterThan(stats7.summary.request_count);
    await expectCardsMatchStats(page, stats90);
    const expectedOld = stats90.daily.find((point) => point.date === eightDaysAgo);
    expect(expectedOld).toBeDefined();
    await expect(
      page.locator(`[data-testid="overview-daily-point"][data-date="${eightDaysAgo}"]`),
    ).toHaveAttribute('data-request-count', String(expectedOld?.request_count));
    await expect(
      page.locator(`[data-testid="overview-heatmap-cell"][data-date="${eightDaysAgo}"]`),
    ).toHaveAttribute('data-request-count', String(expectedOld?.request_count));

    await page.locator('#overview-days').click();
    await page.getByRole('option', { name: /1 day/i }).click();
    const stats1 = await fetchStats(request, 1);
    expect(stats1.daily).toHaveLength(24);
    await expectCardsMatchStats(page, stats1);
    await expect(page.getByTestId('overview-daily-point')).toHaveCount(24);
    await expect(page.getByTestId('overview-heatmap-cell')).toHaveCount(HEATMAP_WEEK_COUNT * 7);
  });
});
