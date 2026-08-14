import type { CalendarHeatmap } from '@/features/overview/heatmap';
import {
  buildHeatmapTierScale,
  formatHeatmapMonth,
  HEATMAP_TIER_MIN_REQUESTS,
} from '@/features/overview/heatmap';

function readCssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function buildChartColors(): string[] {
  return [
    readCssVar('--seed-primary') || '#3b82f6',
    readCssVar('--info') || '#3b82f6',
    readCssVar('--success') || '#16a34a',
    readCssVar('--warn') || '#eab308',
  ];
}

function axisStyle(): { color: string } {
  return { color: readCssVar('--fg-subtle') };
}

function mixChannel(weight: number, base: number, accent: number): number {
  return Math.round(base + (accent - base) * weight);
}

/** 与 CSS `color-mix(in srgb, accent p%, base)` 对齐的近似混色。 */
function mixHex(baseHex: string, accentHex: string, weight: number): string {
  const parse = (hex: string): [number, number, number] => {
    const normalized = hex.replace('#', '');
    const value = Number.parseInt(normalized, 16);
    return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
  };
  const [br, bg, bb] = parse(baseHex);
  const [ar, ag, ab] = parse(accentHex);
  const r = mixChannel(weight, br, ar);
  const g = mixChannel(weight, bg, ag);
  const b = mixChannel(weight, bb, ab);
  return `#${[r, g, b].map((channel) => channel.toString(16).padStart(2, '0')).join('')}`;
}

function heatmapLevelColors(): string[] {
  const primary = readCssVar('--seed-primary') || '#3b82f6';
  const surface = readCssVar('--seed-surface') || '#ffffff';
  const surfaceAlt = readCssVar('--seed-surface-alt') || '#f4f4f5';
  return [
    surfaceAlt,
    mixHex(surface, primary, 0.2),
    mixHex(surface, primary, 0.4),
    mixHex(surface, primary, 0.6),
    mixHex(surface, primary, 0.8),
    primary,
  ];
}

function tooltipStyle(): Record<string, unknown> {
  const border = readCssVar('--seed-border');
  const shadow = readCssVar('--card-shadow');
  return {
    backgroundColor: readCssVar('--seed-surface'),
    borderColor: border,
    borderWidth: 1,
    textStyle: { color: readCssVar('--seed-fg') },
    extraCssText: `box-shadow: 2px 3px 0 0 ${shadow};`,
    axisPointer: { lineStyle: { color: border } },
  };
}

/** UTC 小时标签 `YYYY-MM-DDTHH:00:00Z` → `HH:00`；日历日原样。 */
export function formatTrendLabel(date: string): string {
  const hour = /^(\d{4}-\d{2}-\d{2})T(\d{2}):/.exec(date);
  if (hour) {
    return `${hour[2]}:00`;
  }
  return date;
}

/** 趋势：请求量走左轴，费用（美元）走右轴。浮窗颜色跟当前主题。 */
export function buildTrendChartOption(
  labels: string[],
  series: { name: string; data: number[]; yAxisIndex?: number }[],
): Record<string, unknown> {
  const colors = buildChartColors();
  const border = readCssVar('--seed-border');
  return {
    color: colors,
    animationDurationUpdate: 200,
    grid: { left: 48, right: 56, top: 36, bottom: 32 },
    legend: {
      top: 0,
      textStyle: axisStyle(),
    },
    tooltip: {
      trigger: 'axis',
      ...tooltipStyle(),
    },
    xAxis: {
      type: 'category',
      data: labels,
      axisLine: { lineStyle: { color: border } },
      axisLabel: axisStyle(),
    },
    yAxis: [
      {
        type: 'value',
        axisLine: { show: false },
        splitLine: { lineStyle: { color: border } },
        axisLabel: axisStyle(),
      },
      {
        type: 'value',
        axisLine: { show: false },
        splitLine: { show: false },
        axisLabel: axisStyle(),
      },
    ],
    series: series.map((item) => ({
      name: item.name,
      type: 'line',
      smooth: true,
      showSymbol: labels.length <= 14,
      yAxisIndex: item.yAxisIndex ?? 0,
      areaStyle: { opacity: 0.06 },
      data: item.data,
    })),
  };
}

