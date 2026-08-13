<script setup lang="ts">
import { computed } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { LineChart } from 'echarts/charts';
import { GridComponent, LegendComponent, TooltipComponent } from 'echarts/components';
import VChart from 'vue-echarts';
import { useI18n } from 'vue-i18n';
import type { DailyPoint } from '@/api/types';
import { buildTrendChartOption } from '@/lib/chart-theme';

use([CanvasRenderer, LineChart, GridComponent, LegendComponent, TooltipComponent]);

const props = defineProps<{
  daily: DailyPoint[];
}>();

const { t } = useI18n();

const chartOption = computed(() =>
  buildTrendChartOption(
    props.daily.map((point) => point.date),
    [
      {
        name: t('overview.requests'),
        data: props.daily.map((point) => point.request_count),
      },
      {
        name: t('overview.inputTokens'),
        data: props.daily.map((point) => point.input_tokens),
      },
      {
        name: t('overview.outputTokens'),
        data: props.daily.map((point) => point.output_tokens),
      },
      {
        name: t('overview.cost'),
        data: props.daily.map((point) => point.cost_usd_micros / 1_000_000),
        yAxisIndex: 1,
      },
    ],
  ),
);
</script>

<template>
  <VChart class="h-full w-full" :option="chartOption" autoresize />
</template>
