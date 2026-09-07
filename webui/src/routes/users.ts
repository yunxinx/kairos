import { createFileRoute } from '@tanstack/vue-router';
import UsersFeature from '@/features/users/UsersFeature.vue';
import { requireCapability } from '@/lib/router-guards';

export const Route = createFileRoute('/users')({
  beforeLoad: requireCapability('manage_users'),
  validateSearch: (
    search: Record<string, unknown>,
  ): {
    q?: string | undefined;
    role?: string | undefined;
    status?: string | undefined;
    plan?: string | undefined;
  } => ({
    q: typeof search.q === 'string' ? search.q : undefined,
    role: typeof search.role === 'string' ? search.role : undefined,
    status: typeof search.status === 'string' ? search.status : undefined,
    plan: typeof search.plan === 'string' ? search.plan : undefined,
  }),
  component: UsersFeature,
  staticData: { titleKey: 'nav.users' },
});
