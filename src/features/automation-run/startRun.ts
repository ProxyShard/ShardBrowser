import { automationRun, type Project } from "../../entities/automation";
import { profileList } from "../../entities/profile/model/api";
import type { ProfileMeta } from "../../entities/profile/model/types";
import { confirmDirect, isProtected, lockedRefusal, proxyMissing, useProfile, useProfileUi } from "../../entities/profile";
import { proxyList } from "../../entities/proxy";
import { t } from "../../shared/i18n";
import { toast } from "../../shared/model/toast";
import { isUnreviewed, markReviewed, reviewLines, type ReviewLine } from "../automation-review";

/// What a project's first Run must show before it starts (SEC-02): the lines
/// of an imported project that reaches beyond the page, or null when there is
/// nothing to show (the project is then marked reviewed).
export function reviewBeforeRun(project: Project): ReviewLine[] | null {
  if (!isUnreviewed(project.id)) return null;
  const lines = reviewLines(project.blocks, project.rules ?? []);
  if (lines.length > 0) return lines;
  markReviewed(project.id);
  return null;
}

/// The saved profiles a run will open: the `id` of each enabled "Use profile"
/// step. `project.run.profiles` is NOT it — the runner ignores that field
/// (runner.rs: kept only by projects saved before profiles became steps), and
/// nothing in the UI can clear it, so going by it would refuse a run over a
/// profile the run never opens. An id built from a variable can't be known
/// before the run and is left to the runner.
function profilesUsed(project: Project): string[] {
  const ids = project.blocks
    .filter((b) => b.enabled && b.kind === "profile.use")
    .map((b) => b.params?.id)
    .filter((id): id is string => typeof id === "string" && id !== "" && !id.includes("{"));
  return [...new Set(ids)];
}

/// Starts a project's run, from the editor or from the list. A project that
/// opens a profile whose proxy was deleted must not run it on this computer's
/// own IP (IFACE-01, backend B1): same notice and fix as every other Start
/// path. The profiles and proxies are read fresh at Run, not from a list loaded
/// earlier, which may not have arrived yet or may miss a proxy deleted since;
/// when they can't be read, nothing runs.
export async function startProjectRun(project: Project): Promise<void> {
  let fresh: ProfileMeta[];
  let proxies: { id: string }[];
  try {
    [fresh, proxies] = await Promise.all([profileList(), proxyList()]);
  } catch (e) {
    toast.fail({ title: t("projectEditor.runFailed", { name: project.name }), detail: e });
    return;
  }
  const targets = profilesUsed(project)
    .map((id) => fresh.find((p) => p.id === id))
    .filter((p): p is ProfileMeta => !!p);
  // A run never opens a protected profile (the runner refuses it): said before anything starts.
  const locked = targets.find(isProtected);
  if (locked) {
    toast.fail({ title: t("projectEditor.runFailed", { name: project.name }), detail: `${locked.name}: ${lockedRefusal(locked)}` });
    return;
  }
  const missing = targets.filter((p) => proxyMissing(p, proxies));
  if (missing.length > 0) {
    toast.err(
      missing.length === 1
        ? t("useProfile.proxyMissingStart", { name: missing[0].name })
        : t("useProfile.proxyMissingStartMany", { n: missing.length }),
      {
        action: {
          label: missing.length === 1 ? t("useProfile.assignProxy") : t("useProfile.assignProxies"),
          run: () => useProfileUi.getState().openAssign(missing.map((p) => p.id)),
        },
      },
    );
    return;
  }
  // The same O9a question as every Start: a Run is all-or-nothing, so
  // anything but "start" means nothing runs (Cancel, Esc alike; Assign opens
  // the Assign proxies flow, for the profiles in the run with no proxy).
  // `allowSkip: false` because a run can't start a subset of its profiles.
  const choice = await confirmDirect(useProfile.getState, targets, { allowSkip: false });
  if (choice === "assign") { useProfileUi.getState().openAssign(targets.filter((p) => !p.proxy_id).map((p) => p.id)); return; }
  if (choice !== "start") return;
  await automationRun(project.id).catch((e) => toast.fail({ title: t("projectEditor.runFailed", { name: project.name }), detail: e }));
}
