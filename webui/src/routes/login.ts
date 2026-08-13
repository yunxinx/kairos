import { createFileRoute } from '@tanstack/vue-router';
import LoginFeature from '@/features/auth/LoginFeature.vue';
import { requireGuest } from '@/lib/router-guards';

export const Route = createFileRoute('/login')({
  beforeLoad: requireGuest,
  component: LoginFeature,
  staticData: { titleKey: 'auth.loginTitle' },
});
