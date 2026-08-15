<script setup lang="ts">
// 悬浮提示：text 为空时仅透传插槽，等价于无包裹，便于按条件复用。
import {
  TooltipArrow,
  TooltipContent,
  TooltipPortal,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from 'reka-ui';

withDefaults(
  defineProps<{
    /** 提示文案；为空则不渲染提示，仅透传插槽。 */
    text?: string;
    side?: 'top' | 'right' | 'bottom' | 'left';
    align?: 'start' | 'center' | 'end';
  }>(),
  { text: '', side: 'top', align: 'center' },
);
</script>

<template>
  <slot v-if="text === ''" />
  <TooltipProvider v-else :delay-duration="200" :skip-delay-duration="0">
    <TooltipRoot>
      <TooltipTrigger as-child>
        <slot />
      </TooltipTrigger>
      <TooltipPortal>
        <TooltipContent :side="side" :align="align" :side-offset="6" class="tooltip-content">
          {{ text }}
          <TooltipArrow class="tooltip-arrow" />
        </TooltipContent>
      </TooltipPortal>
    </TooltipRoot>
  </TooltipProvider>
</template>
