import { createFileRoute, redirect } from '@tanstack/vue-router';
import { requireAuth } from '@/lib/router-guards';

/** 旧「价格」路径改到模型页。 */
export const Route = createFileRoute('/pricing')({
  beforeLoad: async () => {
    await requireAuth();
    throw redirect({ to: '/models' });
  },
});
