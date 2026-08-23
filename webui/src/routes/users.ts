import { createFileRoute } from '@tanstack/vue-router';
import UsersFeature from '@/features/users/UsersFeature.vue';
import { requireCapability } from '@/lib/router-guards';

export const Route = createFileRoute('/users')({
  beforeLoad: requireCapability('manage_users'),
  component: UsersFeature,
  staticData: { titleKey: 'nav.users' },
});
