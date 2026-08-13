import type { DailyPoint } from '@/api/types';

export type HeatmapLevel = 0 | 1 | 2 | 3 | 4;

export interface HeatmapCell {
  date: string;
  requestCount: number;
  tokenCount: number;
  costUsdMicros: number;
  level: HeatmapLevel;
}

export interface CalendarHeatmap {
  weeks: HeatmapCell[][];
  monthLabels: Array<string | null>;
}

/** GitHub 贡献图：约一年、周日→周六。 */
export const HEATMAP_WEEK_COUNT = 53;

const MS_PER_DAY = 86_400_000;

function levelForCount(count: number, max: number): HeatmapLevel {
  if (count <= 0 || max <= 0) return 0;
  const scaled = Math.ceil((count / max) * 4);
  if (scaled <= 1) return 1;
  if (scaled >= 4) return 4;
  return scaled as HeatmapLevel;
}

function formatUtcDay(millis: number): string {
  return new Date(millis).toISOString().slice(0, 10);
}

function parseUtcDay(date: string): number {
  const parts = date.split('-');
  const year = Number(parts[0]);
  const month = Number(parts[1]);
  const day = Number(parts[2]);
  return Date.UTC(year, month - 1, day);
}

function utcDayStart(millis: number): number {
  const stamp = new Date(millis);
  return Date.UTC(stamp.getUTCFullYear(), stamp.getUTCMonth(), stamp.getUTCDate());
}

function resolveLocale(locale: string): string {
  if (locale === 'zh-CN') return 'zh-CN';
  return 'en-US';
}

/** 日历月短名，UTC，避免本地时区把日期推到相邻月。 */
export function formatHeatmapMonth(date: string, locale: string): string {
  const millis = parseUtcDay(date);
  return new Intl.DateTimeFormat(resolveLocale(locale), {
    month: 'short',
    timeZone: 'UTC',
  }).format(millis);
}

/** 周日短名；`weekday` 为 `Date#getUTCDay`（0=周日）。 */
export function formatHeatmapWeekday(weekday: number, locale: string): string {
  const sunday = Date.UTC(2024, 0, 7);
  return new Intl.DateTimeFormat(resolveLocale(locale), {
    weekday: 'short',
    timeZone: 'UTC',
  }).format(sunday + weekday * MS_PER_DAY);
}

/** 小时点折成日历日；无流量日不进表，由网格补空格。 */
function rollupByDay(
  points: DailyPoint[],
): Map<string, { requestCount: number; tokenCount: number; costUsdMicros: number }> {
  const byDay = new Map<
    string,
    { requestCount: number; tokenCount: number; costUsdMicros: number }
  >();
  for (const point of points) {
    const day = point.date.slice(0, 10);
    const tokenCount = point.input_tokens + point.output_tokens;
    const prev = byDay.get(day);
    if (prev) {
      prev.requestCount += point.request_count;
      prev.tokenCount += tokenCount;
      prev.costUsdMicros += point.cost_usd_micros;
    } else {
      byDay.set(day, {
        requestCount: point.request_count,
        tokenCount,
        costUsdMicros: point.cost_usd_micros,
      });
    }
  }
  return byDay;
}

/**
 * 始终铺满约 53 周空白格（GitHub 贡献图）；`daily` 只给有流量的日子上色。
 */
export function buildHeatmap(daily: DailyPoint[], nowMillis: number = Date.now()): CalendarHeatmap {
  const byDay = rollupByDay(daily);
  let max = 0;
  for (const item of byDay.values()) {
    if (item.requestCount > max) max = item.requestCount;
  }

  let end = utcDayStart(nowMillis);
  while (new Date(end).getUTCDay() !== 6) {
    end += MS_PER_DAY;
  }
  const start = end - (HEATMAP_WEEK_COUNT * 7 - 1) * MS_PER_DAY;

  const weeks: HeatmapCell[][] = [];
  let week: HeatmapCell[] = [];
  for (let millis = start; millis <= end; millis += MS_PER_DAY) {
    const date = formatUtcDay(millis);
    const rolled = byDay.get(date);
    const requestCount = rolled?.requestCount ?? 0;
    week.push({
      date,
      requestCount,
      tokenCount: rolled?.tokenCount ?? 0,
      costUsdMicros: rolled?.costUsdMicros ?? 0,
      level: levelForCount(requestCount, max),
    });
    if (week.length === 7) {
      weeks.push(week);
      week = [];
    }
  }

  const monthLabels: Array<string | null> = weeks.map((cells, weekIndex) => {
    const firstOfMonth = cells.find((cell) => cell.date.endsWith('-01'));
    if (firstOfMonth) return firstOfMonth.date;
    if (weekIndex === 0) return cells[0]?.date ?? null;
    return null;
  });

  return { weeks, monthLabels };
}
