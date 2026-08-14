<script setup lang="ts">
import { computed } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { LineChart } from 'echarts/charts';
import { GridComponent, LegendComponent, TooltipComponent } from 'echarts/components';
import VChart from 'vue-echarts';
import { useI18n } from 'vue-i18n';
import type { DailyPoint } from '@/api/types';
import { useChartThemeTick } from '@/composables/useChartThemeTick';
import { buildTrendChartOption, formatTrendLabel } from '@/lib/chart-theme';

use([CanvasRenderer, LineChart, GridComponent, LegendComponent, TooltipComponent]);

const props = defineProps<{
  daily: DailyPoint[];
}>();

const { t } = useI18n();
const themeTick = useChartThemeTick();

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
