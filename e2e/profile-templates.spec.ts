import { expect, test, type Page } from "@playwright/test";
import { openApp, row, waitForIdle } from "./support/app";
import { pickOption } from "./support/select";
import { MEMBER_INVITE, joinTeam, space } from "./support/team";

// Profile templates (#145): a profile's settings kept under a name; a new
// profile from one gets those settings and a fingerprint of its own.
type Stored = {
  name: string; notes: string; webrtc: string;
  navigator: { hardware_concurrency: number };
  noise: Record<string, { enabled: boolean; seed: number }>;
  _meta: { id: string; folder: string; proxy_id: string | null; gpu_preset_id: string; color?: string; extensions: string[] };
};
type Template = {
  id: string; name: string; folder: string; tags: string[]; is_default: boolean;
  source: { library: string | null; os: string };
  settings: Record<string, any>;
};
type Mock = {
  __shardxMock: {
    state: { order: string[]; profiles: Record<string, Stored>; templates?: Template[] };
    save: () => void;
    emit: (event: string, payload?: unknown) => void;
    teamGrants: (g: { scope: { kind: string; name?: string }; action: string; allow: boolean }[]) => void;
  };
};
const ADS = "c31d92f0";
const dialog = (page: Page, name: string) => page.getByRole("dialog", { name, exact: true });
const toastText = (page: Page, text: string) => page.locator("[data-toast]").getByText(text, { exact: true });
const templates = (page: Page) => page.evaluate(() => (window as unknown as Mock).__shardxMock.state.templates ?? []);
const newest = (page: Page) => page.evaluate(() => {
  const s = (window as unknown as Mock).__shardxMock.state;
  return s.profiles[s.order[s.order.length - 1]];
});

/** FB Ads acc-01 with settings a template should carry, and noise seeds it must not. */
async function tuneAds(page: Page) {
  await page.evaluate((id) => {
    const m = (window as unknown as Mock).__shardxMock;
    const p = m.state.profiles[id];
    p.webrtc = "tcp_only";
    p.noise.canvas = { enabled: true, seed: 77 };
    m.save();
  }, ADS);
}

/** The row's menu, Save as template, named and made the default. */
async function saveAdsAsTemplate(page: Page, name: string) {
  await row(page, "FB Ads acc-01").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Save as template", exact: true }).click();
  const d = dialog(page, "Save as template");
  const field = d.getByRole("textbox", { name: "Template name" });
  await expect(field).toHaveValue("FB Ads acc-01");
  await field.fill(name);
  await expect(d.getByRole("combobox", { name: "Fingerprint of each new profile" })).toContainText("A new one from");
  await d.getByRole("checkbox", { name: "Use for every new profile" }).check();
  await d.getByRole("button", { name: "Save template" }).click();
  await expect(d).toHaveCount(0);
  await expect(toastText(page, `Template “${name}” created`)).toBeVisible();
}

test("a profile saved as a template keeps its settings and where its fingerprint came from, nothing of its own", async ({ page }) => {
  await openApp(page);
  await tuneAds(page);
  await saveAdsAsTemplate(page, "Ads baseline");
  const [tpl] = await templates(page);
  expect(tpl).toMatchObject({ name: "Ads baseline", folder: "Ads", is_default: true, source: { library: "win11-rtx3060", os: "Windows" } });
  expect(tpl.settings).toMatchObject({ webrtc: "tcp_only", _meta: { proxy_id: "px-us-1", color: "#3b82f6", extensions: ["ublock", "cookie-editor"] } });
  expect(tpl.settings.noise.canvas).toEqual({ enabled: true, seed: 0 });
  for (const key of ["name", "notes", "webgl", "screen_custom"]) expect(tpl.settings).not.toHaveProperty(key);
  expect(tpl.settings._meta).not.toHaveProperty("gpu_preset_id");
  expect(tpl.settings.navigator ?? {}).not.toHaveProperty("hardware_concurrency");
});

test("New profile starts from the default template: its settings, a fresh fingerprint, a name of its own", async ({ page }) => {
  await openApp(page);
  await tuneAds(page);
  await saveAdsAsTemplate(page, "Ads baseline");
  const before = await page.evaluate(() => (window as unknown as Mock).__shardxMock.state.order.length);

  await page.getByRole("button", { name: "New profile" }).first().click();
  const d = dialog(page, "New profile");
  await expect(d.getByRole("combobox", { name: "Template" })).toContainText("Ads baseline");
  await expect(d.getByRole("textbox", { name: "Name" })).toHaveValue(/^Profile \d+$/);
  await d.getByRole("button", { name: "Create profile" }).click();
  await expect(d).toHaveCount(0);
  await waitForIdle(page);

  expect(await page.evaluate(() => (window as unknown as Mock).__shardxMock.state.order.length)).toBe(before + 1);
  const made = await newest(page);
  expect(made.name).toMatch(/^Profile \d+$/);
  expect(made.notes).toBe("");
  expect(made.webrtc).toBe("tcp_only");
  expect(made._meta).toMatchObject({ folder: "Ads", proxy_id: "px-us-1", color: "#3b82f6", extensions: ["ublock", "cookie-editor"], gpu_preset_id: "win11-rtx3060" });
  // Its own fingerprint: the noise is on as the template says, under a seed of its own, not the profile's.
  expect(made.noise.canvas.enabled).toBe(true);
  expect(made.noise.canvas.seed).not.toBe(77);
  expect(made._meta.id).not.toBe(ADS);
});

