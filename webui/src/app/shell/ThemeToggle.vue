<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import { useResolvedDarkTheme } from '@/composables/useResolvedDarkTheme';
import { toggleTheme, resolveDark, getStoredTheme } from '@/lib/theme';

const { t } = useI18n();
const isDark = useResolvedDarkTheme();

const themeActionLabel = computed(() => (isDark.value ? t('app.themeLight') : t('app.themeDark')));

function handleToggle() {
  toggleTheme();
  isDark.value = resolveDark(getStoredTheme());
}
</script>

<template>
  <button
    type="button"
    class="icon-btn text-fg-muted p-1.5 transition-colors"
    :title="themeActionLabel"
    :aria-label="themeActionLabel"
    @click="handleToggle"
  >
    <UiIcon v-if="isDark" name="sun" class="h-3.5 w-3.5" :size="14" />
    <UiIcon v-else name="moon" class="h-3.5 w-3.5" :size="14" />
  </button>
</template>
