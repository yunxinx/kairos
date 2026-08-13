<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue';
import { Link, useNavigate } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import { toggleLocale } from '@/app/providers/i18n';
import { getStoredTheme, resolveDark, toggleTheme } from '@/lib/theme';
import { NAV_TABS } from '@/lib/nav';
import { clearAdminKey, hasAdminKey } from '@/lib/session';
import { useResolvedDarkTheme } from '@/composables/useResolvedDarkTheme';

const { t } = useI18n();
const navigate = useNavigate();
const menuOpen = ref(false);
const peek = ref(false);
const isDark = useResolvedDarkTheme();
const fabNavEl = ref<HTMLElement | null>(null);

const tabs = computed(() => (hasAdminKey() ? NAV_TABS : []));

/** 与桌面 NavBar 左→右顺序对应，移动端面板内为下→上（概览贴近 FAB）。 */
const tabsBottomUp = computed(() => [...tabs.value].reverse());

const themeActionLabel = computed(() =>
  resolveDark(getStoredTheme()) ? t('app.themeLight') : t('app.themeDark'),
);

function handleScroll() {
  peek.value = window.scrollY > 80;
}

function toggle() {
  menuOpen.value = !menuOpen.value;
  if (menuOpen.value) peek.value = false;
}

function closeMenu() {
  menuOpen.value = false;
}

/** 打开菜单时将当前路由对应项滚入可视区（列表自下而上，默认 scrollTop=0 会藏住底部选中项）。 */
function scrollActiveTabIntoView() {
  const nav = fabNavEl.value;
  if (!nav) return;
  const active = nav.querySelector<HTMLElement>('.router-link-exact-active');
  active?.scrollIntoView({ block: 'end', inline: 'nearest' });
}

watch(menuOpen, (open) => {
  if (!open) return;
  void nextTick(scrollActiveTabIntoView);
});

async function handleLogout() {
  closeMenu();
  clearAdminKey();
  await navigate({ to: '/login' });
}

function handleThemeToggle() {
  toggleTheme();
  isDark.value = resolveDark(getStoredTheme());
  closeMenu();
}

function handleLocaleToggle() {
  toggleLocale();
  closeMenu();
}

onMounted(() => {
  window.addEventListener('scroll', handleScroll, { passive: true });
});
onUnmounted(() => window.removeEventListener('scroll', handleScroll));
</script>

