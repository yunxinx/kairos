<script setup lang="ts">
import type { ComponentPublicInstance } from 'vue';
import { useI18n } from 'vue-i18n';
import type { LogEntry } from '@/api/types';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import LogBodyPanel from '@/features/logs/LogBodyPanel.vue';
import type { FlatLogRow } from '@/features/logs/flat-log-row';
import { formatUnixMillis, formatUsdMicros } from '@/lib/format';

const props = defineProps<{
  flatRow: FlatLogRow;
  entry: LogEntry;
  expanded: boolean;
  detailColSpan: number;
  measureRow: (element: unknown) => void;
}>();

const emit = defineEmits<{
  toggleExpand: [];
}>();

const { t, locale } = useI18n();

function onRowRef(element: Element | ComponentPublicInstance | null) {
  props.measureRow(element);
}

function statusBadgeClass(statusCode: number): string {
  return statusCode >= 200 && statusCode < 300 ? 'badge-success' : 'badge-danger';
}
</script>

<template>
  <TableRow
    v-if="flatRow.kind === 'main'"
    :ref="onRowRef"
    data-testid="log-row"
    :data-log-id="String(entry.id)"
    :data-model="entry.model"
    :data-token-key="entry.token_key"
  >
    <TableCell class="text-fg-muted font-mono text-xs">
      {{ formatUnixMillis(entry.created_at, locale) }}
    </TableCell>
    <TableCell class="max-w-0 truncate" :title="entry.token_key">
      {{ entry.token_name }}
    </TableCell>
    <TableCell data-testid="log-model">{{ entry.model }}</TableCell>
    <TableCell data-testid="log-channel">{{ entry.channel }}</TableCell>
    <TableCell>
      <span class="badge" :class="statusBadgeClass(entry.status_code)" data-testid="log-status">{{
        entry.status_code
      }}</span>
    </TableCell>
    <TableCell class="font-mono text-sm" data-testid="log-latency">
      {{ entry.latency_ms }} ms
    </TableCell>
    <TableCell class="font-mono text-sm" data-testid="log-cost">
      {{ formatUsdMicros(entry.cost_usd_micros) }}
    </TableCell>
    <TableCell>
      <button
        type="button"
        class="text-fg-muted hover:text-fg inline-flex h-7 w-7 items-center justify-center rounded-md"
        data-testid="log-expand"
        :aria-expanded="expanded ? 'true' : 'false'"
        :aria-label="expanded ? t('logs.collapseDetails') : t('logs.expandDetails')"
        @click="emit('toggleExpand')"
      >
        <UiIcon :name="expanded ? 'chevron-up' : 'chevron-down'" :size="16" />
      </button>
    </TableCell>
  </TableRow>
  <TableRow v-else :ref="onRowRef" data-testid="log-detail-row" class="hover:bg-transparent">
    <TableCell :colspan="detailColSpan" class="align-top whitespace-normal">
      <LogBodyPanel :entry="entry" />
    </TableCell>
  </TableRow>
</template>
