import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';
import {
  E2E_ADMIN_PORT,
  E2E_CONFIG_PATH,
  E2E_DB_PATH,
  writeE2eConfig,
} from './e2e/helpers/gateway';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
writeE2eConfig();

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  workers: 1,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://127.0.0.1:${E2E_ADMIN_PORT}`,
    locale: 'en-US',
    permissions: ['clipboard-read', 'clipboard-write'],
  },
  webServer: {
    command: `pnpm --dir webui build && rm -f ${E2E_DB_PATH} ${E2E_DB_PATH}-wal ${E2E_DB_PATH}-shm && cargo run -q --bin kairos -- --config ${E2E_CONFIG_PATH}`,
    cwd: repoRoot,
    url: `http://127.0.0.1:${E2E_ADMIN_PORT}/`,
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
