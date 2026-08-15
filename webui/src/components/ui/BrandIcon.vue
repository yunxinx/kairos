<script setup lang="ts">
// 品牌 logo 图标：以 @lobehub/icons-static-svg 的静态 SVG 作蒙版，
// 用 currentColor 着色，随所在徽章/文字的颜色自适应明暗主题。
// mask 经行内样式直写而非 CSS 变量中转：构建内联的 data URI 含裸单引号，
// 无引号的 url() 不接受裸引号，须先编码为 %27，否则整条声明被解析器丢弃。
import { computed } from 'vue';

const props = withDefaults(
  defineProps<{
    /** 静态 SVG 资源 URL（mono 变体）。 */
    src: string;
    size?: number;
  }>(),
  { size: 16 },
);

const sizePx = computed(() => `${props.size}px`);
const maskUrl = computed(() => `url(${props.src.replaceAll("'", '%27')})`);
</script>

<template>
  <span
    aria-hidden="true"
    class="brand-icon"
    :style="{
      width: sizePx,
      height: sizePx,
      WebkitMaskImage: maskUrl,
      maskImage: maskUrl,
    }"
  />
</template>

<style scoped>
.brand-icon {
  display: inline-block;
  flex: none;
  background-color: currentColor;
  -webkit-mask-position: center;
  mask-position: center;
  -webkit-mask-repeat: no-repeat;
  mask-repeat: no-repeat;
  -webkit-mask-size: contain;
  mask-size: contain;
}
</style>
