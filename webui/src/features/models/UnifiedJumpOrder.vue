<script setup lang="ts">
// 路由顺序列对齐模型组「组内模型」：双栏最多 3 行，满 6 格后 +N 占最后一格。
import { computed } from 'vue';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import type { ChannelView, UnifiedMember } from '@/api/types';
import UnifiedJumpLine from '@/features/models/UnifiedJumpLine.vue';
import { overflowGridItems } from '@/lib/overflow-grid';
import { channelNameForMember, memberSourceKind, unifiedMemberKey } from '@/lib/unified-sources';

const props = defineProps<{
  members: UnifiedMember[];
  channels: ChannelView[];
  /** 隐藏列是成员名单不是路由：传 true 用 ul 且永不标 hop。缺省用 ol，两条及以上才标。 */
  hideIndex?: boolean;
}>();

const { t } = useI18n();

const lines = computed(() =>
  props.members.map((member, index) => ({
    key: unifiedMemberKey(member),
    member: member.model,
    channel: channelNameForMember(props.channels, member),
    kind: memberSourceKind(member, props.channels),
    hop: index + 1,
  })),
);

const listTag = computed(() => (props.hideIndex ? 'ul' : 'ol'));
const indexVisible = computed(() => !props.hideIndex && lines.value.length > 1);
const sliced = computed(() => overflowGridItems(lines.value));
const hiddenCount = computed(() => sliced.value.hidden.length);
</script>

<template>
  <component
    :is="listTag"
    v-if="lines.length > 0"
    class="m-0 grid max-w-full list-none grid-cols-2 gap-1.5 p-0"
    data-testid="unified-members"
  >
    <UnifiedJumpLine
      v-for="line in sliced.visible"
      :key="line.key"
      :member="line.member"
      :channel="line.channel"
      :kind="line.kind"
      :hop="line.hop"
      :show-index="indexVisible"
    />
    <li v-if="hiddenCount > 0" class="flex items-center">
      <PopoverRoot>
        <PopoverTrigger as-child>
          <button
            type="button"
            class="badge badge-neutral cursor-pointer"
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
            <component :is="listTag" class="m-0 grid list-none grid-cols-2 gap-1.5 p-1">
              <UnifiedJumpLine
                v-for="line in sliced.hidden"
                :key="line.key"
                :member="line.member"
                :channel="line.channel"
                :kind="line.kind"
                :hop="line.hop"
                :show-index="indexVisible"
              />
            </component>
          </PopoverContent>
        </PopoverPortal>
      </PopoverRoot>
    </li>
  </component>
  <span v-else class="text-fg-muted">{{ $t('common.emptyCell') }}</span>
</template>
