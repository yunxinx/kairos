import { createFileRoute, redirect } from '@tanstack/vue-router';
import HomeFeature from '@/features/home/HomeFeature.vue';
import {
  captureSessionGeneration,
  getMe,
  markSessionActive,
  setMeForSession,
} from '@/lib/session';
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
      if (error && typeof error === 'object' && 'to' in error) throw error;
    }
  },
  component: HomeFeature,
});
