import type { OIProfileChainItem } from '@/api/oi-profile'

/** The latest open interest a feed tick carried for one contract, and when. */
export interface LiveOi {
  oi: number
  /** Epoch ms the tick arrived. */
  at: number
}

/**
 * Key a live-OI map by exchange and symbol, uppercased the way the feed
 * normalises both, so a lowercase symbol in a profile answer still finds ticks.
 */
export const liveOiKey = (exchange: string, symbol: string): string =>
  `${exchange}:${symbol}`.toUpperCase()

/**
 * Lay live open interest from the WebSocket feed over a polled OI Profile.
 *
 * The profile answer is cached on the server and refreshed on a slow beat, so
 * on its own it lags the exchange. A broker whose feed carries `oi` lets the
 * page do better for free. A leg with no usable tick keeps its polled number,
 * which is what makes this safe on brokers whose feed never sends it.
 *
 * - A tick of 0 is unknown (some adapters send 0 when the packet carried no OI).
 * - A tick older than `maxAgeMs` is no longer live, so a feed that silently
 *   stops falls back to the polled number on the next beat.
 * - Change recomputes as `live - base` where a leg's base (its polled OI) is
 *   known; a leg without a base keeps its polled change.
 */
export function applyLiveOi(
  chain: OIProfileChainItem[],
  live: Map<string, LiveOi>,
  exchange: string,
  now: number = Date.now(),
  maxAgeMs: number = Number.POSITIVE_INFINITY
): { chain: OIProfileChainItem[]; liveLegs: number } {
  let liveLegs = 0
  const tickOi = (symbol: string | null | undefined): number | null => {
    if (!symbol) return null
    const t = live.get(liveOiKey(exchange, symbol))
    if (!t || !(t.oi > 0) || now - t.at > maxAgeMs) return null
    return t.oi
  }
  const next = chain.map((row) => {
    const out = { ...row }
    const ce = tickOi(row.ce_symbol)
    if (ce !== null) {
      liveLegs += 1
      out.ce_oi = ce
      if (row.ce_base != null) out.ce_oi_change = ce - row.ce_base
    }
    const pe = tickOi(row.pe_symbol)
    if (pe !== null) {
      liveLegs += 1
      out.pe_oi = pe
      if (row.pe_base != null) out.pe_oi_change = pe - row.pe_base
    }
    return out
  })
  return { chain: next, liveLegs }
}
