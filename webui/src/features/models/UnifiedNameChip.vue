<script setup lang="ts">
// 组内把统一模型标成身份徽章；成员渠道与状态由外层展开。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import Tooltip from '@/components/ui/Tooltip.vue';

const props = defineProps<{
  name: string;
}>();

const { t } = useI18n();

const hint = computed(() => `${props.name} · ${t('models.unifiedChipTooltip')}`);
</script>

<template>
  <Tooltip :text="hint">
    <span
      class="badge badge-unified inline-flex w-fit max-w-[9rem] shrink-0 truncate font-mono"
      data-testid="group-unified-chip"
      :data-model="name"
    >
      {{ name }}
    </span>
  </Tooltip>
</template>
