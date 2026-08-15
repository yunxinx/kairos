<script setup lang="ts">
import { computed, unref, useTemplateRef } from 'vue';
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

interface VisualMapEl {
  type: string;
  silent: boolean;
  cursor?: string;
  onclick?: ((event: unknown) => void) | null;
}

type ChartHandle = {
  dispatchAction: (payload: Record<string, unknown>) => void;
  chart?: {
    getModel: () => { getComponent: (mainType: string) => unknown };
    getViewOfComponentModel: (
      model: unknown,
    ) => { group: { traverse: (callback: (el: VisualMapEl) => void) => void } } | undefined;
  };
};

const chartRef = useTemplateRef<ChartHandle>('chartRef');

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

function echartsInstance(): NonNullable<ChartHandle['chart']> | undefined {
  return unref(chartRef.value?.chart);
}

function hideHeatmapTip(): void {
  chartRef.value?.dispatchAction({ type: 'hideTip' });
}

/**
 * piecewise 两端「少/多」默认 cursor:pointer；色块 onclick 会选档。
 * 文字保持标签，色块只走原生 hoverLink。
 */
function tuneVisualMapPointer(): void {
  const instance = echartsInstance();
  if (!instance) return;
  const model = instance.getModel().getComponent('visualMap');
  if (!model) return;
  const view = instance.getViewOfComponentModel(model);
  view?.group.traverse((el) => {
    if (el.type === 'text') {
      el.silent = true;
      el.cursor = 'default';
    }
    el.onclick = null;
  });
}
</script>

<template>
  <VChart
    ref="chartRef"
    class="h-full w-full"
    :option="chartOption"
    :update-options="{ notMerge: true }"
    autoresize
    @globalout="hideHeatmapTip"
    @native:mouseleave="hideHeatmapTip"
    @finished="tuneVisualMapPointer"
  />
</template>
