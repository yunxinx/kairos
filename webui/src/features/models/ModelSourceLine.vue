<script setup lang="ts">
// 组员列的一格：统一模型只出身份徽章；普通名省略时走 portal 提示。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import OverflowChips, { type OverflowChip } from '@/components/ui/OverflowChips.vue';
import Tooltip from '@/components/ui/Tooltip.vue';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import UnifiedNameChip from '@/features/models/UnifiedNameChip.vue';
import { memberSourceI18nKey, type CallableSourceLine } from '@/lib/unified-sources';

const props = withDefaults(
  defineProps<{
    line: CallableSourceLine;
    chipTestId?: string;
  }>(),
  { chipTestId: 'group-source-channel' },
);

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
  <li
    class="flex min-w-0 items-center gap-1"
    data-testid="group-member-line"
    :data-model="line.name"
    :data-channel="line.channels[0]?.name"
    :data-unified="line.isUnified ? 'true' : undefined"
  >
    <UnifiedNameChip v-if="line.isUnified" :name="line.name" />
    <template v-else>
      <Tooltip :text="line.name">
        <span class="min-w-0 truncate font-mono text-sm">{{ line.name }}</span>
      </Tooltip>
      <OverflowChips
        v-if="line.channels.length > 0"
        :items="ordinaryChips"
        :chip-test-id="chipTestId"
      />
      <ChannelSourceMark v-else :kind="line.emptyKind ?? 'unlisted'" />
    </template>
  </li>
</template>
