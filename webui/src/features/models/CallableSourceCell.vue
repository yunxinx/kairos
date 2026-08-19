<script setup lang="ts">
// 组编辑器来源列：统一模型一枚主色 chip；普通名列出当前渠道。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import OverflowChips, { type OverflowChip } from '@/components/ui/OverflowChips.vue';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import { memberSourceI18nKey, type CallableSourceLine } from '@/lib/unified-sources';

const props = defineProps<{
  line: CallableSourceLine;
}>();

const { t } = useI18n();

const sourceChips = computed((): OverflowChip[] => {
  if (props.line.isUnified) {
    return [{ name: t('models.unifiedChipTooltip'), isUnified: true }];
  }
  return props.line.channels.map((channel) => {
    const key = memberSourceI18nKey(channel.kind);
    if (key === undefined) return { name: channel.name };
    return { name: channel.name, aside: t(key), asideKind: channel.kind };
  });
});
</script>

<template>
  <OverflowChips
    v-if="sourceChips.length > 0"
    :items="sourceChips"
    chip-test-id="group-source-channel"
  />
  <ChannelSourceMark v-else :kind="line.emptyKind ?? 'unlisted'" />
</template>
