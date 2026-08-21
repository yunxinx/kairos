import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const E2E_ADMIN_EMAIL = 'root@localhost';
export const E2E_ADMIN_PASSWORD = 'sk-e2e-admin';
export const E2E_ADMIN_PORT = 18787;
export const E2E_PROTOCOL_PORT = 18786;

const E2E_WORK_DIR = path.join(os.tmpdir(), 'kairos-e2e-webui');

export const E2E_DB_PATH = path.join(E2E_WORK_DIR, 'kairos.db');
export const E2E_CONFIG_PATH = path.join(E2E_WORK_DIR, 'config.json');

/** Playwright 配置进程与测试进程共用的路径。 */
export interface E2eRuntime {
  configPath: string;
  dbPath: string;
}

/**
 * 写入固定路径的 e2e 配置。路径必须稳定：Playwright 会多次加载 config，
 * `mkdtemp` 会让 webServer 与测试落到不同的库文件。
 *
 * 邮箱/密码写进配置，让首次启动把内置 root 播种成可登录账号（不是 Bearer）。
 */
export function writeE2eConfig(): E2eRuntime {
  fs.mkdirSync(E2E_WORK_DIR, { recursive: true });
  fs.writeFileSync(
    E2E_CONFIG_PATH,
    JSON.stringify({
      listen: { host: '127.0.0.1', port: E2E_PROTOCOL_PORT },
      database: { path: E2E_DB_PATH },
      admin_email: E2E_ADMIN_EMAIL,
      admin_password: E2E_ADMIN_PASSWORD,
      admin_listen: { host: '127.0.0.1', port: E2E_ADMIN_PORT },
    }),
  );
  return { configPath: E2E_CONFIG_PATH, dbPath: E2E_DB_PATH };
}

export function readE2eRuntime(): E2eRuntime {
  return { configPath: E2E_CONFIG_PATH, dbPath: E2E_DB_PATH };
}
