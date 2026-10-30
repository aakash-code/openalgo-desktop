import { apiClient } from './client'

/** One ranked-list entry. The enrichment fields (CPR, first-candle,
 * directional, future-liquidity, momentum) only ever arrive on `intraday_boost`
 * and only once the server computes them -- the Desktop backend serves the base
 * four fields today and attaches the rest in a later follow-up, so each is
 * optional and simply absent until then. */
export interface TfListItem {
  symbol: string
  ltp: number
  prev_close: number
  change_pct: number
  score: number
  cpr_width_pct?: number | null
  cpr_bias?: 'bullish' | 'bearish' | null
  first_candle_range_pct?: number | null
  /** Kaufman efficiency-ratio "steadiness since open", 0-100. */
  directional_score?: number | null
  directional_direction?: 'up' | 'down' | null
  directional_reversals?: number | null
  /** How tradeable this stock's current-month future is right now. */
  fut_symbol?: string | null
  fut_spread_pct?: number | null
  fut_spread_rs?: number | null
  fut_turnover_cr?: number | null
  fut_tier?: 'tight' | 'ok' | 'wide' | null
  /** The chart's own Momentum Setup indicator, computed server-side. */
  momentum_signal?: 'buy' | 'sell' | null
  momentum_signal_price?: number | null
  momentum_signal_bar_time?: string | null
  momentum_signal_count?: number | null
}

export interface MarketPulseData {
  intraday_boost: TfListItem[]
  breakout_beacon: TfListItem[]
  high_powered_stocks: TfListItem[]
  /** Distinct intraday_boost symbols that have fired a Momentum Setup signal so
   * far today. Absent until the momentum enrichment lands server-side. */
  momentum_signals_today?: number | null
}

export interface MarketPulseResponse {
  status: 'success' | 'error'
  data?: MarketPulseData
  /** On `error` (e.g. no TradeFinder session configured), a trader-facing
   * sentence naming the cause and the fix -- safe to surface verbatim. */
  message?: string
}

/** Raw TradeFinder field names, passed through unmapped except `symbol` which
 * the backend adds. `param_3` is rfactor everywhere it appears. */
export interface SectorIndexItem {
  Symbol: string
  param_3: number
}

export interface SectorStockItem {
  Symbol: string
  symbol: string
  param_0: number
  param_1: number
  param_2: number
  param_3: number
  cpr_width_pct?: number | null
  cpr_bias?: 'bullish' | 'bearish' | null
}

export interface SectorScopeData {
  index: SectorIndexItem[]
  /** Keyed by `"<name>_r_factor"` -- strip that suffix before displaying. */
  sectors: Record<string, Record<string, SectorStockItem>>
}

export interface SectorScopeResponse {
  status: 'success' | 'error'
  data?: SectorScopeData
  message?: string
}

/** Client for the TradeFinder proxy endpoints the Desktop backend serves today
 * (POST /api/v1/tfmarketpulse and /api/v1/tfsectorscope; apikey in the body).
 *
 * The fork's client also exposes background-settings, JWT-health, boost-snapshot
 * and boost-movement methods. Those backends are not yet in Desktop, so they are
 * intentionally left out here rather than shipped as calls that would 404 -- add
 * each method alongside the route that backs it. */
export const tradefinderApi = {
  getMarketPulse: async (apiKey: string): Promise<MarketPulseResponse> => {
    const response = await apiClient.post<MarketPulseResponse>('/tfmarketpulse', {
      apikey: apiKey,
    })
    return response.data
  },

  getSectorScope: async (apiKey: string): Promise<SectorScopeResponse> => {
    const response = await apiClient.post<SectorScopeResponse>('/tfsectorscope', {
      apikey: apiKey,
    })
    return response.data
  },
}
