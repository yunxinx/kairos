<script setup lang="ts">
import { onMounted, onUnmounted, useTemplateRef } from 'vue';
import { Link } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import MarketingSiteHeader from '@/app/shell/MarketingSiteHeader.vue';
import '@/styles/home.css';

const { t } = useI18n();

const heroEl = useTemplateRef<HTMLElement>('heroEl');
let revealObserver: IntersectionObserver | undefined;

onMounted(() => {
  revealObserver = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add('is-visible');
          revealObserver?.unobserve(entry.target);
        }
      }
    },
    { threshold: 0.12, rootMargin: '0px 0px -8% 0px' },
  );

  heroEl.value?.querySelectorAll('[data-reveal]').forEach((node, index) => {
    (node as HTMLElement).style.setProperty('--reveal-index', String(index));
    revealObserver?.observe(node);
  });
});

onUnmounted(() => {
  revealObserver?.disconnect();
});
</script>

<template>
  <div class="home-page dotted-bg flex min-h-0 flex-1 flex-col">
    <MarketingSiteHeader />

    <div class="home-main marketing-main-stage max-w-content px-page-x mx-auto flex-col">
      <section ref="heroEl" class="home-hero">
        <p
          class="home-kicker text-fg-muted font-mono text-xs tracking-[0.14em] uppercase"
          data-reveal
        >
          {{ t('home.kicker') }}
        </p>
        <h1 class="home-title font-serif font-normal tracking-tight" data-reveal>
          {{ t('home.headline') }}
        </h1>
        <p class="home-lead text-fg-muted" data-reveal>
          {{ t('home.lead') }}
        </p>
        <div class="home-actions" data-reveal>
          <Link to="/login" class="btn btn-primary">
            {{ t('home.startCta') }}
          </Link>
        </div>
        <ul class="home-anchors text-fg-subtle font-mono text-xs" data-reveal>
          <li>{{ t('home.anchorRun') }}</li>
          <li aria-hidden="true">·</li>
          <li>{{ t('home.anchorRelay') }}</li>
          <li aria-hidden="true">·</li>
          <li>{{ t('home.anchorObserve') }}</li>
        </ul>
      </section>
    </div>
  </div>
</template>
