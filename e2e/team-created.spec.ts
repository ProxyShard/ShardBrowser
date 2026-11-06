import { expect, test, type Page } from "@playwright/test";
import { openApp, toggleDisplayColumns, waitForIdle } from "./support/app";
import { pickOption } from "./support/select";
import { ADMIN_INVITE, joinTeam, server, space } from "./support/team";

// #144: a team's rows are dated by the server's own row (remote.rs meta), not by
// settings this device happened to read, so Created shows and sorts in a team too.

const names = (page: Page) => page.locator("[role='row'][data-profile-id] [data-cell='name']").allTextContents();
const created = (page: Page, id: string) => page.locator(`[role='row'][data-profile-id='${id}'] [data-cell='created']`);

async function order(page: Page, option: string) {
  await page.getByRole("button", { name: /^Display/ }).click();
  await pickOption(page.getByRole("dialog", { name: "Display" }), "Order", option);
  await page.keyboard.press("Escape");
}

// Made 1 to 29 days before the frozen clock (src/dev/mock/teamServer.ts).
const NEWEST = [
  "Reddit 01", "Google Ads 01", "Amazon seller", "FB Ads 02", "FB Ads 01", "Etsy backup", "Etsy main", "Shared sandbox",
];

test("a team's rows show when each was made, and sort by it both ways", async ({ page }) => {
  await openApp(page, { section: "profiles" });
  await joinTeam(page, ADMIN_INVITE);
  await expect(space(page)).toHaveText("Ads team");
  await waitForIdle(page);
  await toggleDisplayColumns(page, "Created");
  await expect(created(page, "tp-ads-3")).toContainText("4 days ago");
  // Every row has a date: none reads "—".
  await expect(page.locator("[role='row'][data-profile-id] [data-cell='created']")).toHaveCount(8);
  await expect(page.locator("[role='row'][data-profile-id] [data-cell='created']", { hasText: "—" })).toHaveCount(0);

  await order(page, "Newest first");
  await expect.poll(() => names(page)).toEqual(NEWEST);
  await order(page, "Oldest first");
  await expect.poll(() => names(page)).toEqual([...NEWEST].reverse());

  // The server's rows carry the date, as store.rs answers them.
  const rows = (await server(page)).profiles as { id: string; created_at: number | null }[];
  expect(rows.every((r) => typeof r.created_at === "number")).toBe(true);
});
