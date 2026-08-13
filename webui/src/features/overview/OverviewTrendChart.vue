<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { LineChart } from 'echarts/charts';
import { GridComponent, LegendComponent, TooltipComponent } from 'echarts/components';
import VChart from 'vue-echarts';
import { useI18n } from 'vue-i18n';
import type { DailyPoint } from '@/api/types';
import { buildTrendChartOption, formatTrendLabel } from '@/lib/chart-theme';

use([CanvasRenderer, LineChart, GridComponent, LegendComponent, TooltipComponent]);

const props = defineProps<{
  daily: DailyPoint[];
}>();

const { t } = useI18n();

/** html.dark 切换时重读 CSS 变量，让浮窗/轴线跟主题。 */
const themeTick = ref(0);
let themeObserver: MutationObserver | undefined;

onMounted(() => {
  themeObserver = new MutationObserver(() => {
    themeTick.value += 1;
  });
  themeObserver.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['class'],
  });
});

onUnmounted(() => {
  themeObserver?.disconnect();
});

const chartOption = computed(() => {
  void themeTick.value;
  return buildTrendChartOption(
    props.daily.map((point) => formatTrendLabel(point.date)),
    [
      {
        name: t('overview.requests'),
        data: props.daily.map((point) => point.request_count),
      },
      {
        name: t('overview.cost'),
        data: props.daily.map((point) => point.cost_usd_micros / 1_000_000),
        yAxisIndex: 1,
      },
    ],
  );
});
</script>

<template>
  <VChart
    class="h-full w-full"
    :option="chartOption"
    :update-options="{ notMerge: true }"
    autoresize
  />
</template>
