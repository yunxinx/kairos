<script setup lang="ts">
// 行选择单元格：复选框在单元格内垂直水平居中（flex 容器），三张资源表共用。
import { useI18n } from 'vue-i18n';
import Checkbox from '@/components/ui/Checkbox.vue';
import TableCell from '@/components/ui/table/TableCell.vue';

defineProps<{
  checked: boolean;
  testId: string;
}>();

const emit = defineEmits<{
  toggle: [];
}>();

const { t } = useI18n();
</script>

<template>
  <TableCell class="w-10">
    <div class="flex items-center justify-center">
      <Checkbox
        :model-value="checked"
        :data-testid="testId"
        :aria-label="t('common.selectRow')"
        @update:model-value="() => emit('toggle')"
      />
    </div>
  </TableCell>
</template>
