<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { isProtocol } from '@/api/types';
import BrandIcon from '@/components/ui/BrandIcon.vue';
import { PROTOCOL_BADGE_CLASS, PROTOCOL_ICON_SRC } from '@/lib/protocol';

const props = defineProps<{
  protocol: string;
}>();

const { t } = useI18n();

const known = computed(() => (isProtocol(props.protocol) ? props.protocol : null));

const label = computed(() => (known.value ? t(`protocol.${known.value}`) : props.protocol));

const badgeClass = computed(() =>
  known.value ? PROTOCOL_BADGE_CLASS[known.value] : 'badge-neutral',
);
</script>

<template>
  <span class="badge w-fit gap-1" :class="badgeClass" :title="label">
    <BrandIcon v-if="known" :src="PROTOCOL_ICON_SRC[known]" :size="12" />
    {{ label }}
  </span>
</template>
