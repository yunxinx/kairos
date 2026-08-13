<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import PageHeader from '@/app/layout/PageHeader.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FilterField from '@/components/ui/FilterField.vue';
import InlineError from '@/components/ui/InlineError.vue';
import PageSkeleton from '@/components/ui/PageSkeleton.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { OverviewShareChart, OverviewTrendChart } from '@/features/overview/overview-charts.async';
import { formatUsdMicros } from '@/lib/format';

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

const summary = computed(() => statsQuery.data.value?.summary);
const daily = computed(() => statsQuery.data.value?.daily ?? []);
const byModel = computed(() => statsQuery.data.value?.by_model ?? []);
const byChannel = computed(() => statsQuery.data.value?.by_channel ?? []);

const modelSlices = computed(() =>
  byModel.value.map((share) => ({
    name: share.model,
    value: share.cost_usd_micros,
    requestCount: share.request_count,
  })),
);

const channelSlices = computed(() =>
  byChannel.value.map((share) => ({
    name: share.channel,
    value: share.cost_usd_micros,
    requestCount: share.request_count,
  })),
);
</script>

<template>
  <div>
    <PageHeader :title="t('overview.title')" :subtitle="t('overview.subtitle')">
      <template #actions>
        <FilterField :label="t('overview.days')" input-id="overview-days" class="min-w-[10rem]">
          <UiSelect
            id="overview-days"
            v-model="days"
            data-testid="overview-days"
            :options="dayOptions"
          />
        </FilterField>
      </template>
    </PageHeader>

    <PageSkeleton v-if="statsQuery.isPending.value" :stat-cards="7" with-chart />

    <InlineError
      v-else-if="statsQuery.isError.value"
      :message="extractApiError(statsQuery.error.value).message"
      @retry="() => statsQuery.refetch()"
    />

    <template v-else>
      <section class="mb-6">
        <div class="grid grid-cols-2 gap-3 lg:grid-cols-4">
          <div class="card">
            <div class="card-body">
              <div class="text-fg-muted text-xs font-medium">{{ t('overview.requests') }}</div>
              <div class="mt-2 font-mono text-2xl font-bold" data-testid="overview-request-count">
                {{ summary?.request_count ?? 0 }}
              </div>
            </div>
          </div>
          <div class="card">
            <div class="card-body">
              <div class="text-fg-muted text-xs font-medium">{{ t('overview.success') }}</div>
              <div class="mt-2 font-mono text-2xl font-bold" data-testid="overview-success-count">
                {{ summary?.success_count ?? 0 }}
              </div>
            </div>
          </div>
          <div class="card">
            <div class="card-body">
              <div class="text-fg-muted text-xs font-medium">{{ t('overview.inputTokens') }}</div>
              <div class="mt-2 font-mono text-2xl font-bold" data-testid="overview-input-tokens">
                {{ summary?.input_tokens ?? 0 }}
              </div>
            </div>
          </div>
          <div class="card">
            <div class="card-body">
              <div class="text-fg-muted text-xs font-medium">{{ t('overview.outputTokens') }}</div>
              <div class="mt-2 font-mono text-2xl font-bold" data-testid="overview-output-tokens">
                {{ summary?.output_tokens ?? 0 }}
              </div>
            </div>
          </div>
          <div class="card">
            <div class="card-body">
              <div class="text-fg-muted text-xs font-medium">{{ t('overview.cost') }}</div>
              <div
                class="text-primary mt-2 font-mono text-2xl font-bold"
                data-testid="overview-cost"
              >
                {{ formatUsdMicros(summary?.cost_usd_micros ?? 0) }}
              </div>
            </div>
          </div>
          <div class="card">
            <div class="card-body">
              <div class="text-fg-muted text-xs font-medium">{{ t('overview.tokenCount') }}</div>
              <div class="mt-2 font-mono text-2xl font-bold" data-testid="overview-token-count">
                {{ summary?.token_count ?? 0 }}
              </div>
            </div>
          </div>
          <div class="card">
            <div class="card-body">
              <div class="text-fg-muted text-xs font-medium">{{ t('overview.channelCount') }}</div>
              <div class="mt-2 font-mono text-2xl font-bold" data-testid="overview-channel-count">
                {{ summary?.channel_count ?? 0 }}
              </div>
            </div>
          </div>
        </div>
      </section>

      <section class="mb-6">
        <div class="card">
          <div class="card-header">
            <h2 class="font-serif text-base font-semibold">{{ t('overview.trend') }}</h2>
          </div>
          <div class="card-body">
            <div data-testid="overview-trend-chart" class="h-chart">
              <OverviewTrendChart class="h-full w-full" :daily="daily" />
            </div>
            <ul class="mt-4 flex flex-wrap gap-2">
              <li
                v-for="point in daily"
                :key="point.date"
                data-testid="overview-daily-point"
                :data-date="point.date"
                :data-request-count="String(point.request_count)"
                :data-input-tokens="String(point.input_tokens)"
                :data-output-tokens="String(point.output_tokens)"
                :data-cost-usd-micros="String(point.cost_usd_micros)"
                class="text-fg-muted font-mono text-xs"
              >
                {{ point.date }} · {{ point.request_count }}
              </li>
            </ul>
          </div>
        </div>
      </section>

      <section class="grid gap-6 lg:grid-cols-2">
        <div class="card">
          <div class="card-header">
            <h2 class="font-serif text-base font-semibold">{{ t('overview.byModel') }}</h2>
          </div>
          <div class="card-body">
            <template v-if="byModel.length > 0">
              <div data-testid="overview-model-chart" class="h-52">
                <OverviewShareChart
                  class="h-full w-full"
                  :slices="modelSlices"
                  :series-name="t('overview.cost')"
                />
              </div>
              <ul class="mt-4 space-y-2">
                <li
                  v-for="share in byModel"
                  :key="share.model"
                  data-testid="overview-model-share"
                  :data-model="share.model"
                  :data-request-count="String(share.request_count)"
                  :data-cost-usd-micros="String(share.cost_usd_micros)"
                  class="flex items-center justify-between gap-3 text-sm"
                >
                  <span class="min-w-0 truncate font-medium">{{ share.model }}</span>
                  <span class="text-fg-muted shrink-0 font-mono text-xs">
                    {{ share.request_count }} · {{ formatUsdMicros(share.cost_usd_micros) }}
                  </span>
                </li>
              </ul>
            </template>
            <EmptyState v-else :title="t('common.emptyList')" />
          </div>
        </div>

        <div class="card">
          <div class="card-header">
            <h2 class="font-serif text-base font-semibold">{{ t('overview.byChannel') }}</h2>
          </div>
          <div class="card-body">
            <template v-if="byChannel.length > 0">
              <div data-testid="overview-channel-chart" class="h-52">
                <OverviewShareChart
                  class="h-full w-full"
                  :slices="channelSlices"
                  :series-name="t('overview.cost')"
                />
              </div>
              <ul class="mt-4 space-y-2">
                <li
                  v-for="share in byChannel"
                  :key="share.channel"
                  data-testid="overview-channel-share"
                  :data-channel="share.channel"
                  :data-request-count="String(share.request_count)"
                  :data-cost-usd-micros="String(share.cost_usd_micros)"
                  class="flex items-center justify-between gap-3 text-sm"
                >
                  <span class="min-w-0 truncate font-medium">{{ share.channel }}</span>
                  <span class="text-fg-muted shrink-0 font-mono text-xs">
                    {{ share.request_count }} · {{ formatUsdMicros(share.cost_usd_micros) }}
                  </span>
                </li>
              </ul>
            </template>
            <EmptyState v-else :title="t('common.emptyList')" />
          </div>
        </div>
      </section>
    </template>
  </div>
</template>
