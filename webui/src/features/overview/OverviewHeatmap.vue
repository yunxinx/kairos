<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import type { DailyPoint, LifetimeStats } from '@/api/types';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import { formatCount, formatTokensMillions, formatUsdMicros } from '@/lib/format';
import {
  buildHeatmap,
  formatHeatmapMonth,
  formatHeatmapWeekday,
  HEATMAP_WEEK_COUNT,
  type HeatmapCell,
} from '@/features/overview/heatmap';

const props = withDefaults(
  defineProps<{
    daily: DailyPoint[];
    lifetime?: LifetimeStats | null;
    lifetimeLoading?: boolean;
    lifetimeError?: string;
    loading?: boolean;
  }>(),
  {
    lifetime: null,
    lifetimeLoading: false,
    lifetimeError: '',
    loading: false,
  },
);

const emit = defineEmits<{
  retryLifetime: [];
}>();

const { t, locale } = useI18n();

const heatmap = computed(() => buildHeatmap(props.daily));

const weekdayLabels = computed(() =>
  [0, 1, 2, 3, 4, 5, 6].map((weekday) => ({
    weekday,
    label: weekday % 2 === 1 ? formatHeatmapWeekday(weekday, locale.value) : '',
  })),
);

const hovered = ref<HeatmapCell | null>(null);
const tipX = ref(0);
const tipY = ref(0);

const cellsByDate = computed(() => {
  const map = new Map<string, HeatmapCell>();
  for (const week of heatmap.value.weeks) {
    for (const cell of week) {
      map.set(cell.date, cell);
    }
  }
  return map;
});

const hoveredDateLabel = computed(() => hovered.value?.date ?? '');

function cellAriaLabel(cell: HeatmapCell): string {
  return t('overview.heatmapCell', {
    date: cell.date,
    count: cell.requestCount,
    tokens: formatTokensMillions(cell.tokenCount),
    cost: formatUsdMicros(cell.costUsdMicros),
  });
}

function monthCaption(date: string | null): string {
  if (!date) return '';
  return formatHeatmapMonth(date, locale.value);
}

function placeTip(event: PointerEvent): void {
  const offset = 12;
  const tipWidth = 176;
  const tipHeight = 110;
  let x = event.clientX + offset;
  let y = event.clientY + offset;
  if (x + tipWidth > window.innerWidth) {
    x = event.clientX - tipWidth - 8;
  }
  if (y + tipHeight > window.innerHeight) {
    y = event.clientY - tipHeight - 8;
  }
  tipX.value = Math.max(8, x);
  tipY.value = Math.max(8, y);
}

function handleBoardPointer(event: PointerEvent): void {
  const target = event.target;
  if (!(target instanceof HTMLElement) || !target.dataset.date) {
    hovered.value = null;
    return;
  }
  const cell = cellsByDate.value.get(target.dataset.date);
  if (!cell) {
    hovered.value = null;
    return;
  }
  hovered.value = cell;
  placeTip(event);
}

function clearTip(): void {
  hovered.value = null;
}
</script>

