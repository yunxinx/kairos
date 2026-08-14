<script setup lang="ts">
import { cn } from '@/lib/cn';
import { useI18n } from 'vue-i18n';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
    /** 表体正在加载骨架时标忙，避免读屏把空表当结果。 */
    busy?: boolean;
  }>(),
  {
    class: '',
    busy: false,
  },
);

const { t } = useI18n();
</script>

<template>
  <!--
    单表（thead+tbody 同一 <table>）+ 工具栏在边框外 + 分页在边框下。
    刻意不拆表头/表体、不做虚拟滚动：管理端列表短，分表是列宽错位的主因。
    表格自然撑开高度，垂直滚动由页面（main）承担，表内只处理横向滚动。
  -->
  <div
    data-slot="data-table"
    v-bind="$attrs"
    :role="props.busy ? 'status' : undefined"
    :aria-label="props.busy ? t('common.loading') : undefined"
    :class="cn('flex flex-col gap-4', props.class)"
  >
    <div v-if="$slots.toolbar" data-slot="data-table-toolbar-slot" class="shrink-0">
      <slot name="toolbar" />
    </div>
    <DataTablePanel>
      <div data-slot="data-table-scroll" class="seed-scrollbar relative w-full overflow-x-auto">
        <table
          data-slot="table"
          class="w-full caption-bottom text-sm"
          :aria-busy="props.busy ? 'true' : undefined"
        >
          <slot />
        </table>
      </div>
    </DataTablePanel>
    <div v-if="$slots.pagination" data-slot="data-table-pagination-slot" class="shrink-0">
      <slot name="pagination" />
    </div>
  </div>
</template>
