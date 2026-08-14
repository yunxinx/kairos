<script setup lang="ts">
// 复合时间范围选择：触发条展示已选范围，面板内可精调起止时间或走快速选择，确认后提交。
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import UiIcon from '@/components/ui/UiIcon.vue';
import {
  type DateRange,
  type DateRangeQuickKey,
  datetimeLocalToMillis,
  formatRangeLabel,
  millisToDatetimeLocal,
  quickRange,
} from '@/lib/date-range';

defineProps<{
  triggerId: string;
  fromInputId: string;
  toInputId: string;
  triggerTestId?: string | undefined;
}>();

const { t } = useI18n();

const range = defineModel<DateRange>({ required: true });

const open = ref(false);
const draftFrom = ref('');
const draftTo = ref('');
const activeQuick = ref<DateRangeQuickKey | null>(null);

// 打开面板时以当前值为草稿起点，避免残留上次未确认的编辑。
watch(open, (next) => {
  if (!next) {
    return;
  }
  draftFrom.value = range.value.from === null ? '' : millisToDatetimeLocal(range.value.from);
  draftTo.value = range.value.to === null ? '' : millisToDatetimeLocal(range.value.to);
  activeQuick.value = null;
});

const quickPicks: readonly DateRangeQuickKey[] = ['today', 'days7', 'days15', 'days30', 'month'];

const triggerLabel = computed(() => formatRangeLabel(range.value));

function onManualEdit() {
  activeQuick.value = null;
}

function applyQuickPick(key: DateRangeQuickKey) {
  const picked = quickRange(key, new Date());
  draftFrom.value = picked.from === null ? '' : millisToDatetimeLocal(picked.from);
  draftTo.value = picked.to === null ? '' : millisToDatetimeLocal(picked.to);
  activeQuick.value = key;
}

function confirm() {
  const next: DateRange = {
    from: datetimeLocalToMillis(draftFrom.value),
    to: datetimeLocalToMillis(draftTo.value),
  };
  open.value = false;
  // 值未变化时不替换模型对象，避免消费方的变更监听（如重置分页）被误触发。
  if (next.from === range.value.from && next.to === range.value.to) {
    return;
  }
  range.value = next;
}
</script>

<template>
  <PopoverRoot v-model:open="open">
    <PopoverTrigger as-child>
      <button
        :id="triggerId"
        type="button"
        class="input date-range-trigger h-8"
        :data-testid="triggerTestId"
        :aria-label="t('dateRange.placeholder')"
        aria-haspopup="dialog"
        :aria-expanded="open ? 'true' : 'false'"
      >
        <UiIcon name="calendar" :size="14" class="text-fg-subtle shrink-0" />
        <span v-if="triggerLabel" class="truncate font-mono text-xs">{{ triggerLabel }}</span>
        <span v-else class="text-fg-muted truncate text-xs">{{ t('dateRange.placeholder') }}</span>
      </button>
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent
        class="reka-popover-content ui-date-range-content"
        side="bottom"
        align="end"
        :side-offset="6"
      >
        <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <label :for="fromInputId" class="block">
            <span class="text-fg-muted mb-1 block text-xs font-medium">
              {{ t('dateRange.startTime') }}
            </span>
            <input
              :id="fromInputId"
              v-model="draftFrom"
              type="datetime-local"
              class="input h-9 w-full"
              @input="onManualEdit"
            />
          </label>
          <label :for="toInputId" class="block">
            <span class="text-fg-muted mb-1 block text-xs font-medium">
              {{ t('dateRange.endTime') }}
            </span>
            <input
              :id="toInputId"
              v-model="draftTo"
              type="datetime-local"
              class="input h-9 w-full"
              @input="onManualEdit"
            />
          </label>
        </div>
        <div class="mt-3 flex flex-wrap gap-2">
          <button
            v-for="key in quickPicks"
            :key="key"
            type="button"
            class="date-range-quick"
            :data-testid="`date-range-quick-${key}`"
            :aria-pressed="activeQuick === key ? 'true' : 'false'"
            @click="applyQuickPick(key)"
          >
            {{ t(`dateRange.quick.${key}`) }}
          </button>
        </div>
        <div class="mt-4 flex justify-end">
          <button
            type="button"
            class="btn btn-primary"
            data-testid="date-range-confirm"
            @click="confirm"
          >
            {{ t('dateRange.confirm') }}
          </button>
        </div>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
