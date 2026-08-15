import {
  buildHeatmapTierScale,
  formatHeatmapMonth,
  heatmapSeriesPoint,
  HEATMAP_CHART_GRID,
  HEATMAP_TIER_MIN_REQUESTS,
  type CalendarHeatmap,
  type HeatmapSeriesDatum,
} from '@/features/overview/heatmap';

function readCssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
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
  const primary = readCssVar('--seed-primary');
  const surface = readCssVar('--seed-surface');
  const surfaceAlt = readCssVar('--seed-surface-alt');
  return [
    surfaceAlt,
    mixHex(surface, primary, 0.2),
    mixHex(surface, primary, 0.4),
    mixHex(surface, primary, 0.6),
    mixHex(surface, primary, 0.8),
    primary,
  ];
}

function trendSeriesColors(): [string, string] {
  return [readCssVar('--seed-primary'), readCssVar('--blue-light')];
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

/** ECharts HTML 浮窗默认 `white-space: nowrap`，必须用 `<br/>` 换行。 */
function joinTooltipLines(lines: string[]): string {
  return lines.join('<br/>');
}

function escapeTooltipText(text: string): string {
  return text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
}

/** 浮窗日期做成与页面一致的标记/徽章，而不是标题下划线。 */
function tooltipTitleHtml(title: string): string {
  return `<div class="overview-chart-tooltip-title"><span class="badge badge-neutral font-mono">${escapeTooltipText(title)}</span></div>`;
}

interface HeatmapTooltipRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface HeatmapTooltipSize {
  contentSize: [number, number];
  viewSize: [number, number];
}

/**
 * 热力格上方优先，不够再下方，再贴格子左右。最后才相对指针偏移。
 * 避开指针的那条轴不 clamp 回视口，避免把浮窗压到指针上。
 */
function heatmapTooltipPosition(
  point: [number, number],
  _params: unknown,
  _el: unknown,
  rect: HeatmapTooltipRect | null,
  size: HeatmapTooltipSize,
): [number, number] {
  const gap = 8;
  const [tipWidth, tipHeight] = size.contentSize;
  const [viewWidth, viewHeight] = size.viewSize;
  const [mouseX, mouseY] = point;
  const maxX = Math.max(0, viewWidth - tipWidth);
  const maxY = Math.max(0, viewHeight - tipHeight);
  const clampX = (x: number): number => Math.min(Math.max(x, 0), maxX);
  const clampY = (y: number): number => Math.min(Math.max(y, 0), maxY);

  if (rect) {
    const centeredX = clampX(rect.x + rect.width / 2 - tipWidth / 2);
    const above = rect.y - tipHeight - gap;
    if (above >= 0) return [centeredX, above];
    const below = rect.y + rect.height + gap;
    if (below + tipHeight <= viewHeight) return [centeredX, below];

    const yBesideCell = clampY(rect.y + rect.height / 2 - tipHeight / 2);
    const right = rect.x + rect.width + gap;
    if (right + tipWidth <= viewWidth) return [right, yBesideCell];
    const left = rect.x - tipWidth - gap;
    if (left >= 0) return [left, yBesideCell];
    const roomRight = viewWidth - (rect.x + rect.width);
    return [roomRight >= rect.x ? right : left, yBesideCell];
  }

  const right = mouseX + gap;
  const left = mouseX - tipWidth - gap;
  const x = right + tipWidth <= viewWidth || mouseX < viewWidth / 2 ? right : left;
  const abovePointer = mouseY - tipHeight - gap;
  const y = abovePointer >= 0 ? abovePointer : mouseY + gap;
  return [x, y];
}

interface AxisPointerLabelParams {
  value?: unknown;
}

function formatAxisPointerLabel(
  params: AxisPointerLabelParams,
  formatLabel: (value: number) => string,
): string {
  const raw = params.value;
  const value = typeof raw === 'number' ? raw : Number(raw);
  if (!Number.isFinite(value)) return '';
  return formatLabel(value);
}

/** UTC 小时标签 `YYYY-MM-DDTHH:00:00Z` → `HH:00`；日历日原样。 */
export function formatTrendLabel(date: string): string {
  const hour = /^(\d{4}-\d{2}-\d{2})T(\d{2}):/.exec(date);
  if (hour) {
    return `${hour[2]}:00`;
  }
  return date;
}

export interface TrendChartSeries {
  name: string;
  type: 'line' | 'bar';
  data: number[];
  yAxisIndex?: number;
  formatValue: (value: number) => string;
}

export interface TrendChartAxis {
  name: string;
  formatLabel: (value: number) => string;
}

interface TrendTooltipPoint {
  seriesName?: string;
  value?: number;
  axisValue?: string;
  dataIndex?: number;
  marker?: string;
}

export interface TrendVisibleAxes {
  left: boolean;
  right: boolean;
}

export interface TrendChartInput {
  labels: string[];
  series: TrendChartSeries[];
  axes: { left: TrendChartAxis; right: TrendChartAxis };
  extraTooltipLines?: (dataIndex: number) => string[];
  visibleAxes?: TrendVisibleAxes;
}

function valueAxis(
  axis: TrendChartAxis,
  options: {
    show: boolean;
    minInterval?: number;
    splitLine: boolean;
    border: string;
  },
): Record<string, unknown> {
  return {
    type: 'value',
    show: options.show,
    name: options.show ? axis.name : '',
    min: 0,
    ...(options.minInterval === undefined ? {} : { minInterval: options.minInterval }),
    alignTicks: true,
    nameGap: 8,
    nameTextStyle: axisStyle(),
    axisLine: { show: false },
    splitLine: options.splitLine
      ? { show: true, lineStyle: { color: options.border } }
      : { show: false },
    axisLabel: {
      ...axisStyle(),
      show: options.show,
      formatter: (value: number) => axis.formatLabel(value),
    },
    axisPointer: {
      show: options.show,
      label: {
        formatter: (params: AxisPointerLabelParams) =>
          formatAxisPointerLabel(params, axis.formatLabel),
      },
    },
  };
}

/** 趋势：请求量折线走左轴，费用柱走右轴。浮窗颜色跟当前主题。 */
export function buildTrendChartOption(input: TrendChartInput): Record<string, unknown> {
  const visibleAxes = input.visibleAxes ?? { left: true, right: true };
  const colors = trendSeriesColors();
  const border = readCssVar('--seed-border');
  const subtle = readCssVar('--fg-subtle');
  const showLineSymbol = input.labels.length <= 14;
  const leftName = input.series.find((item) => (item.yAxisIndex ?? 0) === 0)?.name;
  const rightName = input.series.find((item) => item.yAxisIndex === 1)?.name;

  return {
    color: colors,
    animationDurationUpdate: 200,
    grid: { left: 4, right: 4, top: 52, bottom: 12, containLabel: true },
    legend: {
      top: 0,
      textStyle: axisStyle(),
      selected: {
        ...(leftName ? { [leftName]: visibleAxes.left } : {}),
        ...(rightName ? { [rightName]: visibleAxes.right } : {}),
      },
    },
    tooltip: {
      trigger: 'axis',
      ...tooltipStyle(),
      axisPointer: {
        type: 'cross',
        lineStyle: { color: border },
        crossStyle: { color: subtle },
      },
      formatter: (params: TrendTooltipPoint | TrendTooltipPoint[]) => {
        const points = Array.isArray(params) ? params : [params];
        const title = points[0]?.axisValue;
        if (typeof title !== 'string') return '';
        const dataIndex = points[0]?.dataIndex;
        const extras =
          typeof dataIndex === 'number' ? (input.extraTooltipLines?.(dataIndex) ?? []) : [];
        const rows = points.flatMap((point) => {
          const seriesItem = input.series.find((item) => item.name === point.seriesName);
          if (!seriesItem || typeof point.value !== 'number') return [];
          const marker = typeof point.marker === 'string' ? `${point.marker} ` : '';
          return [
            `${marker}${escapeTooltipText(`${seriesItem.name}: ${seriesItem.formatValue(point.value)}`)}`,
          ];
        });
        const extraRows = extras.map((line) => escapeTooltipText(line));
        return `${tooltipTitleHtml(title)}${joinTooltipLines([...rows, ...extraRows])}`;
      },
    },
    xAxis: [
      {
        type: 'category',
        data: input.labels,
        axisTick: { alignWithLabel: true },
        axisPointer: { type: 'shadow' },
        axisLine: { lineStyle: { color: border } },
        axisLabel: { ...axisStyle(), hideOverlap: true },
      },
    ],
    yAxis: [
      valueAxis(input.axes.left, {
        show: visibleAxes.left,
        minInterval: 1,
        splitLine: visibleAxes.left,
        border,
      }),
      valueAxis(input.axes.right, {
        show: visibleAxes.right,
        splitLine: !visibleAxes.left && visibleAxes.right,
        border,
      }),
    ],
    series: input.series.map((item, index) => {
      const yAxisIndex = item.yAxisIndex ?? 0;
      if (item.type === 'bar') {
        return {
          name: item.name,
          type: 'bar',
          yAxisIndex,
          barMaxWidth: 36,
          itemStyle: { color: colors[index] ?? colors[1] },
          data: item.data,
        };
      }
      return {
        name: item.name,
        type: 'line',
        yAxisIndex,
        z: 3,
        smooth: true,
        showSymbol: showLineSymbol,
        data: item.data,
      };
    }),
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
  seriesData: HeatmapSeriesDatum[],
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
      // 默认还包含 click：点底部色条会走 _tryShow，随后 setOption 刷新时 _keepShow 会把浮窗钉住。
      triggerOn: 'mousemove',
      hideDelay: 0,
      position: heatmapTooltipPosition,
      className: 'overview-heatmap-tooltip',
      ...tooltipStyle(),
      formatter: (params: { data?: unknown; value?: unknown }) => {
        const tuple = heatmapSeriesPoint(params.value ?? params.data);
        if (!tuple) return '';
        const [weekIndex, dayIndex, requestCount] = tuple;
        if (requestCount <= 0) return '';
        const cell = heatmap.weeks[weekIndex]?.[dayIndex];
        if (!cell || cell.requestCount <= 0) return '';
        return `${tooltipTitleHtml(cell.date)}${joinTooltipLines([
          `${labels.requests}: ${formatters.count(cell.requestCount)}`,
          `${labels.tokenSpend}: ${formatters.tokens(cell.tokenCount)}`,
          `${labels.cost}: ${formatters.cost(cell.costUsdMicros)}`,
        ])}`;
      },
    },
    grid: {
      ...HEATMAP_CHART_GRID,
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
          // 与官方 heatmap 示例一致：只给当前格加阴影，不要 focus/blur 遮罩其余格子。
          focus: 'none',
          itemStyle: {
            shadowBlur: 10,
            shadowColor: 'rgba(0, 0, 0, 0.5)',
          },
        },
      },
    ],
  };
}
