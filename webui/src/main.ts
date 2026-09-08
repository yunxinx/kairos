import { createApp } from 'vue';
import { VueQueryPlugin } from '@tanstack/vue-query';
import { RouterProvider } from '@tanstack/vue-router';
import { queryClient } from '@/app/providers/query';
import { i18n, syncDocumentLocale } from '@/app/providers/i18n';
import { router } from '@/router';
import { initTheme } from '@/lib/theme';
import { hasUnsavedWork, onSessionInvalidated } from '@/lib/session';
import { useToast } from '@/composables/useToast';
import '@/styles/globals.css';

initTheme();
syncDocumentLocale(i18n.global.locale.value);

const toast = useToast();

onSessionInvalidated(() => {
  // 会话失效默认跳登录；但仍有声明的未保存工作（浮窗脏草稿）时不静默
  // 跳转——路由切换会卸载浮窗，草稿随之丢失，交由用户自行处理后再走。
  if (hasUnsavedWork()) {
    // 停在原页面但会话已失效：后续请求都会 401，必须给出可见提示而非
    // 让用户对着不刷新的界面继续编辑。
    toast.error(i18n.global.t('session.expiredWithUnsaved'));
    return;
  }
  if (router.state.location.pathname !== '/login') {
    void router.navigate({ to: '/login' });
  }
});

const app = createApp(RouterProvider, { router });
app.use(i18n);
app.use(VueQueryPlugin, { queryClient });
app.mount('#app');
