/**
 * A per-indicator Timeframe setting, the way charting terminals offer one on
 * every study: a 1h EMA on a 5m chart plots the 1h values as a step, the 1h bar still
 * forming updates live, and anything a study draws lands at its real time.
 *
 * Done on the host, once, for every registered descriptor (built-ins, the
 * operator's own modules and OpenScript studies), so no indicator has to know.
 * The descriptor is wrapped: its `calc` runs on higher-timeframe bars and each
 * output column is spread back over the chart bars inside each period.
 *
 * Only coarser timeframes are accepted, as charting terminals do. A past period shows
 * its final value across all of its chart bars, which is what the higher chart
 * itself shows; the period still forming is folded from the chart's own bars,
 * so it moves with every tick without a refetch.
 */
import type {
  Bar,
  IndicatorAttachContext,
  IndicatorCalcContext,
  IndicatorDescriptor,
  IndicatorSettings,
  IndicatorValues,
} from 'openalgo-charts'
import { bucketStartOf, intervalToSeconds, tryResolveInterval } from 'openalgo-charts'

import { ensureCalendarIntervals } from '@/lib/chart/intervalRegistry'

type Core = Pick<
  typeof import('openalgo-charts'),
  | 'registeredIndicators'
  | 'registerIndicator'
  | 'intervalToSeconds'
  | 'mergeBars'
  | 'zonedDayIndex'
  | 'zonedWeekIndex'
  | 'IndicatorInputError'
>

/** The setting this adds. A descriptor with its own input of that key opts out. */
export const TF_KEY = 'tf'
/** "Wait for timeframe closes": show only completed periods. */
export const TF_WAIT_KEY = 'tfWait'

const WRAPPED = Symbol.for('openalgo.indicatorTimeframe')
// The descriptor a wrapped one was built from, so a caller can ask about the
// study itself rather than about the Timeframe wrapper around it.
const INNER = Symbol.for('openalgo.indicatorTimeframe.inner')
const STORE_KEY = '__mtf'
/**
 * Whether the last calc ran on transformed bars (Renko and the like), and the
 * attach's sync to rerun when that changes. Only calc is told, attach is not.
 */
const TRANSFORMED_KEY = '__mtfTransformed'
const RESYNC_KEY = '__mtfResync'

/** Higher-timeframe bars a study needs before the first chart bar: warmup. */
const WARMUP_BARS = 300
/** One Indian equity session (09:15 to 15:30), for sizing the warmup window. */
const SESSION_SECONDS = 22_500

interface MtfState {
  /** What the bars were fetched for; a change of any part refetches. */
  key: string
  /** Null while the request is in flight. */
  bars: readonly Bar[] | null
  /** The chart's first bar when fetched; older chart history refetches. */
  coveredFrom: number
}

/** What the drawing hooks need, keyed by the values object `calc` returned. */
const htfContext = new WeakMap<
  object,
  { bars: readonly Bar[]; values: IndicatorValues; map: Int32Array }
>()

type Hook = (ctx: {
  bars: readonly Bar[]
  values: IndicatorValues
  settings: Readonly<IndicatorSettings>
}) => unknown

function tfOf(settings: Readonly<IndicatorSettings>): string {
  const tf = (settings as Record<string, unknown>)[TF_KEY]
  return typeof tf === 'string' ? tf.trim() : ''
}

/** Seconds in an interval code, or null for one the engine cannot time. */
export function safeIntervalSeconds(code: string | undefined): number | null {
  return secondsOf({ intervalToSeconds } as Core, code)
}

const CALENDAR_DAYS = { month: 30.44, quarter: 91.31, year: 365.25 } as const

/** The calendar bucketing of `M`, `Q`, `Y` (registered by the app), else null. */
function calendarOf(code: string | undefined) {
  const b = code ? tryResolveInterval(code)?.bucketing : undefined
  return b?.mode === 'calendar' ? b : null
}

