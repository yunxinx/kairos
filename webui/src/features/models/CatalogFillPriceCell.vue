<script setup lang="ts">
// 价格同步预览：覆盖模式下把将被替换的已有金额划掉，旁边/下方展示新价。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { formatUsdAmount } from '@/lib/format';

const props = defineProps<{
  current: number | null | undefined;
  next: number | null | undefined;
  /** 「覆盖已有」且该行将写入时，对将被替换的已有金额加删除线。 */
  overwrite: boolean;
}>();

const { t } = useI18n();

function formatTier(value: number | null | undefined): string {
  if (value === null || value === undefined) return t('common.emptyCell');
  return formatUsdAmount(value);
}

const replaced = computed(
  () =>
    props.overwrite &&
    props.current !== null &&
    props.current !== undefined &&
    props.current !== props.next,
);
</script>

<template>
  <span v-if="replaced" class="inline-flex flex-col items-start gap-0.5 leading-tight">
    <span class="text-fg-muted text-xs line-through">{{ formatTier(props.current) }}</span>
    <span>{{ formatTier(props.next) }}</span>
  </span>
  <template v-else>{{ formatTier(props.next) }}</template>
</template>
