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
    /** 顶部指标卡数量 */
    statCards?: number;
    /** 是否包含图表区域骨架 */
    withChart?: boolean;
    /** 是否包含表格区域骨架 */
    withTable?: boolean;
    tableColumns?: number;
    tableRows?: number;
  }>(),
  {
    statCards: 4,
    withChart: false,
    withTable: false,
    tableColumns: 5,
    tableRows: 5,
  },
);
</script>

<template>
  <div class="space-y-6" role="status" :aria-label="$t('common.loading')">
    <section v-if="statCards > 0">
      <SkeletonBlock height="h-5" width="w-32" class="mb-3" />
      <div
        class="grid grid-cols-2 gap-3"
        :class="statCards > 3 ? 'lg:grid-cols-4' : 'lg:grid-cols-3'"
      >
        <div v-for="i in statCards" :key="i" class="card">
          <div class="card-body space-y-2">
            <SkeletonBlock height="h-3" width="w-20" />
            <SkeletonBlock height="h-8" width="w-24" />
          </div>
        </div>
      </div>
    </section>

    <section v-if="withChart" class="card">
      <div class="card-header">
        <SkeletonBlock height="h-5" width="w-28" />
      </div>
      <div class="card-body">
        <SkeletonBlock height="h-52" width="w-full" rounded="rounded" />
      </div>
    </section>

    <DataTablePanel v-if="withTable">
      <div class="border-b border-[var(--seed-border)] px-4 py-3">
        <SkeletonBlock height="h-9" width="w-full max-w-md" />
      </div>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead v-for="c in tableColumns" :key="'h' + c">
              <SkeletonBlock height="h-3" :width="c === 1 ? 'w-24' : 'w-16'" />
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="r in tableRows" :key="'r' + r">
            <TableCell v-for="c in tableColumns" :key="'c' + c">
              <SkeletonBlock height="h-4" :width="c === 1 ? 'w-32' : 'w-20'" />
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </DataTablePanel>
  </div>
</template>
