import { formatUsdMicros } from '@/lib/format';

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

function pieTooltipText(raw: unknown): string {
  if (typeof raw !== 'object' || raw === null) {
    return '';
  }
  const name = 'name' in raw && typeof raw.name === 'string' ? raw.name : '';
  const value = 'value' in raw && typeof raw.value === 'number' ? raw.value : 0;
  const data = 'data' in raw && typeof raw.data === 'object' && raw.data !== null ? raw.data : {};
  const requestCount =
    'requestCount' in data && typeof data.requestCount === 'number' ? data.requestCount : 0;
  return `${name}<br/>${formatUsdMicros(value)} · ${requestCount}`;
}

/** 逐日趋势：请求量 / token 走左轴，费用（美元）走右轴。 */
export function buildTrendChartOption(
  labels: string[],
  series: { name: string; data: number[]; yAxisIndex?: number }[],
): Record<string, unknown> {
  const colors = buildChartColors();
  const border = readCssVar('--seed-border');
  return {
    color: colors,
    grid: { left: 48, right: 56, top: 36, bottom: 32 },
    legend: {
      top: 0,
      textStyle: axisStyle(),
    },
    tooltip: { trigger: 'axis' },
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

/** 分布饼图：value 为费用（micro-USD），tooltip 展示美元与请求数。 */
export function buildPieChartOption(
  slices: { name: string; value: number; requestCount: number }[],
  seriesName: string,
): Record<string, unknown> {
  const colors = buildChartColors();
  return {
    color: colors,
    tooltip: { trigger: 'item', formatter: pieTooltipText },
    series: [
      {
        name: seriesName,
        type: 'pie',
        radius: ['36%', '62%'],
        avoidLabelOverlap: true,
        label: { color: readCssVar('--fg-muted') },
        data: slices,
      },
    ],
  };
}