function secondsOf(core: Core, code: string | undefined): number | null {
  if (!code) return null
  try {
    const s = core.intervalToSeconds(code)
    if (Number.isFinite(s) && s > 0) return s
  } catch {
    // a calendar code has no fixed length; sized below
  }
  // Months, quarters and years vary in length: an average is enough to compare
  // against the chart and to size the warmup, and periods are cut by calendar.
  const cal = calendarOf(code)
  return cal ? CALENDAR_DAYS[cal.unit] * (cal.count ?? 1) * 86_400 : null
}

/**
 * The period a bar time belongs to, as a comparable number. Days and weeks go
 * by the chart's calendar; anything shorter is anchored on the first bar of
 * each session, so a 1h period on NSE runs 09:15 to 10:15 like the 1h chart.
 */
function periodKeyer(core: Core, tfSec: number, zone: string | undefined, code?: string) {
  const cal = calendarOf(code)
  if (cal) return (t: number) => bucketStartOf(cal, t, zone)
  if (tfSec >= 604_800) return (t: number) => core.zonedWeekIndex(t, zone)
  if (tfSec >= 86_400) {
    const days = Math.round(tfSec / 86_400)
    return (t: number) => Math.floor(core.zonedDayIndex(t, zone) / days)
  }
  let day = Number.NaN
  let anchor = 0
  // Stateful: fed bar times in ascending order, one session after another.
  return (t: number) => {
    const d = core.zonedDayIndex(t, zone)
    if (d !== day) {
      day = d
      anchor = t
    }
    return anchor + Math.floor((t - anchor) / tfSec) * tfSec
  }
}

/**
 * Fetched higher-timeframe history, with the period(s) the chart's own bars
 * cover rebuilt from those bars, so the forming period is live.
 */
export function buildHigherBars(
  core: Core,
  fetched: readonly Bar[],
  chart: readonly Bar[],
  tfSec: number,
  zone?: string,
  code?: string
): Bar[] {
  if (fetched.length === 0 || chart.length === 0) return fetched.slice()
  const last = fetched[fetched.length - 1]
  const tail = chart.filter((b) => b.time >= last.time)
  if (tail.length === 0) return fetched.slice()

  // The keyer is stateful (it anchors on each session's first bar), so every
  // time goes through it exactly once, in ascending order.
  const key = periodKeyer(core, tfSec, zone, code)
  const lastKey = key(last.time)
  const groups: { key: number; bars: Bar[] }[] = []
  for (const b of tail) {
    const k = key(b.time)
    if (groups.length === 0 || groups[groups.length - 1].key !== k)
      groups.push({ key: k, bars: [] })
    groups[groups.length - 1].bars.push(b)
  }

  const out = fetched.slice(0, -1)
  // The last fetched period is rebuilt from chart bars only when the chart
  // holds all of it; otherwise the fetched bar stands.
  const rebuildLast = groups[0].key === lastKey && chart[0].time <= last.time
  if (!rebuildLast) out.push(last)
  for (const g of groups) {
    if (g.key === lastKey && !rebuildLast) continue
    const merged = core.mergeBars(g.bars)
    // A rebuilt period keeps the broker's own start time, so anything anchored
    // to it (a drawing, a marker) stays where the higher chart puts it.
    out.push(g.key === lastKey ? { ...merged, time: last.time } : merged)
  }
  return out
}

/** For each chart bar, the index of the higher bar it falls in, or -1. */
export function mapToChart(higher: readonly Bar[], chart: readonly Bar[]): Int32Array {
  const map = new Int32Array(chart.length).fill(-1)
  let j = -1
  for (let i = 0; i < chart.length; i++) {
    const t = chart[i].time
    while (j + 1 < higher.length && higher[j + 1].time <= t) j++
    map[i] = j
  }
  return map
}

function spread<T>(column: readonly T[], map: Int32Array, empty: T): T[] {
  const out = new Array<T>(map.length)
  for (let i = 0; i < map.length; i++) {
    const j = map[i]
    out[i] = j >= 0 && j < column.length ? (column[j] ?? empty) : empty
  }
  return out
}

function spreadValues(values: IndicatorValues, map: Int32Array): IndicatorValues {
  const out: Record<string, (number | null)[]> = {}
  for (const k of Object.keys(values)) out[k] = spread(values[k], map, null)
  return out
}

