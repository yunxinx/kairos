<script setup lang="ts">
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableRow from '@/components/ui/table/TableRow.vue';

withDefaults(
  defineProps<{
    columns?: number;
    rows?: number;
  }>(),
  {
    columns: 5,
    rows: 6,
  },
);
</script>

<template>
  <TableRow
    v-for="row in rows"
    :key="'skeleton-row-' + row"
    class="skeleton-stagger pointer-events-none"
    :style="{ '--skeleton-index': row - 1 }"
    aria-hidden="true"
  >
    <TableCell v-for="column in columns" :key="'skeleton-cell-' + column">
      <SkeletonBlock height="h-4" :width="column === 1 ? 'w-36' : 'w-20'" />
    </TableCell>
  </TableRow>
</template>
