<script setup lang="ts">
// 渠道 chip 右侧跟状态标：未登记 / 已停用 / 渠道已失效。渠道已删则只出失效标。
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
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
    <span
      v-if="channelName !== ''"
      class="badge badge-info max-w-[9rem] truncate"
      :data-testid="chipTestId"
      :data-model="channelName"
      :title="channelName"
      >{{ channelName }}</span
    >
    <span
      v-if="statusKey !== undefined"
      class="badge badge-danger"
      data-testid="member-source-status"
      :data-kind="kind"
      >{{ t(statusKey) }}</span
    >
  </span>
</template>
