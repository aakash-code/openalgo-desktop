import { describe, expect, it } from 'vitest'
import type { OIProfileChainItem } from '@/api/oi-profile'
import { applyLiveOi, liveOiKey } from './oiProfileLive'

const row = (): OIProfileChainItem => ({
  strike: 25000,
  ce_oi: 100,
  pe_oi: 200,
  ce_oi_change: 0,
  pe_oi_change: 0,
  ce_base: 90,
  pe_base: 180,
  ce_symbol: 'NIFTY30OCT2625000CE',
  pe_symbol: 'NIFTY30OCT2625000PE',
})

describe('applyLiveOi', () => {
  it('overlays a fresh tick and recomputes change from base', () => {
    const live = new Map([[liveOiKey('NFO', 'NIFTY30OCT2625000CE'), { oi: 150, at: Date.now() }]])
    const { chain, liveLegs } = applyLiveOi([row()], live, 'NFO')
    expect(liveLegs).toBe(1)
    expect(chain[0].ce_oi).toBe(150)
    expect(chain[0].ce_oi_change).toBe(60) // 150 - 90
    expect(chain[0].pe_oi).toBe(200) // no tick -> polled kept
  })

  it('ignores a zero tick and a stale tick', () => {
    const live = new Map([
      [liveOiKey('NFO', 'NIFTY30OCT2625000CE'), { oi: 0, at: Date.now() }],
      [liveOiKey('NFO', 'NIFTY30OCT2625000PE'), { oi: 999, at: Date.now() - 60_000 }],
    ])
    const { chain, liveLegs } = applyLiveOi([row()], live, 'NFO', Date.now(), 10_000)
    expect(liveLegs).toBe(0)
    expect(chain[0].ce_oi).toBe(100)
    expect(chain[0].pe_oi).toBe(200)
  })
})
