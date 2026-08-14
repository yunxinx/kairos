import { defineAsyncComponent, type Component } from 'vue';

/** 懒加载概览图表，避免 echarts 进入主包。 */
export const OverviewTrendChart = defineAsyncComponent(async (): Promise<Component> => {
  const module = (await import('./OverviewTrendChart.vue')) as { default: Component };
  return module.default;
});

export const OverviewHeatmapChart = defineAsyncComponent(async (): Promise<Component> => {
  const module = (await import('./OverviewHeatmapChart.vue')) as { default: Component };
  return module.default;
});
