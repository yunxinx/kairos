<script setup lang="ts">
import { Link } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import LocaleToggle from '@/app/shell/LocaleToggle.vue';
import ThemeToggle from '@/app/shell/ThemeToggle.vue';
import { useShellMode } from '@/composables/useShellMode';

const { t } = useI18n();
const { showMarketingChrome } = useShellMode();
</script>

<template>
  <header
    v-if="showMarketingChrome"
    class="site-header max-w-content px-page-x h-topnav mx-auto flex w-full items-center justify-between"
  >
    <Link to="/" class="site-brand font-serif text-lg tracking-tight">{{ t('app.title') }}</Link>
    <div class="flex items-center gap-3">
      <LocaleToggle />
      <ThemeToggle />
    </div>
  </header>
</template>
