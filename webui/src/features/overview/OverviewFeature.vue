<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import PageHeader from '@/app/layout/PageHeader.vue';
import FilterField from '@/components/ui/FilterField.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import OverviewKpiGrid from '@/features/overview/OverviewKpiGrid.vue';
import OverviewShareList from '@/features/overview/OverviewShareList.vue';
import OverviewHeatmap from '@/features/overview/OverviewHeatmap.vue';
import OverviewLifetime from '@/features/overview/OverviewLifetime.vue';
import ChartPanelSkeleton from '@/features/overview/ChartPanelSkeleton.vue';
import { OverviewTrendChart } from '@/features/overview/overview-charts.async';
import { useOverviewStats } from '@/features/overview/useOverviewStats';

const { t } = useI18n();

const {
  days,
  summary,
  daily,
  byModel,
  byChannel,
  lifetime,
  lifetimeLoading,
  lifetimeError,
  statsErrorMessage,
  showSkeleton,
  showError,
  retryStats,
  retryLifetime,
} = useOverviewStats();

const dayOptions = computed(() =>
  ['1', '7', '30', '90'].map((value) => ({
    value,
    label: t(`overview.daysOption.${value}`),
  })),
);

const trendTitle = computed(() =>
  days.value === '1' ? t('overview.trendHourly') : t('overview.trend'),
);
</script>

<template>
  <div>
    <PageHeader :title="t('overview.title')">
      <template #actions>
        <FilterField
          :label="t('overview.days')"
          input-id="overview-days"
          inline
          class="min-w-[10rem]"
        >
          <UiSelect
            id="overview-days"
            v-model="days"
            data-testid="overview-days"
            :options="dayOptions"
          />
        </FilterField>
      </template>
    </PageHeader>

    <InlineError v-if="showError" :message="statsErrorMessage" @retry="retryStats" />

    <template v-else>
      <OverviewKpiGrid class="mb-6" :summary="summary ?? null" />

      <section class="mb-6">
        <div class="card">
          <div class="card-header">
            <h2 class="font-serif text-base font-semibold">{{ trendTitle }}</h2>
          </div>
          <div class="card-body overview-trend-body">
            <div data-testid="overview-trend-chart" class="h-chart">
              <ChartPanelSkeleton v-if="showSkeleton" />
              <Suspense v-else>
                <OverviewTrendChart class="h-full w-full" :daily="daily" />
                <template #fallback>
                  <ChartPanelSkeleton />
                </template>
              </Suspense>
            </div>
            <ul class="sr-only">
              <li
                v-for="point in daily"
                :key="point.date"
                data-testid="overview-daily-point"
                :data-date="point.date"
                :data-request-count="String(point.request_count)"
                :data-input-tokens="String(point.input_tokens)"
                :data-output-tokens="String(point.output_tokens)"
                :data-cost-usd-micros="String(point.cost_usd_micros)"
              >
                {{ point.date }} · {{ point.request_count }}
              </li>
            </ul>
          </div>
        </div>
      </section>

      <section class="overview-bottom-grid grid items-stretch gap-6 lg:grid-cols-2">
        <OverviewShareList
          :model-items="byModel"
          :channel-items="byChannel"
          :loading="showSkeleton"
        />
        <div class="overview-activity-stack" data-testid="overview-activity-stack">
          <OverviewHeatmap
            class="overview-activity-heatmap"
            :daily="daily"
            :loading="showSkeleton"
          />
          <OverviewLifetime
            class="overview-activity-lifetime"
            :lifetime="lifetime"
            :lifetime-loading="lifetimeLoading"
            :lifetime-error="lifetimeError"
            @retry-lifetime="retryLifetime"
          />
        </div>
      </section>
    </template>
  </div>
</template>
