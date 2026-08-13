<script setup lang="ts">
import { computed, useTemplateRef } from 'vue';
import { useVirtualizer } from '@tanstack/vue-virtual';
import { cn } from '@/lib/cn';
import type { VirtualDataTableColumn } from '@/components/ui/virtual-data-table-columns';
import { virtualDataTableMinWidth } from '@/components/ui/virtual-data-table-columns';

const props = withDefaults(
  defineProps<{
    rowCount: number;
    estimateRowHeight?: number;
    /** 固定列宽；与表头列数一致时启用 table-fixed + colgroup。 */
    columns?: VirtualDataTableColumn[] | undefined;
    /** 按行覆盖预估高度（如展开详情行）。 */
    getRowHeight?: ((index: number) => number) | undefined;
    class?: string;
  }>(),
  {
    estimateRowHeight: 52,
    columns: undefined,
    getRowHeight: undefined,
    class: '',
  },
);

const scrollElement = useTemplateRef<HTMLDivElement>('scrollElement');

const rowVirtualizer = useVirtualizer(
  computed(() => ({
    count: props.rowCount,
    getScrollElement: () => scrollElement.value,
    estimateSize: (index: number) => props.getRowHeight?.(index) ?? props.estimateRowHeight,
    overscan: 10,
  })),
);

const virtualRows = computed(() => rowVirtualizer.value.getVirtualItems());

const paddingTop = computed(() => {
  const rows = virtualRows.value;
  const first = rows[0];
  return first ? first.start : 0;
});

const paddingBottom = computed(() => {
  const rows = virtualRows.value;
  const last = rows[rows.length - 1];
  if (!last) {
    return 0;
  }
  return rowVirtualizer.value.getTotalSize() - (last.start + last.size);
});

const tableMinWidth = computed(() =>
  props.columns ? virtualDataTableMinWidth(props.columns) : undefined,
);

const columnCount = computed(() => props.columns?.length ?? 100);

const tableLayoutClass = computed(() => (props.columns ? 'table-fixed' : ''));

const tableStyle = computed(() =>
  tableMinWidth.value ? { minWidth: tableMinWidth.value } : undefined,
);

/**
 * 仅在 Vitest/jsdom 无布局时回退全量行渲染。
 * 浏览器中若误用 fallback 会把整页数据打进 DOM，撑高表格并破坏 flex 高度约束。
 */
const useFallbackRender = computed(
  () => import.meta.env.MODE === 'test' && props.rowCount > 0 && virtualRows.value.length === 0,
);

const fallbackIndices = computed(() =>
  useFallbackRender.value ? Array.from({ length: props.rowCount }, (_, index) => index) : [],
);

function resolveRowHTMLElement(element: unknown): HTMLElement | null {
  if (!element) {
    return null;
  }
  if (element instanceof HTMLElement) {
    return element;
  }
  if (typeof element === 'object' && '$el' in element) {
    const root = element.$el;
    if (root instanceof HTMLElement) {
      return root;
    }
  }
  return null;
}

function measureRowElement(element: unknown, index: number) {
  const rowElement = resolveRowHTMLElement(element);
  if (!rowElement) {
    return;
  }
  rowElement.dataset.index = String(index);
  rowVirtualizer.value.measureElement(rowElement);
}
</script>

<template>
  <div
    data-slot="virtual-data-table-root"
    :class="cn('relative flex min-h-0 flex-1 flex-col overflow-hidden', props.class)"
  >
    <div
      v-if="$slots.header"
      data-slot="virtual-data-table-header"
      class="seed-scrollbar shrink-0 [scrollbar-width:none] overflow-x-auto border-b border-[var(--seed-border)] [&::-webkit-scrollbar]:hidden"
    >
      <table
        data-slot="virtual-data-table-header-table"
        :class="cn('w-full caption-bottom text-sm', tableLayoutClass)"
        :style="tableStyle"
      >
        <colgroup v-if="columns?.length">
          <col
            v-for="column in columns"
            :key="`head-${column.id}`"
            :style="{ width: column.width }"
          />
        </colgroup>
        <slot name="header" />
      </table>
    </div>

    <div
      ref="scrollElement"
      data-slot="virtual-data-table-scroll"
      class="seed-scrollbar min-h-0 flex-1 overflow-auto"
    >
      <table
        data-slot="virtual-data-table"
        :class="cn('w-full caption-bottom text-sm', tableLayoutClass)"
        :style="tableStyle"
      >
        <colgroup v-if="columns?.length">
          <col v-for="column in columns" :key="column.id" :style="{ width: column.width }" />
        </colgroup>
        <tbody data-slot="table-body">
          <template v-if="useFallbackRender">
            <template v-for="index in fallbackIndices" :key="`fallback-${index}`">
              <slot
                name="row"
                :index="index"
                :measure-row="(el: unknown) => measureRowElement(el, index)"
              />
            </template>
          </template>
          <template v-else>
            <tr v-if="paddingTop > 0" aria-hidden="true">
              <td
                :style="{ height: `${paddingTop}px` }"
                :colspan="columnCount"
                class="border-0 p-0"
              />
            </tr>
            <template v-for="virtualRow in virtualRows" :key="virtualRow.key">
              <slot
                name="row"
                :index="virtualRow.index"
                :measure-row="(el: unknown) => measureRowElement(el, virtualRow.index)"
              />
            </template>
            <tr v-if="paddingBottom > 0" aria-hidden="true">
              <td
                :style="{ height: `${paddingBottom}px` }"
                :colspan="columnCount"
                class="border-0 p-0"
              />
            </tr>
          </template>
        </tbody>
      </table>
    </div>

    <div
      v-if="rowCount === 0 && $slots.empty"
      data-slot="virtual-data-table-empty"
      class="pointer-events-none absolute inset-0 flex items-center justify-center"
    >
      <div class="pointer-events-auto">
        <slot name="empty" />
      </div>
    </div>
  </div>
</template>

<style scoped>
:deep([data-slot='virtual-data-table-header'] th) {
  background: var(--seed-surface);
}
</style>
