// 用普通 `test` 而不是 `authedTest`：后者用 addInitScript 写 root 会话，每次导航都会
// 重放，把 `openSession` 写进去的用户会话覆盖回 root。播种走 `page.request`，自己登录，
// 不依赖 localStorage。
import { expect, test } from './fixtures';
import { e2eRootHeaders } from './helpers/session';
import { seedChannel, seedModelGroup, seedPrice } from './helpers/models';
import { openSession, seedUser } from './helpers/users';
import type { Page } from '@playwright/test';

/** 建一档带组名单与折扣的套餐，返回 id。 */
async function createPlan(
  page: Page,
  body: { display_name: string; groups: string[]; discount_bp?: number },
): Promise<number> {
  const resp = await page.request.post('/api/plans', {
    headers: await e2eRootHeaders(page),
    data: {
      display_name: body.display_name,
      note: '',
      note_visible_to_admin: false,
      discount_bp: body.discount_bp ?? 10000,
      default_rpm: null,
      shared_rpm: null,
      initial_grant_usd_micros: 0,
      capabilities: {},
      shared_with_admin: true,
      groups: body.groups,
    },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
  return ((await resp.json()) as { id: number }).id;
}

async function assignPlan(page: Page, userId: number, planId: number): Promise<void> {
  const resp = await page.request.put(`/api/users/${userId}/plan`, {
    headers: await e2eRootHeaders(page),
    data: { plan_id: planId },
  });
  expect(resp.ok(), await resp.text()).toBeTruthy();
}

test.describe.configure({ mode: 'serial' });

test.describe('my models page', () => {
  test('user sees own groups sectioned with discounted prices and no channel names', async ({
    page,
  }) => {
    // 两条渠道登记同一个名字、报价不同 → 该行应给出区间。
    const cheap = await seedChannel(page, {
      name: 'e2e-my-cheap',
      models: ['shared-model', 'solo-model'],
    });
    const pricey = await seedChannel(page, { name: 'e2e-my-pricey', models: ['shared-model'] });
    await seedPrice(page, {
      channel_id: cheap.id,
      model: 'shared-model',
      input_micros: 2_000_000,
      output_micros: 8_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedPrice(page, {
      channel_id: pricey.id,
      model: 'shared-model',
      input_micros: 4_000_000,
      output_micros: 8_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedPrice(page, {
      channel_id: cheap.id,
      model: 'solo-model',
      input_micros: 1_000_000,
      output_micros: 3_000_000,
      cache_read_micros: null,
      cache_write_micros: null,
    });
    await seedModelGroup(page, {
      name: 'e2e-my-coding',
      models: [{ kind: 'source', channel_id: cheap.id, model: 'solo-model' }],
    });

    // 五折档 + 两个组：验证分段、折后价与区间。
    const planId = await createPlan(page, {
      display_name: 'e2e-my-plan',
      groups: ['default', 'e2e-my-coding'],
      discount_bp: 5000,
    });
    const user = await seedUser(page, { email: 'my-models@example.com', role: 'user' });
    await assignPlan(page, user.id, planId);

    await openSession(page, 'my-models@example.com');
    await page.goto('/models');

    // 普通用户不出标签页：整页只有这一张只读表。
    await expect(page.getByTestId('my-models-table')).toBeVisible();
    await expect(page.getByTestId('models-tab-inventory')).toHaveCount(0);
    await expect(page.getByTestId('models-tab-groups')).toHaveCount(0);

    // 分段：default 在前，自定义组在后。
    const sections = page.getByTestId('my-models-section');
    await expect(sections).toHaveCount(2);
    await expect(sections.nth(0)).toContainText(/ungrouped/i);
    await expect(sections.nth(1)).toContainText('e2e-my-coding');

    // 五折：2.0 → 1.0，跨渠道 2.0/4.0 → 区间 $1–$2。
    const shared = page.locator('[data-testid="my-model"][data-model="shared-model"]');
    await expect(shared.getByTestId('my-model-input')).toContainText('$1–$2');
    await expect(shared.getByTestId('my-model-output')).toContainText('$4');

    // solo-model 归了自定义组，故只在那一段出现，且单渠道两端相等只显示一个数。
    const solo = page.locator('[data-testid="my-model"][data-model="solo-model"]');
    await expect(solo).toHaveCount(1);
    await expect(solo.getByTestId('my-model-input')).toHaveText('$0.5');

    await expect(page.getByTestId('my-models-discount')).toContainText('50%');

    // 这一页存在的理由：不泄漏渠道拓扑。
    const body = await page.locator('body').innerText();
    expect(body).not.toContain('e2e-my-cheap');
    expect(body).not.toContain('e2e-my-pricey');

    // 搜索按可调用名过滤。
    await page.getByTestId('my-models-search').fill('solo');
    await expect(page.locator('[data-testid="my-model"]')).toHaveCount(1);
    await page.getByTestId('my-models-search').fill('nothing-matches-this');
    await expect(page.locator('[data-testid="my-model"]')).toHaveCount(0);
    // 搜索滤空给「没有匹配」，不能说成「你的套餐没有模型组」——那是假警报。
    await expect(page.getByTestId('my-models-table')).toContainText(/no models match/i);
    await expect(page.getByTestId('my-models-table')).not.toContainText(/no model groups yet/i);
  });
});
