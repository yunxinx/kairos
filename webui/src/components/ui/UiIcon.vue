<script setup lang="ts">
import { computed } from 'vue';
import { iconDefs, type IconName } from '@/components/ui/icon-paths';

const props = withDefaults(
  defineProps<{
    name: IconName;
    size?: number | string;
  }>(),
  {
    size: 24,
  },
);

defineOptions({
  inheritAttrs: false,
});

const def = computed(() => iconDefs[props.name]);

const sizePx = computed(() => (typeof props.size === 'number' ? `${props.size}px` : props.size));

const resolvedStrokeWidth = computed(() => def.value.strokeWidth ?? 2);
</script>

<template>
  <svg
    v-bind="$attrs"
    :width="sizePx"
    :height="sizePx"
    :viewBox="def.viewBox ?? '0 0 24 24'"
    fill="none"
    stroke="currentColor"
    :stroke-width="resolvedStrokeWidth"
    :stroke-linecap="def.strokeLinecap"
    :stroke-linejoin="def.strokeLinejoin"
    aria-hidden="true"
  >
    <path v-for="(d, index) in def.paths ?? []" :key="`path-${index}`" :d="d" />
    <circle
      v-for="(circle, index) in def.circles ?? []"
      :key="`circle-${index}`"
      :cx="circle.cx"
      :cy="circle.cy"
      :r="circle.r"
    />
    <line
      v-for="(line, index) in def.lines ?? []"
      :key="`line-${index}`"
      :x1="line.x1"
      :y1="line.y1"
      :x2="line.x2"
      :y2="line.y2"
    />
  </svg>
</template>
