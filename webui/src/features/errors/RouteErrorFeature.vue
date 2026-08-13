<script setup lang="ts">
import { computed } from 'vue';
import { Link } from '@tanstack/vue-router';
import type { ErrorComponentProps } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import MarketingSiteHeader from '@/app/shell/MarketingSiteHeader.vue';
import { useShellMode } from '@/composables/useShellMode';
import { hasAdminKey } from '@/lib/session';

const props = defineProps<ErrorComponentProps>();

const { t } = useI18n();
const { showMarketingChrome } = useShellMode();
const homeTarget = computed(() => (hasAdminKey() ? '/overview' : '/login'));

function handleRetry() {
  props.reset();
}
</script>

<template>
  <div
    :class="
      showMarketingChrome
        ? 'dotted-bg flex min-h-0 flex-1 flex-col'
        : 'flex min-h-0 flex-1 flex-col'
    "
  >
    <MarketingSiteHeader />
    <section :class="showMarketingChrome ? 'marketing-main-stage px-4' : 'flex flex-1 flex-col'">
      <div class="not-found-stage">
        <div class="card mx-auto w-full max-w-sm text-center">
          <div class="card-body">
            <p class="font-mono text-5xl font-bold tracking-tight text-[var(--fg-subtle)]">!</p>
            <h1 class="mt-4 font-serif text-2xl font-normal">
              {{ t('errors.routeErrorTitle') }}
            </h1>
            <p class="text-fg-muted mt-2 text-sm">
              {{ t('errors.routeErrorDescription') }}
            </p>
            <div class="mt-8 flex flex-col gap-3 sm:flex-row sm:justify-center">
              <button type="button" class="btn btn-primary" @click="handleRetry">
                {{ t('common.retry') }}
              </button>
              <Link :to="homeTarget" class="btn">
                {{ t('errors.goHome') }}
              </Link>
            </div>
          </div>
        </div>
      </div>
    </section>
  </div>
</template>
