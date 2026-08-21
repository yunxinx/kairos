<script setup lang="ts">
import { computed } from 'vue';
import { useNavigate } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import {
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuRoot,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from 'reka-ui';
import { apiClient } from '@/api/client';
import { clearAdminKey, useCurrentUser } from '@/lib/session';

const { t } = useI18n();
const navigate = useNavigate();
const me = useCurrentUser();

const label = computed(() => {
  const user = me.value;
  if (!user) return t('nav.account');
  return user.display_name.trim() || user.email;
});

async function handleLogout() {
  try {
    await apiClient.logout();
  } catch {
    // 会话可能已失效；本地清凭据即可。
  }
  clearAdminKey();
  await navigate({ to: '/login' });
}
</script>

<template>
  <DropdownMenuRoot :modal="false">
    <DropdownMenuTrigger as-child>
      <button
        type="button"
        class="icon-btn text-fg-muted max-w-40 cursor-pointer truncate text-xs"
        data-testid="account-menu-trigger"
        :title="me?.email"
      >
        {{ label }}
      </button>
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent class="data-table-menu" align="end" :side-offset="4">
        <div class="data-table-menu-label px-2 py-1.5">
          <p class="truncate text-sm font-medium">{{ me?.display_name }}</p>
          <p class="text-fg-muted truncate font-mono text-xs">{{ me?.email }}</p>
        </div>
        <DropdownMenuSeparator class="data-table-menu-separator" />
        <DropdownMenuItem
          class="data-table-menu-item"
          data-testid="nav-account"
          @select="() => navigate({ to: '/account' })"
        >
          {{ t('nav.account') }}
        </DropdownMenuItem>
        <DropdownMenuItem
          class="data-table-menu-item"
          data-testid="nav-logout"
          @select="handleLogout"
        >
          {{ t('nav.logout') }}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
