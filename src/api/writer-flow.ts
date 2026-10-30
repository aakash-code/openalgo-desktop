import { webClient } from './client'

export interface WriterFlowSession {
  trade_date: string
  underlying: string
  expiry: string
  minutes: number
  last_sample: string
}

export interface WriterFlowSessionsResponse {
  status: 'success' | 'error'
  recorder_enabled: boolean
  sessions: WriterFlowSession[]
  message?: string
}

export type FlowClass =
  | 'writing'
  | 'covering'
  | 'buying'
  | 'unwinding'
  | 'adding'
  | 'reducing'
  | 'steady'

export interface WriterFlowSide {
  cls: FlowClass
  bias: -1 | 0 | 1
  streak_minutes: number
  streak_oi_change: number
  steady_minutes: number
  oi: number
  window_change: Record<string, number | null>
}

export interface WriterFlowRow {
  strike: number
  atm: boolean
  ce: WriterFlowSide | null
  pe: WriterFlowSide | null
}

export interface WriterFlowEvent {
  time: string
  strike: number
  side: 'CE' | 'PE'
  kind: FlowClass
  minutes: number
  oi_change: number
  bias: -1 | 0 | 1
}

export interface WriterFlowResponse {
  status: 'success' | 'empty' | 'error'
  message?: string
  underlying?: string
  expiry?: string
  date?: string
  recorder_enabled?: boolean
  step_seconds?: number
  /** How open interest is quoted: quantity (NSE, BSE) or lots (MCX). */
  oi_unit?: 'quantity' | 'lots'
  spot?: number | null
  times?: string[]
  strikes?: number[]
  heat?: { ce: (number | null)[][]; pe: (number | null)[][] }
  heat_cls?: { ce: string[][]; pe: string[][] }
  totals?: { time: string; ce_change: number; pe_change: number; spot: number | null }[]
  ladder?: WriterFlowRow[]
  events?: WriterFlowEvent[]
}

export interface WriterFlowQuery {
  underlying: string
  expiry?: string
  step?: 'auto' | '60' | '180' | '300'
}

/** One thing the market read noticed, with the numbers behind it. */
export interface MarketReadEvent {
  id: string
  kind: string
  /** major: a turn worth an alert; info: context for the timeline. */
  level: 'major' | 'info'
  time: string
  epoch: number
  title: string
  reason: string
}

export interface MarketReadLevel {
  strike: number
  oi_lakh: number
  peak_lakh: number
  from_peak_pct: number
}

export interface MarketReadState {
  flow: {
    side: 'put' | 'call' | 'balanced' | null
    /** Which way the last 10 minutes lean now; differs from side while a turn is under way. */
    leaning: 'put' | 'call' | null
    since: string | null
    net_lakh: number
    threshold_lakh: number
    ce_lakh: number
    pe_lakh: number
    delta_net_lakh: number | null
    tone: -1 | 0 | 1
  }
  futures: {
    cls: string | null
    words: string | null
    since: string | null
    price_change: number | null
    oi_change_lakh: number | null
    tone: -1 | 0 | 1
  }
  levels: { support: MarketReadLevel | null; resistance: MarketReadLevel | null }
  volatility: {
    vix: number | null
    vix_from_open: number | null
    vix_10m: number | null
    atm_iv: number | null
    atm_iv_10m: number | null
  }
  churn: { traded_lakh: number; oi_change_lakh: number; changing_hands: boolean }
  others: Record<
    string,
    {
      side: 'put' | 'call' | 'balanced' | null
      since: string | null
      net: number
      threshold: number
    }
  >
  price: {
    spot: number
    open: number
    prev_close: number | null
    day_high: number
    day_low: number
  }
}

export interface MarketReadSettings {
  enabled: boolean
  telegram: boolean
  sound: boolean
}

export interface MarketReadResponse {
  status: 'success' | 'empty' | 'error'
  message?: string
  underlying?: string
  expiry?: string
  date?: string
  minute?: string
  headline?: string
  state?: MarketReadState
  events?: MarketReadEvent[]
  series?: { time: string; epoch: number; spot: number; net_lakh: number; threshold_lakh: number }[]
  settings?: MarketReadSettings
}

/** What the server sends as `market_read_update` after each minute it reads. */
export interface MarketReadUpdate {
  underlying: string
  minute: string
  headline: string
  events: MarketReadEvent[]
  /** Whether the trader wants a tone and a popup for major events. */
  sound: boolean
}

export const writerFlowApi = {
  sessions: async (): Promise<WriterFlowSessionsResponse> =>
    (await webClient.get<WriterFlowSessionsResponse>('/writerflow/api/sessions')).data,

  flow: async (q: WriterFlowQuery): Promise<WriterFlowResponse> =>
    (
      await webClient.get<WriterFlowResponse>('/writerflow/api/flow', {
        params: { underlying: q.underlying, expiry: q.expiry || undefined, step: q.step || 'auto' },
      })
    ).data,

  read: async (date?: string): Promise<MarketReadResponse> =>
    (
      await webClient.get<MarketReadResponse>('/writerflow/api/read', {
        params: { date: date || undefined },
      })
    ).data,

  readSettings: async (): Promise<{ status: string; settings: MarketReadSettings }> =>
    (await webClient.get('/writerflow/api/read/settings')).data,

  setReadSettings: async (
    changes: Partial<MarketReadSettings>
  ): Promise<{ status: string; settings?: MarketReadSettings; message?: string }> =>
    (await webClient.post('/writerflow/api/read/settings', changes)).data,
}
