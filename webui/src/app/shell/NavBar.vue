<script setup lang="ts">
import { computed } from 'vue';
import { Link } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import AccountMenu from '@/app/shell/AccountMenu.vue';
import { navTabsFor } from '@/lib/nav';
import { prefetchAdminRoute } from '@/lib/prefetch-admin';
import { useCurrentUser } from '@/lib/session';

const { t } = useI18n();
const me = useCurrentUser();

const tabs = computed(() => navTabsFor(me.value?.role));
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
      <div class="flex shrink-0 items-center">
        <AccountMenu />
      </div>
    </div>
  </nav>
</template>