<template>
  <div class="md:hidden">
    <Transition name="fab-fade">
      <button
        v-if="menuOpen"
        type="button"
        class="fab-backdrop z-overlay"
        :aria-label="t('common.close')"
        @click="closeMenu"
      />
    </Transition>

    <div class="fab-wrap z-floating" :class="{ 'fab-peek': peek && !menuOpen }">
      <Transition name="fab-menu">
        <div v-if="menuOpen" class="fab-panel card">
          <div class="fab-utilities">
            <button
              type="button"
              class="fab-utility-btn"
              :aria-label="t('app.localeToggle')"
              :title="t('app.localeToggle')"
              @click="handleLocaleToggle"
            >
              <UiIcon name="globe" class="fab-utility-icon" :size="16" />
            </button>
            <button
              type="button"
              class="fab-utility-btn"
              :aria-label="themeActionLabel"
              :title="themeActionLabel"
              @click="handleThemeToggle"
            >
              <UiIcon :name="isDark ? 'sun' : 'moon'" class="fab-utility-icon" :size="16" />
            </button>
            <button
              type="button"
              class="fab-utility-btn"
              :aria-label="t('nav.logout')"
              :title="t('nav.logout')"
              @click="handleLogout"
            >
              <UiIcon name="log-out" class="fab-utility-icon" :size="16" />
            </button>
          </div>
          <div class="fab-sep" />
          <nav ref="fabNavEl" class="fab-nav" :aria-label="t('common.navMenu')">
            <Link
              v-for="tab in tabsBottomUp"
              :key="tab.to"
              :to="tab.to"
              class="fab-link"
              :activeProps="{ class: 'router-link-exact-active' }"
              @click="closeMenu"
            >
              {{ t(tab.labelKey) }}
            </Link>
          </nav>
        </div>
      </Transition>

      <button
        type="button"
        class="fab-btn"
        :class="{ 'fab-btn-active': menuOpen }"
        :aria-label="t('common.navMenu')"
        @click.stop="toggle"
      >
        <UiIcon v-if="!menuOpen" name="menu" class="fab-icon" :size="14" />
        <UiIcon v-else name="close" class="fab-icon" :size="14" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.fab-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(10, 14, 28, 0.35);
  border: none;
  padding: 0;
  cursor: default;
}
.fab-fade-enter-active,
.fab-fade-leave-active {
  transition: opacity 200ms ease;
}
.fab-fade-enter-from,
.fab-fade-leave-to {
  opacity: 0;
}
.fab-wrap {
  position: fixed;
  right: 16px;
  bottom: max(24px, env(safe-area-inset-bottom, 0px));
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 8px;
  transition: transform 350ms cubic-bezier(0.4, 0, 0.2, 1);
}
.fab-wrap.fab-peek {
  transform: translateX(24px);
}
.fab-btn {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  background: var(--seed-primary);
  color: var(--fg-on-primary);
  border: 2px solid var(--seed-primary);
  box-shadow: 1px 2px 0 0 var(--primary-shadow);
  transition:
    color 200ms ease,
    background 200ms ease,
    border-color 200ms ease,
    box-shadow 200ms ease,
    transform 200ms ease;
  flex-shrink: 0;
}
.fab-btn:focus-visible {
  outline: none;
  box-shadow:
    1px 2px 0 0 var(--primary-shadow),
    var(--focus-ring);
}
.fab-btn-active:focus-visible {
  box-shadow: var(--focus-ring);
}
.fab-link:focus-visible,
.fab-utility-btn:focus-visible {
  outline: none;
  box-shadow: inset var(--focus-ring);
}
.fab-backdrop:focus-visible {
  outline: none;
}
.fab-btn-active {
  background: var(--seed-surface);
  color: var(--seed-fg);
  border-color: var(--seed-border);
  box-shadow: 1px 2px 0 0 var(--card-shadow);
}
.fab-icon {
  width: 14px;
  height: 14px;
}
.fab-panel {
  display: flex;
  flex-direction: column;
  min-width: 168px;
  max-width: min(240px, calc(100vw - 32px));
  max-height: min(70dvh, calc(100dvh - 120px - env(safe-area-inset-bottom, 0px)));
  padding: 4px 0;
  overflow: hidden;
  transform-origin: bottom right;
}
.fab-utilities {
  display: flex;
  flex-shrink: 0;
  padding: 4px 6px;
  gap: 2px;
}
.fab-utility-btn {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  min-width: 0;
  padding: 8px 6px;
  color: var(--fg-muted);
  background: none;
  border: none;
  border-radius: calc(var(--seed-radius) - 2px);
  cursor: pointer;
  transition:
    color 150ms ease-in-out,
    background 150ms ease-in-out;
}
.fab-utility-btn:hover {
  color: var(--seed-fg);
  background: var(--seed-surface-alt);
}
.fab-utility-icon {
  flex-shrink: 0;
}
.fab-nav {
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  -webkit-overflow-scrolling: touch;
}
.fab-link {
  display: block;
  flex-shrink: 0;
  padding: 10px 16px;
  font-size: 13px;
  font-weight: 500;
  color: var(--fg-muted);
  text-decoration: none;
}
.fab-link:hover {
  color: var(--seed-fg);
  background: var(--seed-surface-alt);
}
.fab-link.router-link-exact-active {
  color: var(--seed-fg);
  font-weight: 600;
  background: var(--seed-surface-alt);
  box-shadow: inset 3px 0 0 var(--seed-primary);
}
.fab-sep {
  flex-shrink: 0;
  height: 1px;
  margin: 4px 12px;
  background: var(--seed-border);
}
.fab-menu-enter-active {
  transition:
    opacity 200ms ease,
    transform 200ms cubic-bezier(0.16, 1, 0.3, 1);
}
.fab-menu-leave-active {
  transition:
    opacity 150ms ease-in,
    transform 150ms ease-in;
}
.fab-menu-enter-from,
.fab-menu-leave-to {
  opacity: 0;
  transform: translateY(8px) scale(0.98);
}
</style>
