<script setup lang="ts">
import DataTable from '@/components/ui/data-table/DataTable.vue';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';

withDefaults(
  defineProps<{
    columns?: number;
    rows?: number;
    withToolbar?: boolean;
    /** 与列表页 DataTable 同槽位占满剩余高度，避免 skeleton→表格高度跳动。 */
    fillViewport?: boolean;
  }>(),
  {
    columns: 5,
    rows: 6,
    withToolbar: true,
    fillViewport: false,
  },
);
</script>

<template>
  <DataTable role="status" :aria-label="$t('common.loading')" :fill-viewport="fillViewport">
    <template v-if="withToolbar" #toolbar>
      <SkeletonBlock height="h-8" width="w-full max-w-xs" />
    </template>
    <TableHeader>
      <TableRow>
        <TableHead v-for="c in columns" :key="'th' + c">
          <SkeletonBlock height="h-3" width="w-16" />
        </TableHead>
      </TableRow>
    </TableHeader>
    <TableBody>
      <TableRow v-for="r in rows" :key="r">
        <TableCell v-for="c in columns" :key="c">
          <SkeletonBlock height="h-4" :width="c === 1 ? 'w-36' : 'w-20'" />
        </TableCell>
      </TableRow>
    </TableBody>
  </DataTable>
</template>
