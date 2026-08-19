<script setup lang="ts">
// 组内把统一模型标成身份徽章；成员渠道与状态由外层展开。
import { useI18n } from 'vue-i18n';
import Tooltip from '@/components/ui/Tooltip.vue';

defineProps<{
  name: string;
}>();

const { t } = useI18n();
</script>

<template>
  <Tooltip :text="t('models.unifiedChipTooltip')">
    <span
      class="badge badge-unified max-w-[9rem] truncate font-mono"
      data-testid="group-unified-chip"
      :data-model="name"
    >
      {{ name }}
    </span>
  </Tooltip>
</template>
