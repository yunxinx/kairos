import { createFileRoute } from '@tanstack/vue-router';
import UsersFeature from '@/features/users/UsersFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/admin/users')({
  beforeLoad: requireRole('admin'),
  component: UsersFeature,
  staticData: { titleKey: 'nav.users' },
});
