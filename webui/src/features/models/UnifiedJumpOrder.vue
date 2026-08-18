<script setup lang="ts">
import { computed } from 'vue';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import type { ChannelView, UnifiedMember } from '@/api/types';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import { channelNameForMember, unifiedMemberKey } from '@/lib/unified-sources';

const VISIBLE_COUNT = 3;

const props = defineProps<{
  members: UnifiedMember[];
  channels: ChannelView[];
}>();

const { t } = useI18n();

const lines = computed(() =>
  props.members.map((member) => ({
    key: unifiedMemberKey(member),
    member: member.model,
    channel: channelNameForMember(props.channels, member),
    available: member.available !== false,
  })),
);

const visible = computed(() => lines.value.slice(0, VISIBLE_COUNT));
const hidden = computed(() => lines.value.slice(VISIBLE_COUNT));
const hiddenCount = computed(() => hidden.value.length);
</script>

<template>
  <div v-if="lines.length > 0" class="flex flex-col gap-1" data-testid="unified-members">
    <ol class="m-0 flex list-none flex-col gap-1 p-0">
      <li
        v-for="(line, index) in visible"
        :key="line.key"
        class="flex flex-wrap items-center gap-2"
        data-testid="unified-member-line"
        :data-member="line.member"
        :data-channel="line.channel"
      >
        <span class="text-fg-muted font-mono text-xs" data-testid="unified-member-index">{{
          index + 1
        }}</span>
        <span class="font-mono text-sm">{{ line.member }}</span>
        <span
          v-if="!line.available"
          class="badge badge-warning"
          data-testid="unified-member-unavailable"
        >
          {{ t('models.unifiedMemberUnavailable') }}
        </span>
        <OverflowChips
          v-if="line.channel"
          :items="[line.channel]"
          chip-test-id="unified-source-channel"
        />
      </li>
    </ol>
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
          <ol class="m-0 flex list-none flex-col gap-2 p-1">
            <li
              v-for="(line, offset) in hidden"
              :key="line.key"
              class="flex flex-wrap items-center gap-2"
              data-testid="unified-member-line"
              :data-member="line.member"
              :data-channel="line.channel"
            >
              <span class="text-fg-muted font-mono text-xs">{{ VISIBLE_COUNT + offset + 1 }}</span>
              <span class="font-mono text-sm">{{ line.member }}</span>
              <span
                v-if="!line.available"
                class="badge badge-warning"
                data-testid="unified-member-unavailable"
              >
                {{ t('models.unifiedMemberUnavailable') }}
              </span>
              <OverflowChips
                v-if="line.channel"
                :items="[line.channel]"
                chip-test-id="unified-source-channel"
              />
            </li>
          </ol>
        </PopoverContent>
      </PopoverPortal>
    </PopoverRoot>
  </div>
  <span v-else class="text-fg-muted">{{ $t('common.emptyCell') }}</span>
</template>
