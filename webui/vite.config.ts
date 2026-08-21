import tailwindcss from '@tailwindcss/vite';
import { tanstackRouter } from '@tanstack/router-plugin/vite';
import vue from '@vitejs/plugin-vue';
import path from 'node:path';
import { defineConfig } from 'vite';

const adminPort = process.env.KAIROS_E2E_ADMIN_PORT ?? '8788';
const adminTarget = `http://127.0.0.1:${adminPort}`;

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
    // 管理 API 整体在 `/api` 下，SPA 独占根命名空间：一条规则即可，
    // 不再需要逐个列出资源路径，也不再需要给 `/login` 做 method 级 bypass。
    proxy: {
      '/api': { target: adminTarget, changeOrigin: true },
    },
  },
});
