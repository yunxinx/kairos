import { createApp } from 'vue';
import { VueQueryPlugin } from '@tanstack/vue-query';
import { RouterProvider } from '@tanstack/vue-router';
import { queryClient } from '@/app/providers/query';
import { i18n, syncDocumentLocale } from '@/app/providers/i18n';
import { router } from '@/router';
import { initTheme } from '@/lib/theme';
import { onSessionInvalidated } from '@/lib/session';
import '@/styles/tokens.css';

initTheme();
syncDocumentLocale(i18n.global.locale.value);

onSessionInvalidated(() => {
  if (router.state.location.pathname !== '/login') {
    void router.navigate({ to: '/login' });
  }
});

const app = createApp(RouterProvider, { router });
app.use(i18n);
app.use(VueQueryPlugin, { queryClient });
app.mount('#app');
