import { createFileRoute } from '@tanstack/vue-router';
import UsersFeature from '@/features/users/UsersFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/users')({
  beforeLoad: requireRole('admin'),
  component: UsersFeature,
  staticData: { titleKey: 'nav.users' },
});
