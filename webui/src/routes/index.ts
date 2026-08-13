import { h } from 'vue';
import { createFileRoute, redirect } from '@tanstack/vue-router';
import { getAdminKey } from '@/lib/session';

export const Route = createFileRoute('/')({
  beforeLoad: () => {
    throw redirect({ to: getAdminKey() ? '/overview' : '/login' });
  },
  component: () => h('div'),
});
