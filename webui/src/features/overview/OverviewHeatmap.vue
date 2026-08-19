<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { DailyPoint } from '@/api/types';
import ChartPanelSkeleton from '@/features/overview/ChartPanelSkeleton.vue';
import { formatTokensMillions, formatUsdMicros } from '@/lib/format';
import { OverviewHeatmapChart } from '@/features/overview/overview-charts.async';
import { buildHeatmap, flattenHeatmapCells, HEATMAP_WEEK_COUNT } from '@/features/overview/heatmap';

const props = withDefaults(
  defineProps<{
    daily: DailyPoint[];
    loading?: boolean;
  }>(),
  {
    loading: false,
  },
);

const { t } = useI18n();

const heatmap = computed(() => buildHeatmap(props.daily));
const heatmapCells = computed(() => flattenHeatmapCells(heatmap.value));

function cellAriaLabel(cell: (typeof heatmapCells.value)[number]): string {
  return t('overview.heatmapCell', {
    date: cell.date,
    count: cell.requestCount,
    tokens: formatTokensMillions(cell.tokenCount),
    cost: formatUsdMicros(cell.costUsdMicros),
  });
}
</script>

<template>
  <div class="card overview-panel overview-heatmap-card">
    <div class="card-header">
      <h2 class="text-base font-semibold">{{ t('overview.heatmap') }}</h2>
    </div>
    <div class="card-body overview-panel-body overview-heatmap-body">
      <div
        data-testid="overview-heatmap"
        data-heatmap-kind="calendar"
        class="overview-heatmap-chart"
      >
        <ChartPanelSkeleton v-if="loading" />
        <Suspense v-else>
          <OverviewHeatmapChart class="h-full w-full" :heatmap="heatmap" />
          <template #fallback>
            <ChartPanelSkeleton />
          </template>
        </Suspense>
      </div>

      <ul class="sr-only">
        <template v-if="loading">
          <li
            v-for="index in HEATMAP_WEEK_COUNT * 7"
            :key="'sk-cell-' + index"
            data-testid="overview-heatmap-cell"
          />
        </template>
        <template v-else>
          <li
            v-for="cell in heatmapCells"
            :key="cell.date"
            data-testid="overview-heatmap-cell"
            :data-date="cell.date"
            :data-request-count="String(cell.requestCount)"
            :data-token-count="String(cell.tokenCount)"
            :data-cost-usd-micros="String(cell.costUsdMicros)"
            :aria-label="cellAriaLabel(cell)"
          >
            {{ cell.date }} · {{ cell.requestCount }}
          </li>
        </template>
      </ul>
    </div>
  </div>
</template>
