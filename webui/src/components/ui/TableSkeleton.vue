<script setup lang="ts">
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import Table from '@/components/ui/table/Table.vue';
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
    /** 与列表页 DataTablePanel 同槽位占满剩余高度，避免 skeleton→表格高度跳动。 */
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
  <DataTablePanel role="status" :aria-label="$t('common.loading')" :fill-viewport="fillViewport">
    <div v-if="withToolbar" class="border-b border-[var(--seed-border)] px-4 py-3">
      <SkeletonBlock height="h-9" width="w-full max-w-md" />
    </div>
    <Table>
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
    </Table>
  </DataTablePanel>
</template>
