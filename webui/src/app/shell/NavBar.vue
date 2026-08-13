<script setup lang="ts">
import { computed } from 'vue';
import { Link, useNavigate } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import ThemeToggle from '@/app/shell/ThemeToggle.vue';
import LocaleToggle from '@/app/shell/LocaleToggle.vue';
import { NAV_TABS } from '@/lib/nav';
import { prefetchAdminRoute } from '@/lib/prefetch-admin';
import { clearAdminKey, hasAdminKey } from '@/lib/session';

const { t } = useI18n();
const navigate = useNavigate();

const tabs = computed(() => (hasAdminKey() ? NAV_TABS : []));

async function handleLogout() {
  clearAdminKey();
  await navigate({ to: '/login' });
}
</script>

<template>
  <nav data-component="Navigation" class="nav-bar z-topnav sticky top-0">
    <div class="max-w-content h-topnav px-page-x mx-auto flex items-center justify-between">
      <div class="flex items-center gap-0 overflow-x-auto">
        <Link
          v-for="tab in tabs"
          :key="tab.to"
          :to="tab.to"
          class="tab-link"
          :activeProps="{ class: 'router-link-active' }"
          @pointerenter="prefetchAdminRoute(tab.to)"
          @focus="prefetchAdminRoute(tab.to)"
        >
          {{ t(tab.labelKey) }}
        </Link>
      </div>
      <div class="flex shrink-0 items-center gap-3">
        <LocaleToggle />
        <ThemeToggle />
        <button
          type="button"
          class="icon-btn text-fg-muted cursor-pointer text-xs"
          @click="handleLogout"
        >
          {{ t('nav.logout') }}
        </button>
      </div>
    </div>
  </nav>
</template>
