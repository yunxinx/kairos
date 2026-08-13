<script setup lang="ts">
import { computed } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { PieChart } from 'echarts/charts';
import { TooltipComponent } from 'echarts/components';
import VChart from 'vue-echarts';
import { buildPieChartOption } from '@/lib/chart-theme';

use([CanvasRenderer, PieChart, TooltipComponent]);

const props = defineProps<{
  slices: { name: string; value: number; requestCount: number }[];
  seriesName: string;
}>();

const chartOption = computed(() => buildPieChartOption(props.slices, props.seriesName));
</script>

<template>
  <VChart class="h-full w-full" :option="chartOption" autoresize />
</template>