export interface HeatmapTooltipLabels {
  requests: string;
  tokenSpend: string;
  cost: string;
  less: string;
  more: string;
}

/** GitHub 式周列热力：顶栏月份、无左侧星期、底部 visualMap 联动高亮。 */
export function buildHeatmapChartOption(
  heatmap: CalendarHeatmap,
  seriesData: [number, number, number][],
  maxCount: number,
  locale: string,
  labels: HeatmapTooltipLabels,
  formatters: {
    count: (value: number) => string;
    tokens: (value: number) => string;
    cost: (value: number) => string;
  },
): Record<string, unknown> {
  const colors = heatmapLevelColors();
  const border = readCssVar('--seed-border');
  const weekCount = heatmap.weeks.length;
  const tierScale = buildHeatmapTierScale(maxCount);
  const visualMax = Math.max(maxCount, HEATMAP_TIER_MIN_REQUESTS);
  const visualPieces = tierScale.tiers.map((tier) => ({
    min: tier.min,
    max: tier.max,
    color: colors[tier.index],
  }));

  return {
    animation: false,
    animationDurationUpdate: 0,
    tooltip: {
      trigger: 'item',
      position: 'top',
      confine: false,
      appendTo: () => document.body,
      ...tooltipStyle(),
      formatter: (params: { data?: [number, number, number] }) => {
        const tuple = params.data;
        if (!tuple) return '';
        const [weekIndex, dayIndex] = tuple;
        const cell = heatmap.weeks[weekIndex]?.[dayIndex];
        if (!cell) return '';
        return [
          cell.date,
          `${labels.requests}: ${formatters.count(cell.requestCount)}`,
          `${labels.tokenSpend}: ${formatters.tokens(cell.tokenCount)}`,
          `${labels.cost}: ${formatters.cost(cell.costUsdMicros)}`,
        ].join('\n');
      },
    },
    grid: {
      left: 4,
      right: 4,
      top: 22,
      bottom: 48,
      containLabel: false,
    },
    xAxis: {
      type: 'category',
      position: 'top',
      data: Array.from({ length: weekCount }, (_, index) => String(index)),
      splitArea: { show: false },
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: {
        interval: 0,
        margin: 6,
        fontSize: 10,
        color: readCssVar('--fg-muted'),
        formatter: (_value: string, index: number) => {
          const monthDate = heatmap.monthLabels[index];
          return monthDate ? formatHeatmapMonth(monthDate, locale) : '';
        },
      },
    },
    yAxis: {
      type: 'category',
      data: Array.from({ length: 7 }, (_, index) => String(index)),
      show: false,
      splitArea: { show: false },
    },
    visualMap: {
      type: 'piecewise',
      min: HEATMAP_TIER_MIN_REQUESTS,
      max: visualMax,
      dimension: 2,
      outOfRange: {
        color: colors[0],
      },
      pieces: visualPieces,
      orient: 'horizontal',
      left: 'center',
      bottom: 0,
      itemWidth: 12,
      itemHeight: 12,
      itemGap: 2,
      hoverLink: true,
      showLabel: false,
      text: [labels.more, labels.less],
      textStyle: axisStyle(),
    },
    series: [
      {
        name: 'heatmap',
        type: 'heatmap',
        data: seriesData,
        label: { show: false },
        itemStyle: {
          borderColor: border,
          borderWidth: 1,
        },
        emphasis: {
          focus: 'none',
          itemStyle: {
            shadowBlur: 10,
            shadowColor: 'rgba(0, 0, 0, 0.5)',
          },
        },
        blur: {
          itemStyle: {
            opacity: 0.12,
          },
        },
      },
    ],
  };
}
