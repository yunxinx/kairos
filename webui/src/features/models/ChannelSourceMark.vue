<script setup lang="ts">
// 渠道 chip 右侧跟状态标：未登记 / 已停用 / 渠道已失效。渠道已删则只出失效标。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import Tooltip from '@/components/ui/Tooltip.vue';
import { memberSourceI18nKey, type MemberSourceKind } from '@/lib/unified-sources';

const props = withDefaults(
  defineProps<{
    kind: MemberSourceKind;
    channelName?: string;
    chipTestId?: string;
  }>(),
  { channelName: '', chipTestId: 'unified-source-channel' },
);

const { t } = useI18n();
const statusKey = computed(() => memberSourceI18nKey(props.kind));
</script>

<template>
  <span class="inline-flex max-w-full items-center gap-1">
    <!--
      名称可收缩、状态标不可：`truncate` 带 `white-space: nowrap`，若不给 `min-w-0`，
      flex 项的自动最小尺寸就是整段文字宽度，于是名称拒绝收缩、把「已停用」挤到下一行。
    -->
    <Tooltip v-if="channelName !== ''" :text="channelName">
      <span
        class="badge badge-info max-w-[9rem] min-w-0 shrink truncate"
        :data-testid="chipTestId"
        :data-model="channelName"
        >{{ channelName }}</span
      >
    </Tooltip>
    <span
      v-if="statusKey !== undefined"
      class="badge badge-danger shrink-0 whitespace-nowrap"
      data-testid="member-source-status"
      :data-kind="kind"
      >{{ t(statusKey) }}</span
    >
  </span>
</template>
