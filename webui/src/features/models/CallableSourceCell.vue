<script setup lang="ts">
// 组编辑器来源列：统一模型展开成员（含状态标）；普通名列出当前渠道。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { ChannelView } from '@/api/types';
import OverflowChips, { type OverflowChip } from '@/components/ui/OverflowChips.vue';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import UnifiedJumpOrder from '@/features/models/UnifiedJumpOrder.vue';
import { memberSourceI18nKey, type CallableSourceLine } from '@/lib/unified-sources';

const props = defineProps<{
  line: CallableSourceLine;
  channels: ChannelView[];
}>();

const { t } = useI18n();

const ordinaryChips = computed((): OverflowChip[] =>
  props.line.channels.map((channel) => {
    const key = memberSourceI18nKey(channel.kind);
    if (key === undefined) return { name: channel.name };
    return { name: channel.name, aside: t(key), asideKind: channel.kind };
  }),
);
</script>

<template>
  <UnifiedJumpOrder
    v-if="line.isUnified"
    :members="line.unifiedMembers"
    :channels="channels"
    hide-index
  />
  <OverflowChips
    v-else-if="line.channels.length > 0"
    :items="ordinaryChips"
    chip-test-id="group-source-channel"
  />
  <ChannelSourceMark v-else kind="unlisted" />
</template>
