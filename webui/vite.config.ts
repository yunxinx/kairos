import tailwindcss from '@tailwindcss/vite';
import { tanstackRouter } from '@tanstack/router-plugin/vite';
import vue from '@vitejs/plugin-vue';
import path from 'node:path';
import { defineConfig } from 'vite';

const adminPort = process.env.KAIROS_E2E_ADMIN_PORT ?? '8788';
const adminTarget = `http://127.0.0.1:${adminPort}`;

/** 管理 API 路径（无 `/api` 前缀）；dev server 代理到本地管理监听。 */
const adminApiPrefixes = [
  '/tokens',
  '/channels',
  '/prices',
  '/model-groups',
  '/unified-models',
  '/catalog',
  '/settings',
  '/logs',
  '/stats',
];

export default defineConfig({
  plugins: [
    tanstackRouter({
      target: 'vue',
      autoCodeSplitting: true,
      routesDirectory: './src/routes',
      generatedRouteTree: './src/routeTree.gen.ts',
    }),
    vue(),
    tailwindcss(),
  ],
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
    extensions: ['.vue', '.mjs', '.js', '.mts', '.ts', '.jsx', '.tsx', '.json'],
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    proxy: Object.fromEntries(
      adminApiPrefixes.map((prefix) => [prefix, { target: adminTarget, changeOrigin: true }]),
    ),
  },
});
