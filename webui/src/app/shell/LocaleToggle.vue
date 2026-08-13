<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { toggleLocale } from '@/app/providers/i18n';

const { t } = useI18n();

const localeActionLabel = computed(() => t('app.localeToggle'));
</script>

<template>
  <button
    type="button"
    class="icon-btn text-fg-muted cursor-pointer text-xs"
    :title="localeActionLabel"
    :aria-label="localeActionLabel"
    @click="toggleLocale()"
  >
    {{ localeActionLabel }}
  </button>
</template>
