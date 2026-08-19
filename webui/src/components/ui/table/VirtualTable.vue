<script setup lang="ts" generic="T">
// 虚拟滚动表：表头在滚动容器外（滚动条只在表体），tbody 只渲染可见行 + 上下占位。
// 官方 sticky-thead 示例会把滚动条拉到表头右侧；拆表头后虚拟器偏移从 0 起算，也不再需要 paddingStart。
import { computed, useTemplateRef } from 'vue';
import { useVirtualizer } from '@tanstack/vue-virtual';
import EmptyState from '@/components/ui/EmptyState.vue';
import SplitTable from '@/components/ui/table/SplitTable.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';

/** 与 SplitTable `defineExpose` 对齐；不从 SFC 再导出类型，避免 eslint 解析失败。 */
type SplitTableHandle = {
  getScrollElement: () => HTMLElement | null;
};

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

const splitTable = useTemplateRef<SplitTableHandle>('splitTable');

const virtualizer = useVirtualizer(
  computed(() => ({
    count: props.loading ? 0 : props.rows.length,
    getScrollElement: () => splitTable.value?.getScrollElement() ?? null,
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
  <SplitTable ref="splitTable" :columns="columns" :class="props.class">
    <template #header>
      <slot name="header" />
    </template>
    <TableRowsSkeleton v-if="loading" :columns="colspan" />
    <template v-else-if="rows.length > 0">
      <TableRow v-if="paddingTop > 0" aria-hidden="true" class="pointer-events-none border-0">
        <TableCell :colspan="colspan" class="p-0" :style="{ height: `${paddingTop}px` }" />
      </TableRow>
      <template v-for="item in visibleRows" :key="item.key">
        <slot name="row" :row="item.row" :index="item.index" />
      </template>
      <TableRow v-if="paddingBottom > 0" aria-hidden="true" class="pointer-events-none border-0">
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
  </SplitTable>
</template>
