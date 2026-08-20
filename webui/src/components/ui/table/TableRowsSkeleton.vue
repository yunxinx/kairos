<script setup lang="ts">
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableRow from '@/components/ui/table/TableRow.vue';

const props = withDefaults(
  defineProps<{
    columns?: number;
    rows?: number;
    /** 第一列与行选择复选框同宽，避免骨架条把 `w-10` 列撑开。 */
    hasSelectColumn?: boolean;
  }>(),
  {
    columns: 5,
    rows: 6,
    hasSelectColumn: false,
  },
);

function barWidth(column: number): string {
  if (props.hasSelectColumn) return 'w-full';
  return column === 1 ? 'w-36' : 'w-20';
}
</script>

<template>
  <TableRow
    v-for="row in rows"
    :key="'skeleton-row-' + row"
    class="skeleton-stagger pointer-events-none"
    :style="{ '--skeleton-index': row - 1 }"
    aria-hidden="true"
  >
    <TableCell
      v-for="column in columns"
      :key="'skeleton-cell-' + column"
      :class="props.hasSelectColumn && column === 1 ? 'w-10' : 'min-w-0'"
    >
      <div v-if="props.hasSelectColumn && column === 1" class="flex items-center justify-center">
        <SkeletonBlock height="h-4" width="w-4" />
      </div>
      <SkeletonBlock v-else height="h-4" :width="barWidth(column)" />
    </TableCell>
  </TableRow>
</template>
