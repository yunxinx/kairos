<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { LogEntry } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import LogBodyPanel from '@/features/logs/LogBodyPanel.vue';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    entry: LogEntry;
    detail?: LogEntry | null;
    detailLoading?: boolean;
    detailError?: string;
    channelProtocolMap?: Map<string, string> | null;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  {
    detail: null,
    detailLoading: false,
    detailError: '',
    channelProtocolMap: null,
    anchor: null,
    stackOrder: 0,
    cascade: 0,
    attention: false,
    topmost: true,
  },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  retryDetail: [];
}>();

const { t } = useI18n();

const windowTitle = computed(
  () => `#${props.entry.id} · ${props.entry.model} · ${t('logs.bodyTitle')}`,
);

function statusBadgeClass(statusCode: number): string {
  if (statusCode >= 200 && statusCode < 300) return 'badge-success';
  if (statusCode >= 400 && statusCode < 500) return 'badge-warn';
  return 'badge-danger';
}
</script>

<template>
  <FloatingWindow
    :title="windowTitle"
    extra-wide
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    data-testid="request-log-body-window"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <template #header-extra>
      <div class="mr-2 ml-auto flex items-center gap-1.5">
        <span class="badge text-[11px]" :class="statusBadgeClass(entry.status_code)">
          {{ entry.status_code }}
        </span>
      </div>
    </template>

    <div class="flex flex-col gap-3 p-4">
      <LogBodyPanel v-if="detail" :entry="detail" />
      <p
        v-else-if="detailLoading"
        class="text-fg-muted py-8 text-center text-sm"
        data-testid="log-body-loading"
      >
        {{ t('logs.bodyLoading') }}
      </p>
      <div
        v-else-if="detailError"
        class="flex flex-col items-center gap-2 py-8"
        data-testid="log-body-error"
      >
        <p class="text-danger text-center text-sm">{{ detailError }}</p>
        <button type="button" class="btn btn-sm" @click="emit('retryDetail')">
          {{ t('common.retry') }}
        </button>
      </div>
    </div>
  </FloatingWindow>
</template>
