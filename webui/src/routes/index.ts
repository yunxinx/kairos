import { createFileRoute, isRedirect, redirect } from '@tanstack/vue-router';
import HomeFeature from '@/features/home/HomeFeature.vue';
import { captureSessionGeneration, getMe, markSessionActive, setMeForSession } from '@/lib/session';
import { apiClient } from '@/api/client';

export const Route = createFileRoute('/')({
  beforeLoad: async () => {
    if (getMe()) {
      throw redirect({ to: '/overview' });
    }
    try {
      const user = await apiClient.getMe();
      markSessionActive();
      setMeForSession(user, captureSessionGeneration());
      throw redirect({ to: '/overview' });
    } catch (error) {
      // redirect() 返回挂 options 的 Response 对象，须用官方判别式重抛，
      // 否则已登录访问根路径会被当普通错误吞掉、停在营销页。
      if (isRedirect(error)) throw error;
    }
  },
  component: HomeFeature,
});