function nullValues(values: IndicatorValues, n: number): IndicatorValues {
  const out: Record<string, (number | null)[]> = {}
  for (const k of Object.keys(values)) out[k] = new Array(n).fill(null)
  return out
}

/**
 * Set `noTimeframe: true` on a descriptor whose picture is not a series over
 * time, such as OI Profile (the option chain now, pinned to the right edge).
 * A Timeframe there would fetch higher bars for nothing and change nothing.
 */
function hasOwnTimeframe(d: IndicatorDescriptor): boolean {
  if ((d as { noTimeframe?: unknown }).noTimeframe === true) return true
  return d.inputs.some((i) => i.key === TF_KEY || i.type === 'interval')
}

/** The input openalgo-charts 2.6.0 put on 29 built-ins (`withTimeframe` there). */
const LIB_TF_KEY = 'timeframe'

/**
 * `d` without the library's own Timeframe, so ours wraps it instead. The
 * library folds only the bars already on the chart, so a daily EMA(200) on a
 * 5m chart has a few dozen daily bars to work with, and it never shows the
 * period still forming. Ours fetches the higher history and does both. The
 * study runs as if the input were empty, which the library defines as exactly
 * the study it was, so a value left in an older saved layout cannot fold it.
 *
 * From 2.6.7 the library's Timeframe also fetches its own higher history in
 * `attach`, on exactly those built-ins and no others. That goes too: ours
 * fetches it already, and leaving theirs made every one of them read as a
 * study waiting on data it fetches itself (`hasOwnAttach`).
 */
function withoutLibraryTimeframe(d: IndicatorDescriptor): IndicatorDescriptor {
  if (!d.inputs.some((i) => i.key === LIB_TF_KEY && i.type === 'interval')) return d
  const plain = (s: Readonly<IndicatorSettings>) => ({ ...s, [LIB_TF_KEY]: '' })
  const { attach: _libraryHistory, ...rest } = d
  const tail = d.calcTail
  return {
    ...rest,
    inputs: d.inputs.filter((i) => i.key !== LIB_TF_KEY),
    calc: (bars, settings, store, ctx) => d.calc(bars, plain(settings), store, ctx),
    ...(tail && {
      calcTail: (bars, settings, fromIndex, previous, store, ctx) =>
        tail(bars, plain(settings), fromIndex, previous, store, ctx),
    }),
  }
}

