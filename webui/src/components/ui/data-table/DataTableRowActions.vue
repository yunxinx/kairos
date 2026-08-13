<script setup lang="ts">
import {
  DropdownMenuContent,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuTrigger,
} from 'reka-ui';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';

withDefaults(
  defineProps<{
    triggerTestId?: string;
  }>(),
  {
    triggerTestId: 'row-actions',
  },
);

const { t } = useI18n();
</script>

<template>
  <DropdownMenuRoot :modal="false">
    <DropdownMenuTrigger as-child>
      <button
        type="button"
        class="btn btn-ghost btn-icon data-[state=open]:bg-[var(--seed-surface-alt)]"
        :data-testid="triggerTestId"
        :aria-label="t('common.openRowActions')"
      >
        <UiIcon name="more-horizontal" :size="16" />
      </button>
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent class="data-table-menu" align="end" :side-offset="4">
        <slot />
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
