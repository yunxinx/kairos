import { createFileRoute } from '@tanstack/vue-router';
import AccountFeature from '@/features/account/AccountFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/account')({
  beforeLoad: requireAuth,
  component: AccountFeature,
  staticData: { titleKey: 'nav.account' },
});
