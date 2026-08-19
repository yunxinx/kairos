<script setup lang="ts">
// 表格单元格：普通名是「模型名 + 来源渠道」；统一模型先出身份徽章，再展开成员与状态。超出条数收进 +N。
import { computed } from 'vue';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import type { ChannelView } from '@/api/types';
import OverflowChips, { type OverflowChip } from '@/components/ui/OverflowChips.vue';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import UnifiedJumpOrder from '@/features/models/UnifiedJumpOrder.vue';
import UnifiedNameChip from '@/features/models/UnifiedNameChip.vue';
import {
  memberSourceI18nKey,
  type CallableSourceLine,
  type SourceChannel,
} from '@/lib/unified-sources';

const VISIBLE_COUNT = 3;

const props = withDefaults(
  defineProps<{
    lines: CallableSourceLine[];
    channels: ChannelView[];
    chipTestId?: string;
  }>(),
  { chipTestId: 'group-source-channel' },
);

const { t } = useI18n();

const visible = computed(() => props.lines.slice(0, VISIBLE_COUNT));
const hidden = computed(() => props.lines.slice(VISIBLE_COUNT));
const hiddenCount = computed(() => hidden.value.length);

function ordinaryChips(channels: SourceChannel[]): OverflowChip[] {
  return channels.map((channel) => {
    const key = memberSourceI18nKey(channel.kind);
    if (key === undefined) return { name: channel.name };
    return { name: channel.name, aside: t(key), asideKind: channel.kind };
  });
}
</script>

<template>
  <div v-if="lines.length > 0" class="flex flex-col gap-1" data-testid="model-source-lines">
    <div
      v-for="line in visible"
      :key="line.name"
      class="flex flex-wrap items-center gap-2"
      data-testid="group-member-line"
      :data-model="line.name"
      :data-unified="line.isUnified ? 'true' : undefined"
    >
      <template v-if="line.isUnified">
        <div class="flex min-w-0 flex-col gap-1">
          <UnifiedNameChip :name="line.name" />
          <UnifiedJumpOrder :members="line.unifiedMembers" :channels="channels" hide-index />
        </div>
      </template>
      <template v-else>
        <span class="font-mono text-sm">{{ line.name }}</span>
        <OverflowChips
          v-if="line.channels.length > 0"
          :items="ordinaryChips(line.channels)"
          :chip-test-id="chipTestId"
        />
        <ChannelSourceMark v-else kind="unlisted" />
      </template>
    </div>
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
          <div class="flex flex-col gap-2 p-1">
            <div
              v-for="line in hidden"
              :key="line.name"
              class="flex flex-wrap items-center gap-2"
              data-testid="group-member-line"
              :data-model="line.name"
              :data-unified="line.isUnified ? 'true' : undefined"
            >
              <template v-if="line.isUnified">
                <div class="flex min-w-0 flex-col gap-1">
                  <UnifiedNameChip :name="line.name" />
                  <UnifiedJumpOrder
                    :members="line.unifiedMembers"
                    :channels="channels"
                    hide-index
                  />
                </div>
              </template>
              <template v-else>
                <span class="font-mono text-sm">{{ line.name }}</span>
                <OverflowChips
                  v-if="line.channels.length > 0"
                  :items="ordinaryChips(line.channels)"
                  :chip-test-id="chipTestId"
                />
                <ChannelSourceMark v-else kind="unlisted" />
              </template>
            </div>
          </div>
        </PopoverContent>
      </PopoverPortal>
    </PopoverRoot>
  </div>
  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
</template>
