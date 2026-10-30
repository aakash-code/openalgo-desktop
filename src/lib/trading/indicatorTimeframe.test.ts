/**
 * The per-indicator Timeframe wrapper, against the real chart library.
 *
 * The reference for every value is the higher chart itself: a 1h study on a 5m
 * chart must read, on every 5m bar, what the same study reads on the 1h chart
 * for the hour that bar falls in.
 */

import type { Bar, IndicatorCalcContext, IndicatorDescriptor } from 'openalgo-charts'
import * as core from 'openalgo-charts'
import { describe, expect, it } from 'vitest'

import {
  applyTimeframeToAll,
  buildHigherBars,
  hasOwnAttach,
  mapToChart,
  safeIntervalSeconds,
  withTimeframe,
} from './indicatorTimeframe'

const safeSeconds = (c: string) => safeIntervalSeconds(c) ?? 0

const ZONE = 'Asia/Kolkata'
// Monday 2026-09-28, 09:15 IST.
const DAY1 = Date.UTC(2026, 8, 28, 3, 45) / 1000

/** 5m bars for whole NSE sessions (09:15 to 15:30), closes a deterministic walk. */
function sessions(days: number): Bar[] {
  const out: Bar[] = []
  let p = 100
  for (let d = 0; d < days; d++) {
    for (let k = 0; k < 75; k++) {
      const time = DAY1 + d * 86_400 + k * 300
      const open = p
      p = p + Math.sin(out.length * 0.7) * 2 + 0.1
      out.push({
        time,
        open,
        high: Math.max(open, p) + 1,
        low: Math.min(open, p) - 1,
        close: p,
        volume: 10,
      })
    }
  }
  return out
}

/** What the broker's 1h chart holds: 09:15-anchored hours per session. */
function hourly(bars: readonly Bar[]): Bar[] {
  const groups = new Map<number, Bar[]>()
  for (const b of bars) {
    const dayStart = DAY1 + Math.floor((b.time - DAY1) / 86_400) * 86_400
    const key = dayStart + Math.floor((b.time - dayStart) / 3600) * 3600
    groups.set(key, [...(groups.get(key) ?? []), b])
  }
  return [...groups.values()].map((g) => core.mergeBars(g))
}

const ctx = (interval: string): IndicatorCalcContext =>
  ({
    interval,
    timezone: ZONE,
    barState: { isNew: false, isConfirmed: true, isRealtime: false, lastIndex: 0 },
  }) as IndicatorCalcContext

function smaStudy(extra: Partial<IndicatorDescriptor> = {}): IndicatorDescriptor {
  return {
    id: 'mtf-test-sma',
    name: 'MTF test SMA',
    placement: 'onchart',
    inputs: [{ key: 'length', type: 'number', label: 'Length', default: 3 }],
    plots: [{ key: 'sma', title: 'SMA', color: '#fff' }] as IndicatorDescriptor['plots'],
    calc(bars, settings) {
      const n = Number(settings.length)
      return {
        sma: bars.map((_, i) =>
          i + 1 < n ? null : bars.slice(i + 1 - n, i + 1).reduce((s, b) => s + b.close, 0) / n
        ),
      }
    },
    ...extra,
  }
}

function run(d: IndicatorDescriptor, bars: Bar[], fetched: Bar[] | null, tf: string) {
  const store: Record<string, unknown> = {}
  if (fetched) store.__mtf = { key: 'k', bars: fetched, coveredFrom: 0 }
  const values = d.calc(bars, { length: 3, tf }, store, ctx('5m'))
  return { values, store }
}

