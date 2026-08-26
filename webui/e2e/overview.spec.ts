import type { APIRequestContext, Page } from '@playwright/test';
import { authedTest as test, expect } from './fixtures';
import { e2eRootHeaders } from './helpers/session';
import { MS_PER_DAY, seedRequestLogs, utcDayStart } from './helpers/seed-logs';
import { usdLabel } from './helpers/usd';
import type { LifetimeStats, StatsView } from '../src/api/types';
import { HEATMAP_CHART_GRID, HEATMAP_WEEK_COUNT } from '../src/features/overview/heatmap';
import { formatCount, formatTokensMillions } from '../src/lib/format';

test.describe.configure({ mode: 'serial' });

async function fetchStats(request: APIRequestContext, days: number): Promise<StatsView> {
  const resp = await request.get(`/api/stats?days=${days}`, {
    headers: await e2eRootHeaders(request),
  });
  expect(resp.ok()).toBeTruthy();
  return (await resp.json()) as StatsView;
}

async function fetchLifetimeStats(request: APIRequestContext): Promise<LifetimeStats> {
  const resp = await request.get('/api/stats/lifetime', {
    headers: await e2eRootHeaders(request),
  });
  expect(resp.ok()).toBeTruthy();
  return (await resp.json()) as LifetimeStats;
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

async function expectLifetimeMatch(page: Page, lifetime: LifetimeStats): Promise<void> {
  await expect(page.getByTestId('overview-lifetime-requests')).toHaveText(
    formatCount(lifetime.request_count, 'en'),
  );
  await expect(page.getByTestId('overview-lifetime-cost')).toHaveText(
    usdLabel(lifetime.cost_usd_micros),
  );
  await expect(page.getByTestId('overview-lifetime-tokens')).toHaveText(
    formatTokensMillions(lifetime.total_tokens),
  );
}

async function expectCardsMatchStats(page: Page, stats: StatsView): Promise<void> {
  await expect(page.getByTestId('overview-request-count')).toHaveText(
    formatCount(stats.summary.request_count, 'en'),
  );
  await expect(page.getByTestId('overview-success-count')).toHaveText(
    formatCount(stats.summary.success_count, 'en'),
  );
  await expect(page.getByTestId('overview-input-tokens')).toHaveText(
    formatTokensMillions(stats.summary.input_tokens),
  );
  await expect(page.getByTestId('overview-output-tokens')).toHaveText(
    formatTokensMillions(stats.summary.output_tokens),
  );
  await expect(page.getByTestId('overview-tokens-millions')).toHaveText(
    formatTokensMillions(stats.summary.input_tokens + stats.summary.output_tokens),
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

interface HeatmapCanvasBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface HeatmapCellIndex {
  weekIndex: number;
  dayIndex: number;
  weekCount: number;
}

/** 与 `HEATMAP_CHART_GRID` 同源；y 轴 category 0 在底部。 */
function heatmapCellCanvasPoint(
  box: HeatmapCanvasBox,
  cell: HeatmapCellIndex,
): { x: number; y: number } {
  const { left, right, top, bottom } = HEATMAP_CHART_GRID;
  const innerWidth = box.width - left - right;
  const innerHeight = box.height - top - bottom;
  return {
    x: box.x + left + ((cell.weekIndex + 0.5) / cell.weekCount) * innerWidth,
    y: box.y + top + ((6 - cell.dayIndex + 0.5) / 7) * innerHeight,
  };
}

async function heatmapCanvasBox(page: Page): Promise<HeatmapCanvasBox> {
  const heatmap = page.getByTestId('overview-heatmap');
  await heatmap.scrollIntoViewIfNeeded();
  const canvas = heatmap.locator('canvas').first();
  await expect(canvas).toBeVisible();
  const box = await canvas.boundingBox();
  expect(box).not.toBeNull();
  return box!;
}

async function findHeatmapCellIndex(
  page: Page,
  prefer: 'filled' | 'empty',
): Promise<HeatmapCellIndex> {
  const target = await page.evaluate(
    ({ weekCount, prefer: which }) => {
      const cells = [...document.querySelectorAll('[data-testid="overview-heatmap-cell"]')];
      const match =
        which === 'filled'
          ? [...cells].reverse().find((el) => Number(el.getAttribute('data-request-count')) > 0)
          : cells.find((el) => Number(el.getAttribute('data-request-count')) === 0);
      if (!(match instanceof HTMLElement)) return null;
      const index = cells.indexOf(match);
      return {
        weekIndex: Math.floor(index / 7),
        dayIndex: index % 7,
        weekCount,
      };
    },
    { weekCount: HEATMAP_WEEK_COUNT, prefer },
  );
  expect(target).not.toBeNull();
  return target!;
}

async function showHeatmapTooltip(page: Page) {
  const box = await heatmapCanvasBox(page);
  const target = await findHeatmapCellIndex(page, 'filled');
  const point = heatmapCellCanvasPoint(box, target);
  const tooltip = page.locator('.overview-heatmap-tooltip');
  await page.mouse.move(point.x, point.y);
  await expect(tooltip).toBeVisible();
  return tooltip;
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
    const lifetime = await fetchLifetimeStats(request);
    await expectCardsMatchStats(page, stats);
    await expectLifetimeMatch(page, lifetime);

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

    const expectedModelShares = [...stats.by_model]
      .sort(
        (left, right) =>
          right.cost_usd_micros - left.cost_usd_micros || right.request_count - left.request_count,
      )
      .slice(0, 5);
    const modelShares = page.getByTestId('overview-model-share');
    await expect(modelShares).toHaveCount(expectedModelShares.length);
    for (const share of expectedModelShares) {
      const modelShare = page.locator(
        `[data-testid="overview-model-share"][data-model="${share.model}"]`,
      );
      await expect(modelShare).toBeVisible();
      await expect(modelShare).toHaveAttribute('data-request-count', String(share.request_count));
      await expect(modelShare).toHaveAttribute(
        'data-cost-usd-micros',
        String(share.cost_usd_micros),
      );
    }

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
    await expect(page.getByTestId('overview-heatmap').locator('canvas').first()).toBeVisible();
  });

  test('hides the heatmap tooltip after the pointer leaves the chart', async ({ page }) => {
    seedOverviewLogs(Date.now());
    await page.goto('/overview');
    const tooltip = await showHeatmapTooltip(page);

    await page.getByRole('heading', { name: /overview/i }).hover();
    await expect(tooltip).toBeHidden();
  });

  test('hides the heatmap tooltip when moving from a filled cell to an empty cell', async ({
    page,
  }) => {
    seedOverviewLogs(Date.now());
    await page.goto('/overview');
    const tooltip = await showHeatmapTooltip(page);

    const box = await heatmapCanvasBox(page);
    const empty = await findHeatmapCellIndex(page, 'empty');
    const point = heatmapCellCanvasPoint(box, empty);
    await page.mouse.move(point.x, point.y);
    await expect(tooltip).toBeHidden();
  });

  test('hides the heatmap tooltip after leaving even if the visualMap was clicked', async ({
    page,
  }) => {
    const now = Date.now();
    seedOverviewLogs(now);
    const todayStart = utcDayStart(now);
    seedRequestLogs(
      Array.from({ length: 12 }, (_, index) => ({
        created_at: todayStart + 10 + index,
        token_key: 'sk-e2e-ov',
        model: 'e2e-ov-gpt',
        channel: 'e2e-ov-primary',
        status_code: 200,
        input_tokens: 1,
        output_tokens: 1,
        cost_usd_micros: 10,
      })),
    );

    await page.goto('/overview');
    const heatmap = page.getByTestId('overview-heatmap');
    await heatmap.scrollIntoViewIfNeeded();
    const canvas = heatmap.locator('canvas').first();
    await expect(canvas).toBeVisible();
    const box = await canvas.boundingBox();
    expect(box).not.toBeNull();

    const visualMapY = box!.y + box!.height * 0.93;
    for (const xRatio of [0.42, 0.46, 0.5, 0.54, 0.58]) {
      await page.mouse.click(box!.x + box!.width * xRatio, visualMapY);
    }

    const tooltip = await showHeatmapTooltip(page);
    await page.mouse.move(box!.x - 8, box!.y - 8);
    await expect(tooltip).toBeHidden();
  });

  test('aligns share and activity columns to the same height', async ({ page }) => {
    seedOverviewLogs(Date.now());
    await page.goto('/overview');
    await expect(page.getByTestId('overview-share-panel')).toBeVisible();
    await expect(page.getByTestId('overview-activity-stack')).toBeVisible();

    const layout = await page.evaluate(() => {
      const share = document.querySelector('[data-testid="overview-share-panel"]');
      const stack = document.querySelector('[data-testid="overview-activity-stack"]');
      if (!(share instanceof HTMLElement) || !(stack instanceof HTMLElement)) {
        return null;
      }
      const shareBox = share.getBoundingClientRect();
      const stackBox = stack.getBoundingClientRect();
      return {
        shareHeight: shareBox.height,
        stackHeight: stackBox.height,
        topDelta: Math.abs(shareBox.top - stackBox.top),
        bottomDelta: Math.abs(shareBox.bottom - stackBox.bottom),
      };
    });

    expect(layout).not.toBeNull();
    expect(Math.abs(layout!.shareHeight - layout!.stackHeight)).toBeLessThan(0.5);
    expect(layout!.topDelta).toBeLessThan(0.5);
    expect(layout!.bottomDelta).toBeLessThan(0.5);
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
    const lifetime = await fetchLifetimeStats(request);
    await expectCardsMatchStats(page, stats7);
    await expectLifetimeMatch(page, lifetime);
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
    await expectLifetimeMatch(page, lifetime);
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
    await expectLifetimeMatch(page, lifetime);
    await expect(page.getByTestId('overview-daily-point')).toHaveCount(24);
    await expect(page.getByTestId('overview-heatmap-cell')).toHaveCount(HEATMAP_WEEK_COUNT * 7);
  });
});