test("Blank profile starts the form over without the template", async ({ page }) => {
  await openApp(page);
  await tuneAds(page);
  await saveAdsAsTemplate(page, "Ads baseline");

  await page.getByRole("button", { name: "New profile" }).first().click();
  const d = dialog(page, "New profile");
  await expect(d.getByRole("combobox", { name: "Template" })).toContainText("Ads baseline");
  await pickOption(d, "Template", "Blank profile");
  await expect(d.getByRole("combobox", { name: "Template" })).toContainText("Blank profile");
  await d.getByRole("button", { name: "Create profile" }).click();
  await expect(d).toHaveCount(0);
  await waitForIdle(page);
  const made = await newest(page);
  expect(made.webrtc).toBe("auto");
  expect(made._meta.proxy_id).toBeNull();
});

test("Save as template in the profile form keeps the form open with what it shows", async ({ page }) => {
  await openApp(page);
  await page.getByRole("button", { name: "New profile" }).first().click();
  const form = dialog(page, "New profile");
  await expect(form.getByRole("textbox", { name: "Name" })).toBeFocused();
  await form.getByRole("button", { name: "Save as template", exact: true }).click();
  const d = dialog(page, "Save as template");
  await d.getByRole("textbox", { name: "Template name" }).fill("From the form");
  await d.getByRole("button", { name: "Save template" }).click();
  await expect(d).toHaveCount(0);
  await expect(form).toBeVisible();
  expect((await templates(page)).map((x) => x.name)).toEqual(["From the form"]);
  // A name taken is said before anything is sent.
  await form.getByRole("button", { name: "Save as template", exact: true }).click();
  await d.getByRole("textbox", { name: "Template name" }).fill("from the FORM");
  await expect(d.getByText("A template named “from the FORM” already exists. Pick another name.")).toBeVisible();
});

test("Settings, Templates: a template opens in the form to change, and a delete has an Undo", async ({ page }) => {
  await openApp(page);
  await tuneAds(page);
  await saveAdsAsTemplate(page, "Ads baseline");
  await page.goto("/?section=settings&tab=templates");
  await waitForIdle(page);
  const list = page.locator("[data-template-list]");
  await expect(list.locator("[data-template-row]")).toHaveCount(1);
  await expect(list).toContainText("Ads baseline");
  await expect(page.getByRole("switch", { name: "Use “Ads baseline” for every new profile" })).toBeChecked();

  await page.getByRole("button", { name: "Edit template “Ads baseline”" }).click();
  const d = dialog(page, "Edit template “Ads baseline”");
  const name = d.getByRole("textbox", { name: "Template name" });
  await expect(name).toHaveValue("Ads baseline");
  // Each profile's own: not in a template's form.
  await expect(d.locator("[data-property='notes']")).toHaveCount(0);
  await expect(d.locator("[data-property='folder']")).toHaveCount(1);
  await d.getByRole("tab", { name: "Fingerprint" }).click();
  await expect(d.locator("[data-template-source]")).toBeVisible();
  await expect(d.locator("[data-template-drawn]")).toBeVisible();
  await d.getByRole("tab", { name: "Basics" }).click();
  await name.fill("Ads v2");
  await d.getByRole("button", { name: "Save", exact: true }).click();
  await expect(d).toHaveCount(0);
  await expect(list).toContainText("Ads v2");
  const [tpl] = await templates(page);
  expect(tpl).toMatchObject({ name: "Ads v2", is_default: true, folder: "Ads" });
  expect(tpl.settings.webrtc).toBe("tcp_only");

  await page.getByRole("button", { name: "Delete template “Ads v2”" }).click();
  await expect(list.locator("[data-template-row]")).toHaveCount(0);
  await page.locator("[data-toast]").getByRole("button", { name: "Undo" }).click();
  await expect(list.locator("[data-template-row]")).toHaveCount(1);
  expect((await templates(page)).length).toBe(1);
});

test("in a team, a member who may make profiles in one folder reads the team's templates and changes none", async ({ page }) => {
  await openApp(page);
  await joinTeam(page, MEMBER_INVITE);
  await expect(space(page)).toHaveText("Shops team");
  // The team's own template; this member may only make profiles in Ads, and not See the team.
  await page.evaluate(() => {
    const m = (window as unknown as Mock).__shardxMock;
    m.state.templates = [{
      id: "pt-team", name: "Team baseline", source: { library: null, os: "Windows" },
      settings: { webrtc: "tcp_only", _meta: { proxy_id: null } }, folder: "Ads", tags: [], is_default: false,
    } as Template];
    m.save();
    const ads = { kind: "folder", name: "Ads" };
    m.teamGrants([{ scope: ads, action: "see", allow: true }, { scope: ads, action: "create", allow: true }]);
    m.emit("store-changed", "templates");
  });
  await page.goto("/?section=settings&tab=templates");
  await waitForIdle(page);
  const list = page.locator("[data-template-list]");
  await expect(list).toContainText("Team baseline");
  await expect(page.getByRole("button", { name: /^Delete template “Team baseline”/ })).toBeDisabled();
  await expect(page.getByRole("switch", { name: "Use “Team baseline” for every new profile" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "New template" })).toBeDisabled();

  // Using one asks only for Create where the profile lands.
  await page.goto("/?section=profiles");
  await waitForIdle(page);
  const before = await page.evaluate(() => (window as unknown as Mock).__shardxMock.state.order.length);
  await page.getByRole("button", { name: "New profile" }).first().click();
  const d = dialog(page, "New profile");
  await pickOption(d, "Template", "Team baseline");
  await expect(d.locator("[data-property='folder']")).toContainText("Ads");
  await d.getByRole("button", { name: "Create profile" }).click();
  await expect(d).toHaveCount(0);
  await waitForIdle(page);
  expect(await page.evaluate(() => (window as unknown as Mock).__shardxMock.state.order.length)).toBe(before + 1);
  const made = await newest(page);
  expect(made.webrtc).toBe("tcp_only");
  expect(made._meta.folder).toBe("Ads");
});
