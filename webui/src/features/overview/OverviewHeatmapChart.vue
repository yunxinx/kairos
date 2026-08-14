<script setup lang="ts">
import { computed, ref } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { HeatmapChart } from 'echarts/charts';
import { GridComponent, TooltipComponent, VisualMapComponent } from 'echarts/components';
import VChart from 'vue-echarts';
import { useI18n } from 'vue-i18n';
import { useChartThemeTick } from '@/composables/useChartThemeTick';
import { formatCount, formatTokensMillions, formatUsdMicros } from '@/lib/format';
import { buildHeatmapChartOption } from '@/lib/chart-theme';
import {
  buildHeatmapSeriesData,
  heatmapMaxRequestCount,
  type CalendarHeatmap,
} from '@/features/overview/heatmap';

use([CanvasRenderer, HeatmapChart, GridComponent, TooltipComponent, VisualMapComponent]);

const props = defineProps<{
  heatmap: CalendarHeatmap;
}>();

const { t, locale } = useI18n();
const themeTick = useChartThemeTick();

type ChartHandle = {
  dispatchAction: (payload: Record<string, unknown>) => void;
  setOption: (option: Record<string, unknown>, opts?: Record<string, unknown>) => void;
};

interface HighlightBatchItem {
  highlightKey?: string;
  seriesIndex?: number;
  dataIndex?: number | number[];
}

const chartRef = ref<ChartHandle | null>(null);

const seriesData = computed(() => buildHeatmapSeriesData(props.heatmap));
const maxCount = computed(() => heatmapMaxRequestCount(props.heatmap));

const chartOption = computed(() => {
  void themeTick.value;
  return buildHeatmapChartOption(
    props.heatmap,
    seriesData.value,
    maxCount.value,
    locale.value,
    {
      requests: t('overview.requests'),
      tokenSpend: t('overview.tokenSpend'),
      cost: t('overview.cost'),
      less: t('overview.heatmapLess'),
      more: t('overview.heatmapMore'),
    },
    {
      count: (value) => formatCount(value, locale.value),
      tokens: (value) => formatTokensMillions(value),
      cost: (value) => formatUsdMicros(value),
    },
  );
});

function isHighlightBatchItem(value: unknown): value is HighlightBatchItem {
  return typeof value === 'object' && value !== null;
}

function isVisualMapHighlight(batch: unknown): batch is HighlightBatchItem[] {
  if (!Array.isArray(batch) || batch.length === 0) return false;
  const [first] = batch as unknown[];
  if (!isHighlightBatchItem(first)) return false;
  const key = first.highlightKey;
  return typeof key === 'string' && key.startsWith('visualMap');
}

function enableVisualMapMask(batch: HighlightBatchItem[]): void {
  const chart = chartRef.value;
  if (!chart) return;
  chart.setOption(
    {
      series: [
        {
          emphasis: { focus: 'self' },
        },
      ],
    },
    { lazyUpdate: false },
  );
  chart.dispatchAction({ type: 'downplay', seriesIndex: 0 });
  chart.dispatchAction({ type: 'highlight', batch });
}

function disableVisualMapMask(): void {
  const chart = chartRef.value;
  if (!chart) return;
  chart.setOption(
    {
      series: [
        {
          emphasis: { focus: 'none' },
        },
      ],
    },
    { lazyUpdate: false },
  );
  chart.dispatchAction({ type: 'downplay', seriesIndex: 0 });
}

function onHighlight(params: unknown): void {
  const payload = params as { batch?: unknown };
  if (!isVisualMapHighlight(payload.batch)) return;
  enableVisualMapMask(payload.batch);
}

function onDownplay(params: unknown): void {
  const payload = params as { batch?: unknown };
  if (!isVisualMapHighlight(payload.batch)) return;
  disableVisualMapMask();
}

function onGlobalOut(): void {
  disableVisualMapMask();
}
</script>

<template>
  <VChart
    ref="chartRef"
    class="h-full w-full"
    :option="chartOption"
    :update-options="{ notMerge: true }"
    autoresize
    @highlight="onHighlight"
    @downplay="onDownplay"
    @globalout="onGlobalOut"
  />
</template>
