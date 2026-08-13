import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';
import { E2E_ADMIN_PORT, E2E_PROTOCOL_PORT, E2E_ADMIN_KEY } from './e2e/helpers/gateway';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function writeE2eConfig(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kairos-e2e-'));
  const configPath = path.join(dir, 'config.json');
  fs.writeFileSync(
    configPath,
    JSON.stringify({
      listen: { host: '127.0.0.1', port: E2E_PROTOCOL_PORT },
      database: { path: path.join(dir, 'kairos.db') },
      admin_key: E2E_ADMIN_KEY,
      admin_listen: { host: '127.0.0.1', port: E2E_ADMIN_PORT },
    }),
  );
  return configPath;
}

const e2eConfigPath = writeE2eConfig();

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
  },
  webServer: {
    command: `cargo run -q --bin kairos -- --config ${e2eConfigPath}`,
    cwd: repoRoot,
    url: `http://127.0.0.1:${E2E_ADMIN_PORT}/`,
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