<template>
  <div class="card overview-panel">
    <div class="card-header">
      <h2 class="font-serif text-base font-semibold">{{ t('overview.heatmap') }}</h2>
    </div>
    <div class="card-body overview-panel-body overview-heatmap-body">
      <div class="heatmap-upper">
        <div
          v-if="loading"
          class="heatmap"
          role="status"
          :aria-label="t('common.loading')"
          :style="{ '--heatmap-weeks': HEATMAP_WEEK_COUNT }"
        >
          <div class="heatmap-calendar">
            <div class="heatmap-month-gutter" aria-hidden="true" />
            <div class="heatmap-months">
              <span
                v-for="week in HEATMAP_WEEK_COUNT"
                :key="'sk-month-' + week"
                class="heatmap-month"
              />
            </div>
            <div class="heatmap-weekdays" aria-hidden="true">
              <span
                v-for="item in weekdayLabels"
                :key="'sk-wd-' + item.weekday"
                class="heatmap-weekday"
              >
                {{ item.label }}
              </span>
            </div>
            <div class="heatmap-grid">
              <span
                v-for="index in HEATMAP_WEEK_COUNT * 7"
                :key="'cell-skeleton-' + index"
                class="heatmap-cell heatmap-level-0 skeleton-stagger"
                :style="{ '--skeleton-index': Math.floor((index - 1) / 7) }"
              />
            </div>
          </div>
        </div>

        <div
          v-else
          class="heatmap"
          data-testid="overview-heatmap"
          data-heatmap-kind="calendar"
          :style="{ '--heatmap-weeks': heatmap.weeks.length }"
          @pointermove="handleBoardPointer"
          @pointerleave="clearTip"
        >
          <div class="heatmap-calendar">
            <div class="heatmap-month-gutter" aria-hidden="true" />
            <div class="heatmap-months">
              <span
                v-for="(date, weekIndex) in heatmap.monthLabels"
                :key="'month-' + weekIndex"
                class="heatmap-month"
              >
                {{ monthCaption(date) }}
              </span>
            </div>
            <div class="heatmap-weekdays" aria-hidden="true">
              <span
                v-for="item in weekdayLabels"
                :key="'wd-' + item.weekday"
                class="heatmap-weekday"
              >
                {{ item.label }}
              </span>
            </div>
            <div class="heatmap-grid">
              <template v-for="(week, weekIndex) in heatmap.weeks" :key="'week-' + weekIndex">
                <span
                  v-for="cell in week"
                  :key="cell.date"
                  class="heatmap-cell"
                  :class="'heatmap-level-' + cell.level"
                  data-testid="overview-heatmap-cell"
                  :data-date="cell.date"
                  :data-request-count="String(cell.requestCount)"
                  :data-token-count="String(cell.tokenCount)"
                  :data-cost-usd-micros="String(cell.costUsdMicros)"
                  :aria-label="cellAriaLabel(cell)"
                />
              </template>
            </div>
          </div>
        </div>

        <p v-if="!loading" class="heatmap-legend">
          <span>{{ t('overview.heatmapLess') }}</span>
          <span class="heatmap-cell heatmap-level-0 heatmap-legend-swatch" />
          <span class="heatmap-cell heatmap-level-1 heatmap-legend-swatch" />
          <span class="heatmap-cell heatmap-level-2 heatmap-legend-swatch" />
          <span class="heatmap-cell heatmap-level-3 heatmap-legend-swatch" />
          <span class="heatmap-cell heatmap-level-4 heatmap-legend-swatch" />
          <span>{{ t('overview.heatmapMore') }}</span>
        </p>
      </div>

      <div class="heatmap-lifetime" data-testid="overview-lifetime">
        <div v-if="lifetimeError" class="heatmap-lifetime-error">
          <p class="text-danger text-sm">{{ lifetimeError }}</p>
          <button type="button" class="btn btn-sm" @click="emit('retryLifetime')">
            {{ t('common.retry') }}
          </button>
        </div>
        <template v-else>
          <div>
            <div class="heatmap-lifetime-label">{{ t('overview.lifetimeRequests') }}</div>
            <div
              v-if="lifetime"
              class="heatmap-lifetime-value"
              data-testid="overview-lifetime-requests"
            >
              {{ formatCount(lifetime.request_count, locale) }}
            </div>
            <SkeletonBlock v-else-if="lifetimeLoading" height="h-6" width="w-16" class="mt-1" />
          </div>
          <div>
            <div class="heatmap-lifetime-label">{{ t('overview.lifetimeCost') }}</div>
            <div
              v-if="lifetime"
              class="heatmap-lifetime-value"
              data-testid="overview-lifetime-cost"
            >
              {{ formatUsdMicros(lifetime.cost_usd_micros) }}
            </div>
            <SkeletonBlock v-else-if="lifetimeLoading" height="h-6" width="w-20" class="mt-1" />
          </div>
          <div>
            <div class="heatmap-lifetime-label">{{ t('overview.lifetimeTokens') }}</div>
            <div
              v-if="lifetime"
              class="heatmap-lifetime-value"
              data-testid="overview-lifetime-tokens"
            >
              {{ formatTokensMillions(lifetime.total_tokens) }}
            </div>
            <SkeletonBlock v-else-if="lifetimeLoading" height="h-6" width="w-20" class="mt-1" />
          </div>
        </template>
      </div>
    </div>

    <div
      v-if="hovered"
      class="heatmap-tooltip"
      data-testid="overview-heatmap-tooltip"
      :style="{ left: tipX + 'px', top: tipY + 'px' }"
    >
      <p class="heatmap-tooltip-date">{{ hoveredDateLabel }}</p>
      <p class="heatmap-tooltip-row">
        <span>{{ t('overview.requests') }}</span>
        <span class="heatmap-tooltip-value">{{ formatCount(hovered.requestCount, locale) }}</span>
      </p>
      <p class="heatmap-tooltip-row">
        <span>{{ t('overview.tokenSpend') }}</span>
        <span class="heatmap-tooltip-value">{{ formatTokensMillions(hovered.tokenCount) }}</span>
      </p>
      <p class="heatmap-tooltip-row">
        <span>{{ t('overview.cost') }}</span>
        <span class="heatmap-tooltip-value">{{ formatUsdMicros(hovered.costUsdMicros) }}</span>
      </p>
    </div>
  </div>
</template>
