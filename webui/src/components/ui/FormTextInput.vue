<script setup lang="ts">
defineOptions({
  inheritAttrs: false,
});

defineProps<{
  id: string;
  invalid?: boolean;
  /** 校验失败时关联 `FormField` 气泡的 id。 */
  hintId?: string | undefined;
}>();

/** 文本类原生 input 的双向绑定；number 字段可用 `string | number`。 */
const model = defineModel<string | number | null | undefined>();
</script>

<template>
  <!--
    直接透传 $attrs：`useAttrs()` 返回的对象本身不是 reactive，放进 computed 无法追踪
    父级运行时切换 disabled/class/aria-* 等动态 fallthrough（Vue 官方文档明确说明）。
    模板里的 $attrs 在每次渲染时读取最新值，跨渲染响应；class/style 与组件自身绑定自动合并。
  -->
  <input
    v-bind="$attrs"
    :id="id"
    v-model="model"
    class="input w-full"
    :class="{ 'input-invalid': invalid }"
    :aria-invalid="invalid ? 'true' : undefined"
    :aria-describedby="invalid && hintId ? hintId : undefined"
  />
</template>
