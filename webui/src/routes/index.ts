import { createFileRoute, redirect } from '@tanstack/vue-router';
import HomeFeature from '@/features/home/HomeFeature.vue';
import { getAdminKey } from '@/lib/session';

export const Route = createFileRoute('/')({
  beforeLoad: () => {
    if (getAdminKey()) {
      throw redirect({ to: '/overview' });
    }
  },
  component: HomeFeature,
});
