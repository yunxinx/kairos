<script setup lang="ts" generic="T">
// 虚拟滚动表：外层 overflow 容器滚动，thead sticky，tbody 只渲染可见行 + 上下占位。
// 不套 Table.vue 的 overflow-x-auto 包装，否则 sticky 会失效。
import { computed, ref } from 'vue';
import { useVirtualizer } from '@tanstack/vue-virtual';
import EmptyState from '@/components/ui/EmptyState.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { cn } from '@/lib/cn';

const props = withDefaults(
  defineProps<{
    rows: T[];
    colspan: number;
    /** 列宽，与 `colspan` 等长；`table-layout:fixed` 下由 colgroup 决定，避免虚拟行撑开列。 */
    columns: { width: string }[];
    estimateSize?: number;
    overscan?: number;
    loading?: boolean;
    class?: string;
    emptyTitle?: string;
    getRowKey: (row: T, index: number) => string | number;
  }>(),
  {
    estimateSize: 40,
    overscan: 8,
    loading: false,
    class: '',
    emptyTitle: '',
  },
);

const scrollRef = ref<HTMLElement | null>(null);

const virtualizer = useVirtualizer(
  computed(() => ({
    count: props.loading ? 0 : props.rows.length,
    getScrollElement: () => scrollRef.value,
    estimateSize: () => props.estimateSize,
    overscan: props.overscan,
    getItemKey: (index: number) => {
      const row = props.rows[index];
      if (row === undefined) return index;
      return props.getRowKey(row, index);
    },
  })),
);

const virtualItems = computed(() => virtualizer.value.getVirtualItems());

const visibleRows = computed(() => {
  const items: { key: string; index: number; row: T }[] = [];
  for (const item of virtualItems.value) {
    const row = props.rows[item.index];
    if (row === undefined) continue;
    items.push({ key: String(item.key), index: item.index, row });
  }
  return items;
});

const paddingTop = computed(() => virtualItems.value[0]?.start ?? 0);

const paddingBottom = computed(() => {
  const last = virtualItems.value[virtualItems.value.length - 1];
  if (last === undefined) return 0;
  return Math.max(0, virtualizer.value.getTotalSize() - last.end);
});
</script>

<template>
  <div
    ref="scrollRef"
    data-slot="virtual-table"
    :class="cn('virtual-table-scroll seed-scrollbar min-h-0 overflow-auto', props.class)"
  >
    <table class="w-full table-fixed caption-bottom text-sm">
      <colgroup>
        <col v-for="(column, index) in columns" :key="index" :style="{ width: column.width }" />
      </colgroup>
      <TableHeader>
        <slot name="header" />
      </TableHeader>
      <TableBody>
        <TableRowsSkeleton v-if="loading" :columns="colspan" />
        <template v-else-if="rows.length > 0">
          <TableRow v-if="paddingTop > 0" aria-hidden="true" class="pointer-events-none border-0">
            <TableCell :colspan="colspan" class="p-0" :style="{ height: `${paddingTop}px` }" />
          </TableRow>
          <template v-for="item in visibleRows" :key="item.key">
            <slot name="row" :row="item.row" :index="item.index" />
          </template>
          <TableRow
            v-if="paddingBottom > 0"
            aria-hidden="true"
            class="pointer-events-none border-0"
          >
            <TableCell :colspan="colspan" class="p-0" :style="{ height: `${paddingBottom}px` }" />
          </TableRow>
        </template>
        <TableRow v-else>
          <TableCell :colspan="colspan" class="h-24 whitespace-normal">
            <slot name="empty">
              <EmptyState v-if="emptyTitle" :title="emptyTitle" />
            </slot>
          </TableCell>
        </TableRow>
      </TableBody>
    </table>
  </div>
</template>
