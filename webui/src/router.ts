import { h } from 'vue';
import { createRouter, type ErrorComponentProps } from '@tanstack/vue-router';
import { routeTree } from '@/routeTree.gen';
import RouteErrorFeature from '@/features/errors/RouteErrorFeature.vue';

export const router = createRouter({
  routeTree,
  defaultPreload: 'intent',
  defaultErrorComponent: (props: ErrorComponentProps) => h(RouteErrorFeature, props),
});

declare module '@tanstack/vue-router' {
  interface Register {
    router: typeof router;
  }

  interface StaticDataRouteOption {
    titleKey?: string;
    /** 表格页：内容区填满视口，在表内滚动。 */
    fillViewport?: boolean;
  }
}
