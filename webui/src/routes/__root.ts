import { createRootRoute } from '@tanstack/vue-router';
import RootLayout from '@/app/shell/RootLayout.vue';

export const Route = createRootRoute({
  component: RootLayout,
});
