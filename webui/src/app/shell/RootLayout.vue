<script setup lang="ts">
import { Outlet } from '@tanstack/vue-router';
import AppShell from '@/app/shell/AppShell.vue';
import NavBar from '@/app/shell/NavBar.vue';
import MobileNav from '@/app/shell/MobileNav.vue';
import { useDocumentTitle } from '@/composables/useDocumentTitle';
import { useShellMode } from '@/composables/useShellMode';

useDocumentTitle();

const { showAdminNav, suppressPageContent, showPublicFooter } = useShellMode();
</script>

<template>
  <AppShell
    :hide-nav="!showAdminNav"
    :show-public-footer="showPublicFooter"
    :suppress-content="suppressPageContent"
  >
    <template #navbar>
      <NavBar />
    </template>
    <template #mobile-nav>
      <MobileNav />
    </template>
    <Outlet />
  </AppShell>
</template>