/** `d` with a Timeframe input; the same object when it opts out or is wrapped. */
export function withTimeframe(core: Core, original: IndicatorDescriptor): IndicatorDescriptor {
  if ((original as unknown as Record<symbol, unknown>)[WRAPPED]) return original
  const d = withoutLibraryTimeframe(original)
  if (hasOwnTimeframe(d)) return original

  const refuse = (tf: string, chart: string) =>
    new core.IndicatorInputError(
      `Timeframe ${tf} is not higher than the chart's ${chart}. Pick a higher timeframe, or set it back to Chart interval.`
    )

  /**
   * The active higher timeframe; null when the study runs on the chart's own;
   * `refused` when the chart is at or above it. A refusal never throws: a
   * layout restored onto a coarser chart must still load, so the study runs on
   * the chart's own bars and says why beside its name.
   */
  const active = (
    settings: Readonly<IndicatorSettings>,
    chartInterval: string | undefined
  ): { tf: string; tfSec: number } | { refused: Error } | null => {
    const tf = tfOf(settings)
    if (!tf || tf === chartInterval) return null
    const tfSec = secondsOf(core, tf)
    const chartSec = secondsOf(core, chartInterval)
    if (tfSec === null || chartSec === null) return null
    if (tfSec <= chartSec) return { refused: refuse(tf, chartInterval ?? '') }
    return { tf, tfSec }
  }

  const wrapped: IndicatorDescriptor = {
    ...d,
    inputs: [
      ...d.inputs,
      {
        key: TF_KEY,
        type: 'interval',
        label: 'Timeframe',
        default: '',
        group: 'Timeframe',
        tooltip:
          "Calculate on a higher timeframe and show it on this chart. Chart interval keeps it on this chart's own bars.",
      } as IndicatorDescriptor['inputs'][number],
      {
        key: TF_WAIT_KEY,
        type: 'boolean',
        label: 'Wait for timeframe closes',
        default: false,
        group: 'Timeframe',
        tooltip:
          'Show each higher-timeframe value only once its period has closed, so it never changes while the period is forming.',
      } as IndicatorDescriptor['inputs'][number],
    ],

    attach(ctx: IndicatorAttachContext) {
      const teardown = d.attach?.(ctx)
      let unsubscribe: (() => void) | undefined
      let aborter: AbortController | null = null
      let refused = false

      const sync = () => {
        const store = ctx.store as Record<string, unknown>
        store[RESYNC_KEY] = sync
        let decision = active(ctx.settings(), ctx.interval?.())
        // Renko and other transformed bars do not close on the clock, so they
        // have no higher period to sit in. The library refuses the same way.
        if (decision && !('refused' in decision) && store[TRANSFORMED_KEY] === true) {
          aborter?.abort()
          decision = {
            refused: new core.IndicatorInputError(
              `${d.name}: this chart draws transformed bars, which a timeframe cannot fold; compute the study on the underlying bars`
            ),
          }
        }
        if (decision && 'refused' in decision) {
          delete store[STORE_KEY]
          refused = true
          ctx.setDataStatus?.({ state: 'error', error: decision.refused })
          return
        }
        if (refused) {
          refused = false
          ctx.setDataStatus?.({ state: 'ready' })
        }
        const want = decision
        if (!want) {
          delete store[STORE_KEY]
          return
        }
        const bars = ctx.bars()
        const symbol = ctx.symbol?.() ?? ctx.dataContext?.()?.symbol
        if (!symbol || bars.length === 0 || !ctx.requestBars) return
        const exchange = ctx.dataContext?.()?.exchange
        const key = `${symbol}|${exchange ?? ''}|${ctx.interval?.() ?? ''}|${want.tf}`
        const have = store[STORE_KEY] as MtfState | undefined
        if (have && have.key === key && bars[0].time >= have.coveredFrom) return

        const span =
          want.tfSec >= 86_400
            ? WARMUP_BARS * want.tfSec * 1.5
            : Math.ceil(WARMUP_BARS / Math.max(1, SESSION_SECONDS / want.tfSec)) * 86_400 * 1.6
        const state: MtfState = { key, bars: null, coveredFrom: bars[0].time }
        store[STORE_KEY] = state
        aborter?.abort()
        aborter = new AbortController()
        const signal = aborter.signal
        ctx.signal?.addEventListener('abort', () => aborter?.abort(), { once: true })
        ctx.setDataStatus?.({ state: 'loading' })
        // Up to the later of the clock and the chart's last bar: a replay or a
        // clock behind the feed must not cut the higher history short.
        const now = Math.max(
          ctx.now?.() ?? Math.floor(Date.now() / 1000),
          bars[bars.length - 1].time
        )
        ctx
          .requestBars({
            symbol,
            exchange,
            interval: want.tf,
            from: Math.floor(bars[0].time - span),
            to: now + want.tfSec,
            signal,
          })
          .then((fetched) => {
            if (signal.aborted || store[STORE_KEY] !== state) return
            state.bars = fetched
            ctx.setDataStatus?.({ state: fetched.length > 0 ? 'ready' : 'empty' })
            ctx.requestRecompute()
          })
          .catch((error) => {
            if (signal.aborted || store[STORE_KEY] !== state) return
            delete store[STORE_KEY]
            ctx.setDataStatus?.({
              state: 'error',
              // An input error so the terminal shows this sentence as written.
              error: Object.assign(
                new core.IndicatorInputError(
                  `Could not load ${want.tf} history for this indicator. Check the broker is logged in, then reload the chart.`
                ),
                { cause: error }
              ),
            })
            ctx.setDataRetry?.(() => sync())
          })
      }

      sync()
      unsubscribe = ctx.subscribeDataChanges?.(() => sync())
      return () => {
        aborter?.abort()
        unsubscribe?.()
        teardown?.()
      }
    },

    calc(bars, settings, store, ctx?: IndicatorCalcContext) {
      const want = active(settings, ctx?.interval)
      if (!want || 'refused' in want) return d.calc(bars, settings, store, ctx)
      const s = store as Record<string, unknown>
      const transformed = ctx?.transformed === true
      if (s[TRANSFORMED_KEY] !== transformed) {
        s[TRANSFORMED_KEY] = transformed
        ;(s[RESYNC_KEY] as (() => void) | undefined)?.()
      }
      if (transformed) return nullValues(d.calc(bars, settings, store, ctx), bars.length)
      const state = (store as Record<string, unknown>)[STORE_KEY] as MtfState | undefined
      if (!state?.bars) {
        // Never flash the chart-timeframe values while the higher bars load.
        return nullValues(d.calc(bars, settings, store, ctx), bars.length)
      }
      // Nothing past the chart's last bar: in bar replay the fetched history runs
      // to today, and the period under the replay cursor would otherwise show
      // its final value, i.e. the future. Cut there and let the fold rebuild it.
      const end = bars.length > 0 ? bars[bars.length - 1].time : Number.POSITIVE_INFINITY
      const known = state.bars.filter((b) => b.time <= end)
      const higher = buildHigherBars(core, known, bars, want.tfSec, ctx?.timezone, want.tf)
      const last = higher.length - 1
      const hctx: IndicatorCalcContext | undefined = ctx && {
        ...ctx,
        interval: want.tf,
        barState: { ...ctx.barState, lastIndex: last, isNew: false },
      }
      const values = d.calc(higher, settings, store, hctx)
      let map = mapToChart(higher, bars)
      // Waiting for the close reads the period before the one each bar is in:
      // a value appears on the first bar after its period ends and never moves.
      if ((settings as Record<string, unknown>)[TF_WAIT_KEY] === true) map = map.map((j) => j - 1)
      const out = spreadValues(values, map)
      htfContext.set(out, { bars: higher, values, map })
      return out
    },
  }

  if (d.calcTail) {
    const tail = d.calcTail
    wrapped.calcTail = (bars, settings, fromIndex, previous, store, ctx) => {
      if (tfOf(settings)) return null
      return tail(bars, settings, fromIndex, previous, store, ctx)
    }
  }

  // Anchored by time, so given the higher bars they land where they belong.
  for (const name of ['draws', 'markers', 'table'] as const) {
    const hook = d[name] as Hook | undefined
    if (!hook) continue
    ;(wrapped as unknown as Record<string, Hook>)[name] = (ctx) => {
      const h = htfContext.get(ctx.values)
      return hook(h ? { ...ctx, bars: h.bars, values: h.values } : ctx)
    }
  }
  // One entry per bar, so computed on the higher bars and spread back.
  for (const name of ['background', 'barColors'] as const) {
    const hook = d[name] as Hook | undefined
    if (!hook) continue
    ;(wrapped as unknown as Record<string, Hook>)[name] = (ctx) => {
      const h = htfContext.get(ctx.values)
      if (!h) return hook(ctx)
      const column = hook({ ...ctx, bars: h.bars, values: h.values }) as readonly (string | null)[]
      return spread(column, h.map, null)
    }
  }

  ;(wrapped as unknown as Record<symbol, unknown>)[WRAPPED] = true
  ;(wrapped as unknown as Record<symbol, unknown>)[INNER] = d
  return wrapped
}

/**
 * Whether the study itself has an `attach` hook, which is how a study that
 * fetches its own data (Writer Flow Totals) starts. Every wrapped descriptor
 * has one for the Timeframe input, so asking the wrapper says yes for `sma`.
 */
export function hasOwnAttach(descriptor: { attach?: unknown } | undefined): boolean {
  if (!descriptor) return false
  const inner = (descriptor as unknown as Record<symbol, { attach?: unknown } | undefined>)[INNER]
  return typeof (inner ?? descriptor).attach === 'function'
}

/** Wrap every registered descriptor not yet wrapped. Safe to call repeatedly. */
export function applyTimeframeToAll(core: Core): void {
  // `M`, `Q` and `Y` are calendar periods the engine only knows once registered;
  // /trading never needed them before, since its own bars come from the broker.
  ensureCalendarIntervals()
  for (const d of core.registeredIndicators()) {
    const w = withTimeframe(core, d)
    if (w !== d) core.registerIndicator(w)
  }
}
