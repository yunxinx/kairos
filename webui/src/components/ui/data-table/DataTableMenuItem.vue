<script setup lang="ts">
import { DropdownMenuItem } from 'reka-ui';
import { cn } from '@/lib/cn';

withDefaults(
  defineProps<{
    danger?: boolean;
    disabled?: boolean;
  }>(),
  {
    danger: false,
    disabled: false,
  },
);

const emit = defineEmits<{
  select: [event: Event];
}>();
</script>

<template>
  <DropdownMenuItem
    :disabled="disabled"
    :class="cn('data-table-menu-item', danger && 'data-table-menu-item-danger')"
    @select="emit('select', $event)"
  >
    <slot />
  </DropdownMenuItem>
</template>
