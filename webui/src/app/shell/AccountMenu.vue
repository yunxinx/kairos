<script setup lang="ts">
import { computed, ref } from 'vue';
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
import { toggleLocale } from '@/app/providers/i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import { useResolvedDarkTheme } from '@/composables/useResolvedDarkTheme';
import { formatUsdMicros } from '@/lib/format';
import { useNavAvatarPreference } from '@/lib/preferences';
import { clearAdminKey, useCurrentUser } from '@/lib/session';
import { getStoredTheme, resolveDark, toggleTheme } from '@/lib/theme';

const { t } = useI18n();
const navigate = useNavigate();
const me = useCurrentUser();
const isDark = useResolvedDarkTheme();
const { showNavAvatar } = useNavAvatarPreference();

const isOpen = ref(false);
let hoverTimer: ReturnType<typeof setTimeout> | undefined;

function handleMouseEnter() {
  if (hoverTimer) {
    clearTimeout(hoverTimer);
    hoverTimer = undefined;
  }
  isOpen.value = true;
}

function handleMouseLeave() {
  hoverTimer = setTimeout(() => {
    isOpen.value = false;
  }, 250);
}

const label = computed(() => {
  const user = me.value;
  if (!user) return t('nav.account');
  return user.display_name.trim() || user.email;
});

const roleLabel = computed(() => {
  const role = me.value?.role;
  if (role === 'root') return t('users.roleRoot');
  if (role === 'admin') return t('users.roleAdmin');
  return t('users.roleUser');
});

const roleBadgeClass = computed(() => {
  const role = me.value?.role;
  if (role === 'root') return 'badge-unified';
  if (role === 'admin') return 'badge-info';
  return 'badge-neutral';
});

function handleToggleTheme() {
  toggleTheme();
  isDark.value = resolveDark(getStoredTheme());
}

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
  <!-- eslint-disable vuejs-accessibility/mouse-events-have-key-events, vuejs-accessibility/no-static-element-interactions -->
  <div
    class="relative inline-flex"
    @mouseenter="handleMouseEnter"
    @mouseleave="handleMouseLeave"
  >
    <DropdownMenuRoot v-model:open="isOpen" :modal="false">
      <DropdownMenuTrigger as-child>
        <button
          type="button"
          class="hover:bg-surface-elevated flex h-8 max-w-64 cursor-pointer items-center gap-2 rounded-full px-2 py-1 text-xs font-medium transition-colors"
          data-testid="account-menu-trigger"
          :title="me ? `${label} (${formatUsdMicros(me.balance_usd_micros)})` : label"
          @pointerdown="(e) => { if (isOpen) e.preventDefault(); }"
          @click="isOpen = true"
        >
          <!-- 头像 -->
          <div
            v-if="showNavAvatar"
            class="border-seed bg-surface-elevated text-fg-muted relative flex size-6 shrink-0 items-center justify-center overflow-hidden rounded-full border text-xs font-bold"
            data-testid="account-menu-avatar"
          >
            <img
              v-if="me?.avatar"
              :src="me.avatar"
              alt="avatar"
              class="size-full object-cover"
            />
            <UiIcon v-else name="user" class="size-3.5" />
          </div>

          <!-- 用户名称 -->
          <span class="text-fg max-w-28 truncate font-medium">{{ label }}</span>

          <!-- 余额 -->
          <span
            v-if="me"
            class="bg-[color-mix(in_srgb,var(--seed-primary)_14%,var(--seed-surface))] border-[color-mix(in_srgb,var(--seed-primary)_30%,transparent)] text-[var(--seed-primary)] rounded-full border px-2 py-0.5 font-mono text-xs font-semibold tracking-tight"
          >
            {{ formatUsdMicros(me.balance_usd_micros) }}
          </span>
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuPortal>
        <DropdownMenuContent
          class="data-table-menu min-w-56"
          align="end"
          :side-offset="6"
          @mouseenter="handleMouseEnter"
          @mouseleave="handleMouseLeave"
        >
          <div class="px-3 py-2.5">
            <div class="flex items-center justify-between gap-1.5">
              <p class="truncate text-sm font-semibold">{{ me?.display_name || t('nav.account') }}</p>
              <span v-if="me" class="badge text-[10px]" :class="roleBadgeClass">
                {{ roleLabel }}
              </span>
            </div>
            <p class="text-fg-muted mt-0.5 truncate font-mono text-xs">{{ me?.email }}</p>
          </div>
          <DropdownMenuSeparator class="data-table-menu-separator" />
          <DropdownMenuItem
            class="data-table-menu-item flex items-center gap-2"
            data-testid="nav-account"
            @select="() => navigate({ to: '/account' })"
          >
            <UiIcon name="user" class="text-fg-muted size-3.5" />
            <span>{{ t('nav.account') }}</span>
          </DropdownMenuItem>
          <DropdownMenuItem
            class="data-table-menu-item flex items-center justify-between"
            data-testid="nav-theme-toggle"
            @select="handleToggleTheme"
          >
            <div class="flex items-center gap-2">
              <UiIcon :name="isDark ? 'sun' : 'moon'" class="text-fg-muted size-3.5" />
              <span>{{ t('app.theme') }}</span>
            </div>
            <span class="text-fg-muted font-mono text-xs">
              {{ isDark ? t('app.themeDark') : t('app.themeLight') }}
            </span>
          </DropdownMenuItem>
          <DropdownMenuItem
            class="data-table-menu-item flex items-center justify-between"
            data-testid="nav-locale-toggle"
            @select="toggleLocale()"
          >
            <div class="flex items-center gap-2">
              <UiIcon name="globe" class="text-fg-muted size-3.5" />
              <span>{{ t('app.language') }}</span>
            </div>
            <span class="text-fg-muted font-mono text-xs">{{ t('app.localeToggle') }}</span>
          </DropdownMenuItem>
          <DropdownMenuSeparator class="data-table-menu-separator" />
          <DropdownMenuItem
            class="data-table-menu-item text-danger focus:text-danger flex items-center gap-2"
            data-testid="nav-logout"
            @select="handleLogout"
          >
            <UiIcon name="log-out" class="size-3.5" />
            <span>{{ t('nav.logout') }}</span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenuPortal>
    </DropdownMenuRoot>
  </div>
</template>
