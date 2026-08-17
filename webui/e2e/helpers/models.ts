import { expect, type Page } from '@playwright/test';
import { E2E_ADMIN_KEY } from './gateway';
import type { Channel, ModelGroup, Price, UnifiedModel } from '../../src/api/types';

const headers = { Authorization: `Bearer ${E2E_ADMIN_KEY}` };

/** 经管理 API 写入一条渠道，供模型页 e2e 绕过渠道编辑器。 */
export async function seedChannel(
  page: Page,
  body: Partial<Channel> & Pick<Channel, 'name' | 'models'>,
): Promise<{ id: number }> {
  const resp = await page.request.post('/channels', {
    headers,
    data: {
      protocol: 'openai_chat',
      base_url: 'http://127.0.0.1:9',
      api_key: 'sk-e2e',
      model_aliases: {},
      priority: 0,
      weight: 1,
      timeout_ms: 5000,
      max_retries: 0,
      enabled: true,
      ...body,
    },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  const created = (await resp.json()) as { id: number };
  return { id: created.id };
}

/** 经管理 API 写入一条价格。 */
export async function seedPrice(page: Page, body: Price): Promise<void> {
  const resp = await page.request.post('/prices', { headers, data: body });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 经管理 API 写入一个统一模型。 */
export async function seedUnifiedModel(page: Page, body: UnifiedModel): Promise<void> {
  const resp = await page.request.post('/unified-models', { headers, data: body });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 经管理 API 写入一个模型组。 */
export async function seedModelGroup(page: Page, body: ModelGroup): Promise<void> {
  const resp = await page.request.post('/model-groups', { headers, data: body });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 经管理 API 写入一条令牌，供分组强制删除等用例绑定组。 */
export async function seedToken(
  page: Page,
  body: Partial<{ name: string; model_group: string }> & { name: string },
): Promise<{ token_key: string }> {
  const resp = await page.request.post('/tokens', {
    headers,
    data: {
      limit_usd_micros: null,
      enabled: true,
      model_group: 'default',
      ...body,
    },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  return (await resp.json()) as { token_key: string };
}
