<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import PageHeader from '@/app/layout/PageHeader.vue';
import FilterField from '@/components/ui/FilterField.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import OverviewKpiGrid from '@/features/overview/OverviewKpiGrid.vue';
import OverviewShareList from '@/features/overview/OverviewShareList.vue';
import OverviewHeatmap from '@/features/overview/OverviewHeatmap.vue';
import ChartPanelSkeleton from '@/features/overview/ChartPanelSkeleton.vue';
import { OverviewTrendChart } from '@/features/overview/overview-charts.async';

const { t } = useI18n();

const days = ref('7');

const dayOptions = computed(() =>
  ['1', '7', '30', '90'].map((value) => ({
    value,
    label: t(`overview.daysOption.${value}`),
  })),
);

const statsQuery = useQuery({
  queryKey: ['stats', days],
  queryFn: () => apiClient.getStats(Number(days.value)),
});

const lifetimeQuery = useQuery({
  queryKey: ['stats', 'lifetime'],
  queryFn: () => apiClient.getLifetimeStats(),
});

const summary = computed(() => statsQuery.data.value?.summary);
const daily = computed(() => statsQuery.data.value?.daily ?? []);
const byModel = computed(() =>
  (statsQuery.data.value?.by_model ?? []).map((share) => ({
    name: share.model,
    requestCount: share.request_count,
    costUsdMicros: share.cost_usd_micros,
  })),
);
const byChannel = computed(() =>
  (statsQuery.data.value?.by_channel ?? []).map((share) => ({
    name: share.channel,
    requestCount: share.request_count,
    costUsdMicros: share.cost_usd_micros,
  })),
);

const trendTitle = computed(() =>
  days.value === '1' ? t('overview.trendHourly') : t('overview.trend'),
);

const lifetime = computed(() => lifetimeQuery.data.value ?? null);
const lifetimeLoading = computed(() => lifetimeQuery.isPending.value && !lifetimeQuery.data.value);
const lifetimeError = computed(() => {
  if (!lifetimeQuery.isError.value || lifetimeQuery.data.value) return '';
  return extractApiError(lifetimeQuery.error.value).message;
});
const showSkeleton = computed(() => statsQuery.isPending.value && !statsQuery.data.value);
const showError = computed(() => statsQuery.isError.value && !statsQuery.data.value);
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

    <InlineError
      v-if="showError"
      :message="extractApiError(statsQuery.error.value).message"
      @retry="() => statsQuery.refetch()"
    />

    <template v-else>
      <OverviewKpiGrid class="mb-6" :summary="summary ?? null" />

      <section class="mb-6">
        <div class="card">
          <div class="card-header">
            <h2 class="font-serif text-base font-semibold">{{ trendTitle }}</h2>
          </div>
          <div class="card-body">
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

      <section class="grid items-stretch gap-6 lg:grid-cols-2">
        <OverviewShareList
          :model-items="byModel"
          :channel-items="byChannel"
          :loading="showSkeleton"
        />
        <OverviewHeatmap
          :daily="daily"
          :lifetime="lifetime"
          :lifetime-loading="lifetimeLoading"
          :lifetime-error="lifetimeError"
          :loading="showSkeleton"
          @retry-lifetime="() => lifetimeQuery.refetch()"
        />
      </section>
    </template>
  </div>
</template>
