import { webClient } from './client'

export interface CandleData {
  timestamp?: string | number
  time?: string | number
  open: number
  high: number
  low: number
  close: number
  volume: number
  oi?: number
}

export interface OIProfileChainItem {
  strike: number
  ce_oi: number
  pe_oi: number
  /**
   * Desktop: null when the previous day's OI could not be had for this
   * contract (TOOLS-14); the web sends 0, which read as no change.
   */
  ce_oi_change: number | null
  pe_oi_change: number | null
  /** The polled OI, so a live WebSocket `oi` tick draws `live - base`. */
  ce_base?: number | null
  pe_base?: number | null
  /** The leg symbol, to match a live tick (null when the strike has no leg). */
  ce_symbol?: string | null
  pe_symbol?: string | null
}

export interface OIProfileDataResponse {
  status: 'success' | 'error'
  message?: string
  underlying?: string
  spot_price?: number
  atm_strike?: number
  lot_size?: number
  expiry_date?: string
  futures_symbol?: string | null
  interval?: string
  candles?: CandleData[]
  oi_chain?: OIProfileChainItem[]
}

export interface IntervalsResponse {
  status: 'success' | 'error'
  data?: { intervals: string[] }
}

export interface UnderlyingsResponse {
  status: 'success' | 'error'
  underlyings: string[]
}

export interface ExpiriesResponse {
  status: 'success' | 'error'
  expiries: string[]
}

export const oiProfileApi = {
  getProfileData: async (params: {
    underlying: string
    exchange: string
    expiry_date: string
    interval: string
    days: number
  }): Promise<OIProfileDataResponse> => {
    const response = await webClient.post<OIProfileDataResponse>(
      '/oiprofile/api/profile-data',
      params
    )
    return response.data
  },

  getIntervals: async (): Promise<IntervalsResponse> => {
    const response = await webClient.get<IntervalsResponse>('/oiprofile/api/intervals')
    return response.data
  },

  getUnderlyings: async (exchange: string): Promise<UnderlyingsResponse> => {
    const response = await webClient.get<UnderlyingsResponse>(
      `/search/api/underlyings?exchange=${exchange}`
    )
    return response.data
  },

  getExpiries: async (exchange: string, underlying: string): Promise<ExpiriesResponse> => {
    const response = await webClient.get<ExpiriesResponse>(
      `/search/api/expiries?exchange=${encodeURIComponent(exchange)}&underlying=${encodeURIComponent(underlying)}&instrumenttype=options`
    )
    return response.data
  },
}
