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
