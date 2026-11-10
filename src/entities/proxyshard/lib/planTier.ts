import { PS_PLAN } from "../model/constants";
import type { ResiType } from "../model/types";

/// Pure parts of the plan reads (`plans.ts`), kept apart so the signal
/// module can use them without the account store.

/** One residential tier as the account has it now. */
export type PlanUsage = {
  tier: ResiType;
  /** The order behind it (Top up adds traffic to it) and its product's
   *  name; null when none matched. */
  orderId: number | null;
  productName: string | null;
  /** Bytes; null for an unmetered plan or when the read failed. */
  total: number | null;
  remain: number | null;
  spent: number | null;
  /** When the plan ends, ms; null when unknown. */
  endsAt: number | null;
  /** When its current period began, ms (the rate's start); null when unknown. */
  startedAt: number | null;
};

const TOKEN_TIER = Object.fromEntries(Object.entries(PS_PLAN).map(([tier, token]) => [token, tier])) as Record<string, ResiType>;

/** The residential tier a proxy draws its traffic from: a ProxyShard relay
 *  whose user name starts with the plan's token ("plan-premium-…"). The
 *  relays moved to shardx.com; proxies saved on the old hosts still count. */
export function planTierOf(p: { host: string; username: string }): ResiType | null {
  if (!/proxyshard|(?:^|\.)shardx\.(?:com|net)$/i.test(p.host)) return null;
  const token = /^plan-([a-z]+)/i.exec(p.username)?.[1]?.toLowerCase();
  return token ? TOKEN_TIER[token] ?? null : null;
}

const FIFTY_YEARS = 50 * 365.25 * 86_400_000;

/** A plan with no end: residential traffic is bought by the GB and does not
 *  run out by date (the API gives it an end date in 2125, which is no end),
 *  and any plan whose end lies more than 50 years away. An Unlimited plan is
 *  the exception: it is a month at a time, and its end date is real. */
export function isLifetime(plan: PlanUsage, now = Date.now()): boolean {
  if (plan.tier !== "unmetered") return true;
  return plan.endsAt !== null && plan.endsAt - now > FIFTY_YEARS;
}
