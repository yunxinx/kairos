import { expect, type Page } from '@playwright/test';
import { e2eRootHeaders } from './session';
import {
  channelWriteBody,
  type Channel,
  type ChannelView,
  type ModelGroup,
  type Price,
  type UnifiedModel,
} from '../../src/api/types';

/** 经管理 API 写入一条渠道，供模型页 e2e 绕过渠道编辑器。 */
export async function seedChannel(
  page: Page,
  body: Partial<Channel> & Pick<Channel, 'name' | 'models'>,
): Promise<{ id: number }> {
  const headers = await e2eRootHeaders(page.request);
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
  const resp = await page.request.post('/prices', {
    headers: await e2eRootHeaders(page.request),
    data: body,
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 经管理 API 写入一个统一模型。 */
export async function seedUnifiedModel(page: Page, body: UnifiedModel): Promise<void> {
  const resp = await page.request.post('/unified-models', {
    headers: await e2eRootHeaders(page.request),
    data: body,
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 经管理 API 写入一个模型组。 */
export async function seedModelGroup(page: Page, body: ModelGroup): Promise<void> {
  const resp = await page.request.post('/model-groups', {
    headers: await e2eRootHeaders(page.request),
    data: body,
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 经管理 API 写入一条令牌，供分组强制删除等用例绑定组。 */
export async function seedToken(
  page: Page,
  body: Partial<{ name: string; model_group: string }> & { name: string },
): Promise<{ token_key: string }> {
  const resp = await page.request.post('/tokens', {
    headers: await e2eRootHeaders(page.request),
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

/** 经管理 API 整表替换价格目录缓存。 */
export async function seedCatalog(
  page: Page,
  models: Array<{
    provider_id: string;
    provider_name: string;
    model_id: string;
    input_micros: number | null;
    output_micros: number | null;
    cache_read_micros: number | null;
    cache_write_micros: number | null;
  }>,
): Promise<void> {
  const resp = await page.request.put('/catalog', {
    headers: await e2eRootHeaders(page.request),
    data: { models },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 读出当前渠道后 PATCH 式覆盖写字段。 */
export async function updateChannel(
  page: Page,
  id: number,
  patch: Partial<Channel>,
): Promise<void> {
  const headers = await e2eRootHeaders(page.request);
  const listed = await page.request.get('/channels', { headers });
  expect(listed.ok(), await listed.text()).toBeTruthy();
  const channels = (await listed.json()) as ChannelView[];
  const current = channels.find((channel) => channel.id === id);
  if (current === undefined) {
    throw new Error(`channel ${id} not found`);
  }
  const resp = await page.request.put(`/channels/${id}`, {
    headers,
    data: { ...channelWriteBody(current), ...patch },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

/** 删除渠道。 */
export async function deleteChannel(page: Page, id: number): Promise<void> {
  const resp = await page.request.delete(`/channels/${id}`, {
    headers: await e2eRootHeaders(page.request),
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}
