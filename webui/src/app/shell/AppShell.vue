<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import MarketingSiteFooter from '@/app/shell/MarketingSiteFooter.vue';

defineProps<{
  hideNav?: boolean;
  showPublicFooter?: boolean;
  suppressContent?: boolean;
}>();

const { t } = useI18n();
</script>

<template>
  <div
    data-slot="app-shell"
    class="app-shell relative flex flex-col"
    :class="hideNav ? 'min-h-dvh' : 'h-dvh max-h-dvh overflow-hidden'"
  >
    <a href="#main-content" class="skip-link">{{ t('a11y.skipToContent') }}</a>
    <template v-if="!hideNav">
      <div class="hidden md:block">
        <slot name="navbar" />
      </div>
    </template>
    <main
      id="main-content"
      data-component="PageContent"
      tabindex="-1"
      class="flex min-h-0 flex-1 flex-col"
      :class="[{ 'dotted-bg': !hideNav }, hideNav ? '' : 'overflow-hidden']"
    >
      <div
        :class="[
          hideNav
            ? 'flex min-h-0 flex-1 flex-col'
            : 'max-w-content px-page-x py-page-y mx-auto flex min-h-0 w-full flex-1 flex-col',
          suppressContent ? 'pointer-events-none opacity-0 transition-opacity duration-150' : '',
        ]"
      >
        <slot />
      </div>
      <MarketingSiteFooter v-if="showPublicFooter" />
    </main>
    <slot v-if="!hideNav" name="mobile-nav" />
  </div>
</template>
