import type { Locator, Page } from '@playwright/test';

/** 打开行 ⋯ 菜单并点击其中一项（菜单经 portal 挂到 document.body）。 */
export async function clickRowAction(row: Locator, page: Page, testId: string): Promise<void> {
  await row.getByTestId('row-actions').click();
  await page.getByTestId(testId).click();
}
