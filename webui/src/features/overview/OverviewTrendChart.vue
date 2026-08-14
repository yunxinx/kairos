<script setup lang="ts">
import { computed, shallowRef } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { BarChart, LineChart } from 'echarts/charts';
import {
  AxisPointerComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
} from 'echarts/components';
import VChart from 'vue-echarts';
import { useI18n } from 'vue-i18n';
import type { DailyPoint } from '@/api/types';
import { useChartThemeTick } from '@/composables/useChartThemeTick';
import { buildTrendChartOption, formatTrendLabel } from '@/lib/chart-theme';
import { formatCount, formatTokensMillions, formatUsdMicros } from '@/lib/format';

use([
  CanvasRenderer,
  BarChart,
  LineChart,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  AxisPointerComponent,
]);

const props = defineProps<{
  daily: DailyPoint[];
}>();

const { t, locale } = useI18n();
const themeTick = useChartThemeTick();
const leftAxisVisible = shallowRef(true);
const rightAxisVisible = shallowRef(true);

const MICROS_PER_USD = 1_000_000;

function formatAxisCount(value: number): string {
  return formatCount(Math.round(value), locale.value);
}

function formatAxisUsd(value: number): string {
  return formatUsdMicros(Math.round(value * MICROS_PER_USD));
}

function tokenTooltipLine(point: DailyPoint): string {
  const total = point.input_tokens + point.output_tokens;
  return `${t('overview.tokenSpend')}: ${formatTokensMillions(total)} (${formatTokensMillions(point.input_tokens)} ${t('overview.inputShort')} · ${formatTokensMillions(point.output_tokens)} ${t('overview.outputShort')})`;
}

const chartOption = computed(() => {
  void themeTick.value;
  const requestsName = t('overview.requests');
  const costName = t('overview.cost');
  return buildTrendChartOption({
    labels: props.daily.map((point) => formatTrendLabel(point.date)),
    series: [
      {
        name: requestsName,
        type: 'line',
        data: props.daily.map((point) => point.request_count),
        formatValue: formatAxisCount,
      },
      {
        name: costName,
        type: 'bar',
        yAxisIndex: 1,
        data: props.daily.map((point) => point.cost_usd_micros / MICROS_PER_USD),
        formatValue: formatAxisUsd,
      },
    ],
    axes: {
      left: { name: requestsName, formatLabel: formatAxisCount },
      right: { name: costName, formatLabel: formatAxisUsd },
    },
    extraTooltipLines: (dataIndex) => {
      const point = props.daily[dataIndex];
      return point ? [tokenTooltipLine(point)] : [];
    },
    visibleAxes: { left: leftAxisVisible.value, right: rightAxisVisible.value },
  });
});

function onLegendSelectChanged(params: unknown): void {
  if (typeof params !== 'object' || params === null || !('selected' in params)) return;
  const selected = params.selected;
  if (typeof selected !== 'object' || selected === null) return;
  const map = selected as Record<string, unknown>;
  leftAxisVisible.value = map[t('overview.requests')] !== false;
  rightAxisVisible.value = map[t('overview.cost')] !== false;
}
</script>

<template>
  <VChart
    class="h-full w-full"
    :option="chartOption"
    :update-options="{ notMerge: true }"
    autoresize
    @legendselectchanged="onLegendSelectChanged"
  />
</template>
