<script setup lang="ts">
// 表格「模型」列：点击复制名称；悬停描边表示可点。
import { onUnmounted, ref } from 'vue';
import { useI18n } from 'vue-i18n';

const COPIED_HINT_MS = 1_500;

const props = defineProps<{
  text: string;
  testId?: string;
}>();

const { t } = useI18n();
const copied = ref(false);
let copiedTimer: ReturnType<typeof setTimeout> | undefined;

async function copy() {
  try {
    await navigator.clipboard.writeText(props.text);
  } catch {
    return;
  }
  copied.value = true;
  if (copiedTimer !== undefined) clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => {
    copied.value = false;
  }, COPIED_HINT_MS);
}

onUnmounted(() => {
  if (copiedTimer !== undefined) clearTimeout(copiedTimer);
});
</script>

<template>
  <button
    type="button"
    class="copyable-name"
    :data-testid="testId"
    :title="copied ? t('common.copied') : t('common.copy')"
    @click.stop="copy"
  >
    {{ text }}
  </button>
</template>