describe('withTimeframe', () => {
  it('adds a Timeframe input and is idempotent', () => {
    const w = withTimeframe(core, smaStudy())
    expect(w.inputs.at(-2)).toMatchObject({ key: 'tf', type: 'interval', default: '' })
    expect(w.inputs.at(-1)).toMatchObject({ key: 'tfWait', type: 'boolean', default: false })
    expect(withTimeframe(core, w)).toBe(w)
  })

  it('leaves a study that owns a timeframe input alone', () => {
    const own = smaStudy({
      inputs: [
        {
          key: 'htf',
          type: 'interval',
          label: 'HTF',
          default: '',
        } as IndicatorDescriptor['inputs'][number],
      ],
    })
    expect(withTimeframe(core, own)).toBe(own)
  })

  it('leaves a study that opts out with noTimeframe alone', () => {
    const snapshot = { ...smaStudy(), noTimeframe: true } as IndicatorDescriptor
    expect(withTimeframe(core, snapshot)).toBe(snapshot)
  })

  it('is the original calc, value for value, on the chart interval', () => {
    const bars = sessions(2)
    const plain = smaStudy()
    const w = withTimeframe(core, plain)
    expect(run(w, bars, null, '').values).toEqual(plain.calc(bars, { length: 3 }, {}, ctx('5m')))
    expect(run(w, bars, null, '5m').values).toEqual(plain.calc(bars, { length: 3 }, {}, ctx('5m')))
  })

  it('reads on every 5m bar what the 1h chart reads for that hour', () => {
    const bars = sessions(3)
    const h = hourly(bars)
    const expected = smaStudy().calc(h, { length: 3 }, {}, ctx('1h')).sma
    // The fetched history ends one hour early, so the last hour comes from the fold.
    const { values } = run(withTimeframe(core, smaStudy()), bars, h.slice(0, -1), '1h')
    const map = mapToChart(h, bars)
    for (let i = 0; i < bars.length; i++) {
      const want = expected[map[i]]
      if (want === null) expect(values.sma[i]).toBeNull()
      else expect(values.sma[i]).toBeCloseTo(want)
    }
    // A step: the twelve 5m bars of 12:15-13:15 on day 1 (past the 3-hour warmup) carry one value.
    const hour = values.sma.slice(36, 48)
    expect(new Set(hour).size).toBe(1)
    expect(hour[0]).not.toBeNull()
  })

  it('waits for the close: each hour shows the previous completed hour, fixed', () => {
    const bars = sessions(3)
    const h = hourly(bars)
    const w = withTimeframe(core, smaStudy())
    const store = { __mtf: { key: 'k', bars: h.slice(0, -1), coveredFrom: 0 } }
    const live = w.calc(bars, { length: 3, tf: '1h' }, store, ctx('5m')).sma
    const waited = w.calc(bars, { length: 3, tf: '1h', tfWait: true }, store, ctx('5m')).sma
    // Bar 48 opens the 13:15 hour on day 1: waiting shows 12:15's final value.
    expect(waited[48]).toBe(live[47])
    expect(waited.slice(48, 60).every((v) => v === live[47])).toBe(true)
    // The forming hour moves the live value, never the waited one.
    const moved = bars.slice()
    moved[moved.length - 1] = { ...moved.at(-1)!, close: moved.at(-1)!.close + 30 }
    const waitedAfter = w.calc(moved, { length: 3, tf: '1h', tfWait: true }, store, ctx('5m')).sma
    expect(waitedAfter.at(-1)).toBe(waited.at(-1))
  })

  it('moves the forming hour with the last 5m bar', () => {
    const bars = sessions(2)
    const h = hourly(bars)
    const w = withTimeframe(core, smaStudy())
    const before = run(w, bars, h.slice(0, -1), '1h').values.sma.at(-1)
    const moved = bars.slice()
    moved[moved.length - 1] = { ...moved.at(-1)!, close: moved.at(-1)!.close + 30 }
    const after = run(w, moved, h.slice(0, -1), '1h').values.sma.at(-1)
    expect(after! - before!).toBeCloseTo(10) // +30 on one of three closes
  })

  it('never reads past the chart in bar replay', () => {
    const all = sessions(2)
    const h = hourly(all)
    // Replay stopped at 10:20 on day 2: the fetched history still runs to the end.
    const cursor = all.findIndex((b) => b.time === DAY1 + 86_400 + 13 * 300)
    const shown = all.slice(0, cursor + 1)
    const w = withTimeframe(core, smaStudy())
    const v = run(w, shown, h, '1h').values.sma
    // The same as if the 1h history had only ever reached 10:20.
    const upTo = hourly(shown)
    const expected = smaStudy().calc(upTo, { length: 3 }, {}, ctx('1h')).sma.at(-1)
    expect(v.at(-1)).toBeCloseTo(expected as number)
    const leaked = smaStudy().calc(h, { length: 3 }, {}, ctx('1h')).sma[upTo.length - 1]
    expect(v.at(-1)).not.toBeCloseTo(leaked as number)
  })

  it('shows nothing while the higher bars are loading', () => {
    const { values } = run(withTimeframe(core, smaStudy()), sessions(1), null, '1h')
    expect(values.sma.every((v) => v === null)).toBe(true)
  })

  it('runs a lower timeframe on the chart bars and says why, never throws', () => {
    const w = withTimeframe(core, smaStudy())
    const bars = sessions(1)
    expect(run(w, bars, null, '1m').values).toEqual(
      smaStudy().calc(bars, { length: 3 }, {}, ctx('5m'))
    )
    const statuses: unknown[] = []
    w.attach!({
      settings: () => ({ length: 3, tf: '1m' }),
      interval: () => '5m',
      bars: () => bars,
      store: {},
      requestRecompute: () => {},
      setDataStatus: (s: unknown) => statuses.push(s),
    } as never)
    expect(statuses).toHaveLength(1)
    const st = statuses[0] as { state: string; error: Error }
    expect(st.state).toBe('error')
    expect(st.error).toBeInstanceOf(core.IndicatorInputError)
    expect(st.error.message).toMatch(/not higher than the chart's 5m/)
  })

  it('hands drawings the higher bars, so they land at the higher times', () => {
    let seen: readonly Bar[] = []
    const w = withTimeframe(
      core,
      smaStudy({
        draws: ({ bars }) => {
          seen = bars
          return []
        },
        barColors: ({ bars }) => bars.map((_, i) => (i % 2 ? 'red' : 'green')),
      })
    )
    const bars = sessions(2)
    const h = hourly(bars)
    const { values } = run(w, bars, h.slice(0, -1), '1h')
    w.draws!({ bars, values, settings: { tf: '1h' } })
    expect(seen.map((b) => b.time)).toEqual(h.map((b) => b.time))
    const colors = w.barColors!({ bars, values, settings: { tf: '1h' } })
    expect(colors).toHaveLength(bars.length)
    expect(new Set(colors.slice(12, 24)).size).toBe(1)
  })
})

describe('buildHigherBars', () => {
  it('keeps a fetched last period the chart only partly holds', () => {
    const bars = sessions(1)
    const h = hourly(bars)
    // Chart starts at 10:20, inside the 10:15 hour; that hour must not be rebuilt
    // from the five 5m bars the chart happens to hold.
    const partial = bars.slice(13)
    const out = buildHigherBars(core, h.slice(0, 2), partial, 3600, ZONE)
    expect(out[1]).toEqual(h[1])
    expect(out.map((b) => b.time)).toEqual(h.map((b) => b.time))
  })

  it('starts each session on its own first bar', () => {
    const bars = sessions(2)
    const out = buildHigherBars(core, hourly(bars).slice(0, 1), bars, 3600, ZONE)
    expect(out.map((b) => b.time)).toEqual(hourly(bars).map((b) => b.time))
  })
})

describe('calendar timeframes', () => {
  it('folds the forming month on a D chart by calendar month', () => {
    core.registerInterval({
      code: 'M',
      bucketing: { mode: 'calendar', unit: 'month', count: 1, timezone: ZONE },
    })
    // Daily bars, 10:00 IST, from 1 Jul to 30 Sep 2026.
    const days: Bar[] = []
    for (let t = Date.UTC(2026, 6, 1, 4, 30) / 1000; t < Date.UTC(2026, 9, 1) / 1000; t += 86_400) {
      days.push({ time: t, open: 1, high: 2, low: 0.5, close: days.length + 1, volume: 1 })
    }
    const monthStart = (y: number, m: number) => Date.UTC(y, m, 1) / 1000 - 19_800
    const fetched = [6, 7].map((m) => ({
      ...core.mergeBars(
        days.filter((d) => d.time >= monthStart(2026, m) && d.time < monthStart(2026, m + 1))
      ),
      time: monthStart(2026, m),
    }))
    const out = buildHigherBars(core, fetched, days, safeSeconds('M'), ZONE, 'M')
    expect(out).toHaveLength(3)
    expect(out[2].close).toBe(days.at(-1)!.close) // September, folded from the chart
    expect(out[1]).toEqual(fetched[1])
  })
})

describe('applyTimeframeToAll', () => {
  it('gives the built-in indicators a Timeframe setting', async () => {
    await import('openalgo-charts/indicators')
    applyTimeframeToAll(core)
    const ema = core.registeredIndicators().find((d) => d.id === 'ema')
    expect(ema?.inputs.some((i) => i.key === 'tf' && i.type === 'interval')).toBe(true)
    const count = core.registeredIndicators().length
    applyTimeframeToAll(core)
    expect(core.registeredIndicators().length).toBe(count)
  })

  it("replaces the library's own Timeframe on the 29 built-ins that have one", async () => {
    const lib = await import('openalgo-charts/indicators')
    const folded = lib.BUILTIN_INDICATORS.filter((d) => d.inputs.some((i) => i.key === 'timeframe'))
    expect(folded.length).toBeGreaterThan(0)
    applyTimeframeToAll(core)
    for (const { id } of folded) {
      const d = core.registeredIndicators().find((r) => r.id === id)
      const intervals = d?.inputs.filter((i) => i.type === 'interval').map((i) => i.key)
      expect([id, intervals]).toEqual([id, ['tf']])
    }
    // A `timeframe` left in an older saved layout must not fold the study.
    const ema = core.registeredIndicators().find((d) => d.id === 'ema') as IndicatorDescriptor
    const bars = sessions(3)
    const ctx = { interval: '5m', timezone: ZONE } as IndicatorCalcContext
    const settings = Object.fromEntries(ema.inputs.map((i) => [i.key, i.default]))
    expect(ema.calc(bars, { ...settings, timeframe: '1h' }, {}, ctx)).toEqual(
      ema.calc(bars, settings, {}, ctx)
    )
  })

  it("drops the library's history fetch with its Timeframe, so ours is the only one", async () => {
    // From openalgo-charts 2.6.7 those built-ins fetch their own higher history
    // in attach. Ours already does, so keeping theirs fetched twice, and it made
    // every one of them look like a study waiting on its own data: `sma` on four
    // bars no longer said it had nothing to plot.
    const lib = await import('openalgo-charts/indicators')
    const folded = lib.BUILTIN_INDICATORS.filter((d) => d.inputs.some((i) => i.key === 'timeframe'))
    expect(folded.some((d) => typeof d.attach === 'function')).toBe(true)
    applyTimeframeToAll(core)
    for (const { id } of folded) {
      const d = core.registeredIndicators().find((r) => r.id === id)
      expect([id, hasOwnAttach(d)]).toEqual([id, false])
    }
  })
})
