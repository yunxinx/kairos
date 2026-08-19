<script setup lang="ts">
import { computed } from 'vue';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import type { ChannelView, UnifiedMember } from '@/api/types';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import { channelNameForMember, memberSourceKind, unifiedMemberKey } from '@/lib/unified-sources';

const VISIBLE_COUNT = 3;

const props = defineProps<{
  members: UnifiedMember[];
  channels: ChannelView[];
  /** 隐藏列是成员名单不是路由：传 true 用 ul 且永不标 hop。缺省用 ol，两条及以上才标。 */
  hideIndex?: boolean;
}>();

const { t } = useI18n();

const lines = computed(() =>
  props.members.map((member) => ({
    key: unifiedMemberKey(member),
    member: member.model,
    channel: channelNameForMember(props.channels, member),
    kind: memberSourceKind(member, props.channels),
  })),
);

const listTag = computed(() => (props.hideIndex ? 'ul' : 'ol'));
const indexVisible = computed(() => !props.hideIndex && lines.value.length > 1);
const visible = computed(() => lines.value.slice(0, VISIBLE_COUNT));
const hidden = computed(() => lines.value.slice(VISIBLE_COUNT));
const hiddenCount = computed(() => hidden.value.length);

function hopLabel(index: number): string {
  return t('models.routeHopIndex', { n: index });
}
</script>

<template>
  <div v-if="lines.length > 0" class="flex flex-col gap-1" data-testid="unified-members">
    <component :is="listTag" class="m-0 flex list-none flex-col gap-1 p-0">
      <li
        v-for="(line, index) in visible"
        :key="line.key"
        class="flex flex-wrap items-center gap-2"
        data-testid="unified-member-line"
        :data-member="line.member"
        :data-channel="line.channel"
        :data-source-kind="line.kind"
      >
        <span
          v-if="indexVisible"
          class="route-index"
          data-testid="unified-member-index"
          :aria-label="hopLabel(index + 1)"
          >{{ index + 1 }}</span
        >
        <span class="font-mono text-sm">{{ line.member }}</span>
        <ChannelSourceMark :channel-name="line.channel" :kind="line.kind" />
      </li>
    </component>
    <PopoverRoot v-if="hiddenCount > 0">
      <PopoverTrigger as-child>
        <button
          type="button"
          class="badge badge-neutral w-fit cursor-pointer"
          data-testid="overflow-more"
          :aria-label="t('common.moreCount', { count: hiddenCount })"
        >
          {{ t('common.moreCount', { count: hiddenCount }) }}
        </button>
      </PopoverTrigger>
      <PopoverPortal>
        <PopoverContent
          align="start"
          :side-offset="4"
          class="data-table-menu overflow-chip-menu seed-scrollbar"
          data-testid="overflow-chip-menu"
        >
          <component :is="listTag" class="m-0 flex list-none flex-col gap-2 p-1">
            <li
              v-for="(line, offset) in hidden"
              :key="line.key"
              class="flex flex-wrap items-center gap-2"
              data-testid="unified-member-line"
              :data-member="line.member"
              :data-channel="line.channel"
              :data-source-kind="line.kind"
            >
              <span
                v-if="indexVisible"
                class="route-index"
                data-testid="unified-member-index"
                :aria-label="hopLabel(VISIBLE_COUNT + offset + 1)"
                >{{ VISIBLE_COUNT + offset + 1 }}</span
              >
              <span class="font-mono text-sm">{{ line.member }}</span>
              <ChannelSourceMark :channel-name="line.channel" :kind="line.kind" />
            </li>
          </component>
        </PopoverContent>
      </PopoverPortal>
    </PopoverRoot>
  </div>
  <span v-else class="text-fg-muted">{{ $t('common.emptyCell') }}</span>
</template>
