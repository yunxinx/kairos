<script setup lang="ts">
defineOptions({
  inheritAttrs: false,
});

defineProps<{
  id: string;
}>();

const model = defineModel<boolean>({ required: true });
</script>

<template>
  <!-- 直接透传 $attrs，跨渲染响应父级动态属性；class/style 与组件自身绑定自动合并。 -->
  <input v-bind="$attrs" :id="id" v-model="model" type="checkbox" />
</template>
