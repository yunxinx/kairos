<script setup lang="ts">
// 路由顺序的一格：hop（可选）+ 省略模型名 + 渠道标。
import { useI18n } from 'vue-i18n';
import Tooltip from '@/components/ui/Tooltip.vue';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import type { MemberSourceKind } from '@/lib/unified-sources';

const props = defineProps<{
  member: string;
  channel: string;
  kind: MemberSourceKind;
  hop: number;
  showIndex: boolean;
}>();

const { t } = useI18n();
</script>

<template>
  <li
    class="flex min-w-0 items-center gap-1"
    data-testid="unified-member-line"
    :data-member="props.member"
    :data-channel="props.channel"
    :data-source-kind="props.kind"
  >
    <span
      v-if="props.showIndex"
      class="route-index"
      data-testid="unified-member-index"
      :aria-label="t('models.routeHopIndex', { n: props.hop })"
      >{{ props.hop }}</span
    >
    <Tooltip :text="props.member">
      <span class="min-w-0 truncate font-mono text-sm">{{ props.member }}</span>
    </Tooltip>
    <ChannelSourceMark :channel-name="props.channel" :kind="props.kind" />
  </li>
</template>
