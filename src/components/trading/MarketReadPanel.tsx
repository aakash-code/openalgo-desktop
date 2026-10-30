/**
 * The market read: what option writers, futures traders and volatility are
 * doing now, in sentences.
 *
 * A chart shows the price. What moved it is spread over every strike's open
 * interest, the futures' open interest, implied volatility, India VIX and two
 * other indices, which nobody can watch at once. The server reads all of them
 * every minute (services/market_read) and this panel shows the result: one
 * headline, the current state of each reading, and a timeline of what changed.
 *
 * It describes; it does not forecast. Measured on the days it was built from,
 * writer flow followed the price rather than led it, and the panel says so.
 */

import { Settings2 } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  type MarketReadEvent,
  type MarketReadResponse,
  type MarketReadSettings,
  type MarketReadState,
  writerFlowApi,
} from '@/api/writer-flow'
import { Button } from '@/components/ui/button'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { Switch } from '@/components/ui/switch'
import { onServerEvent } from '@/lib/serverEvents'
import { cn } from '@/lib/utils'
import { showToast } from '@/utils/toast'
import { PANEL_HEADER, PanelShell } from './panelShell'

/** Colour for a reading that supports a rise (1), weighs on price (-1) or neither. */
function toneClass(tone: number): string {
  if (tone > 0) return 'text-emerald-500'
  if (tone < 0) return 'text-rose-500'
  return 'text-muted-foreground'
}

function signed(value: number | null | undefined, digits = 1): string {
  if (value === null || value === undefined) return '-'
  const text = value.toFixed(digits)
  return value > 0 ? `+${text}` : text
}

function strike(value: number): string {
  return value.toLocaleString('en-IN', { maximumFractionDigits: 1 })
}

const SIDE_WORDS = { put: 'Put writers lead', call: 'Call writers lead', balanced: 'Balanced' }

/** The lead, and whether the last minutes still agree with it. */
function leadText(flow: MarketReadState['flow']): string {
  if (!flow.side) return 'No lead yet'
  const lead = SIDE_WORDS[flow.side]
  if (flow.side === 'balanced') return lead
  if (flow.leaning && flow.leaning !== flow.side) return `${lead}, ${flow.leaning}s taking over`
  if (!flow.leaning) return `${lead}, fading`
  return lead
}

function Reading({
  label,
  value,
  detail,
  tone = 0,
}: {
  label: string
  value: string
  detail?: string
  tone?: number
}) {
  return (
    <div className="min-w-0 rounded border px-2 py-1.5">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className={cn('text-[12px] font-medium leading-snug', toneClass(tone))}>{value}</div>
      {detail && <div className="text-[10px] leading-snug text-muted-foreground">{detail}</div>}
    </div>
  )
}

/** Net writer flow as bars under the price, so a trader sees which moved first. */
function FlowChart({ series }: { series: NonNullable<MarketReadResponse['series']> }) {
  const shape = useMemo(() => {
    if (series.length < 2) return null
    const width = 300
    const priceH = 60
    const flowH = 40
    const prices = series.map((s) => s.spot)
    const lo = Math.min(...prices)
    const hi = Math.max(...prices)
    const span = hi - lo || 1
    const step = width / (series.length - 1)
    const line = series
      .map(
        (s, i) =>
          `${i ? 'L' : 'M'}${(i * step).toFixed(1)},${(priceH - ((s.spot - lo) / span) * (priceH - 4) - 2).toFixed(1)}`
      )
      .join(' ')
    const peak = Math.max(1, ...series.map((s) => Math.max(Math.abs(s.net_lakh), s.threshold_lakh)))
    const mid = priceH + 4 + flowH / 2
    const scale = flowH / 2 / peak
    const bars = series.map((s, i) => ({
      x: i * step - step / 2,
      y: s.net_lakh >= 0 ? mid - s.net_lakh * scale : mid,
      h: Math.abs(s.net_lakh) * scale,
      up: s.net_lakh >= 0,
    }))
    const last = series[series.length - 1]
    return {
      width,
      height: priceH + 4 + flowH,
      line,
      bars,
      mid,
      band: last.threshold_lakh * scale,
      step,
      lo,
      hi,
    }
  }, [series])
  if (!shape) return null
  return (
    <figure className="px-2 pb-1">
      <svg
        viewBox={`0 0 ${shape.width} ${shape.height}`}
        className="h-[104px] w-full"
        role="img"
        aria-label="Price and net writer flow through the day"
        preserveAspectRatio="none"
      >
        <path
          d={shape.line}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.2}
          className="text-foreground"
          vectorEffect="non-scaling-stroke"
        />
        <line
          x1={0}
          x2={shape.width}
          y1={shape.mid}
          y2={shape.mid}
          stroke="currentColor"
          className="text-border"
          vectorEffect="non-scaling-stroke"
        />
        {[shape.mid - shape.band, shape.mid + shape.band].map((y, i) => (
          <line
            key={i === 0 ? 'upper' : 'lower'}
            x1={0}
            x2={shape.width}
            y1={y}
            y2={y}
            stroke="currentColor"
            strokeDasharray="2 3"
            className="text-muted-foreground/50"
            vectorEffect="non-scaling-stroke"
          />
        ))}
        {shape.bars.map((b) => (
          <rect
            key={b.x}
            x={b.x}
            y={b.y}
            width={Math.max(shape.step * 0.8, 0.5)}
            height={Math.max(b.h, 0.3)}
            className={b.up ? 'fill-emerald-500/70' : 'fill-rose-500/70'}
          />
        ))}
      </svg>
      <figcaption className="flex justify-between text-[10px] text-muted-foreground">
        <span>{series[0].time}</span>
        <span>Price; bars: puts minus calls written, last 10 min</span>
        <span>{series[series.length - 1].time}</span>
      </figcaption>
    </figure>
  )
}

function ReadSwitches({
  settings,
  onChange,
}: {
  settings: MarketReadSettings | null
  onChange(next: MarketReadSettings): void
}) {
  const [saving, setSaving] = useState(false)
  const change = async (name: keyof MarketReadSettings, on: boolean) => {
    setSaving(true)
    try {
      const r = await writerFlowApi.setReadSettings({ [name]: on })
      if (!r.settings) throw new Error(r.message)
      onChange(r.settings)
    } catch {
      showToast.error('Could not save the market read setting. Try again in a moment.')
    } finally {
      setSaving(false)
    }
  }
  const rows: [keyof MarketReadSettings, string, string][] = [
    ['enabled', 'Market read', 'Read the market every minute while the recorder runs.'],
    ['telegram', 'Telegram', 'Turns and level changes, at most one message every 10 minutes.'],
    [
      'sound',
      'Sound and popup',
      'A tone and a popup in this tab on a turn, even with the panel closed.',
    ],
  ]
  return (
    <div className="space-y-2">
      {rows.map(([name, label, help]) => (
        <div key={name}>
          <label className="flex items-center justify-between gap-3 text-[12px]">
            <span className="text-foreground">{label}</span>
            <Switch
              checked={settings?.[name] === true}
              disabled={settings === null || saving}
              onCheckedChange={(on) => change(name, on)}
              className="scale-90"
              aria-label={label}
            />
          </label>
          <p className="text-[10px] leading-snug text-muted-foreground">{help}</p>
        </div>
      ))}
    </div>
  )
}

function Timeline({ events }: { events: MarketReadEvent[] }) {
  const [all, setAll] = useState(false)
  const shown = useMemo(
    () =>
      [...events]
        .reverse()
        .filter(
          (e) =>
            all || e.level === 'major' || (e.kind !== 'support_at' && e.kind !== 'resistance_at')
        ),
    [events, all]
  )
  return (
    // Newest first, so a new event is added above what is on screen. With the
    // browser's scroll anchoring on, it shifted the list to keep the old rows
    // in place, and over a morning the newest turns slid out of sight
    // (21-Oct-2026: 1,381 px down, the heading and every event after 11:17
    // hidden). Off, a new event lands at the top where it is read.
    <section
      aria-label="What changed today"
      className="min-h-0 flex-1 overflow-y-auto border-t [overflow-anchor:none]"
    >
      <div className="flex items-center justify-between px-2 py-1">
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
          What changed
        </span>
        <button
          type="button"
          className="text-[10px] text-muted-foreground hover:text-foreground"
          onClick={() => setAll((v) => !v)}
        >
          {all ? 'Fewer' : 'Every level move'}
        </button>
      </div>
      {shown.length === 0 && (
        <p className="px-2 py-3 text-[12px] text-muted-foreground">
          Nothing has changed yet today.
        </p>
      )}
      <ol>
        {shown.map((e) => (
          <li key={e.id} className="border-b px-2 py-1.5 last:border-b-0">
            <div className="flex items-baseline gap-2">
              <span className="w-10 shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground">
                {e.time}
              </span>
              <span
                className={cn(
                  'text-[12px]',
                  e.level === 'major' ? 'font-medium text-foreground' : 'text-muted-foreground'
                )}
              >
                {e.title}
              </span>
            </div>
            <p className="ml-12 text-[11px] leading-snug text-muted-foreground">{e.reason}</p>
          </li>
        ))}
      </ol>
    </section>
  )
}

export function MarketReadPanel() {
  const [read, setRead] = useState<MarketReadResponse | null>(null)
  const [settings, setSettings] = useState<MarketReadSettings | null>(null)
  const [failed, setFailed] = useState(false)

  const load = useCallback(async () => {
    try {
      const r = await writerFlowApi.read()
      setRead(r)
      if (r.settings) setSettings(r.settings)
      setFailed(r.status === 'error')
    } catch {
      setFailed(true)
    }
  }, [])

  useEffect(() => {
    void load()
    // The server says when a minute is read; the poll only covers a missed event.
    const off = onServerEvent('market_read_update', () => void load())
    const timer = window.setInterval(() => void load(), 60_000)
    return () => {
      off()
      window.clearInterval(timer)
    }
  }, [load])

  const state = read?.state
  const others = state ? Object.entries(state.others) : []
  const ownSide = state?.flow.side

  return (
    <PanelShell
      id="oa-panel-marketread"
      label="Market read"
      storageKey="oa-trading-marketread-width"
      defaultWidth={360}
    >
      <div className={PANEL_HEADER}>
        <span className="text-[12px] font-medium">Market read</span>
        {read?.underlying && (
          <span className="text-[11px] text-muted-foreground">
            {read.underlying} {read.minute ? `at ${read.minute}` : ''}
          </span>
        )}
        <div className="ml-auto" />
        <Popover>
          <PopoverTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8 shrink-0"
              title="Market read settings"
              aria-label="Market read settings"
            >
              <Settings2 className="h-3.5 w-3.5" />
            </Button>
          </PopoverTrigger>
          <PopoverContent align="end" className="w-64 p-3">
            <ReadSwitches settings={settings} onChange={setSettings} />
          </PopoverContent>
        </Popover>
      </div>

      {failed && !state && (
        <p className="px-2 py-3 text-[12px] text-muted-foreground">
          Could not read the market just now. It tries again every minute.
        </p>
      )}
      {read?.status === 'empty' && (
        <p className="px-2 py-3 text-[12px] text-muted-foreground">{read.message}</p>
      )}

      {state && read?.headline && (
        <>
          <p className="px-2 pt-2 text-[13px] leading-snug text-foreground" aria-live="polite">
            {read.headline}
          </p>
          <p className="px-2 pb-2 text-[10px] text-muted-foreground">
            What traders are doing now. It is not a forecast: on the days measured, this flow
            followed the price rather than led it.
          </p>

          <div className="grid grid-cols-2 gap-1.5 px-2 pb-2">
            <Reading
              label="Option writers"
              value={leadText(state.flow)}
              detail={`${state.flow.since ? `since ${state.flow.since}; ` : ''}last 10 min: puts ${signed(state.flow.pe_lakh)}, calls ${signed(state.flow.ce_lakh)} lakh`}
              tone={state.flow.tone}
            />
            <Reading
              label="Futures"
              value={state.futures.words ? state.futures.words : 'Waiting for quotes'}
              detail={
                state.futures.price_change !== null
                  ? `5 min: price ${signed(state.futures.price_change, 0)}, OI ${signed(state.futures.oi_change_lakh, 2)} lakh`
                  : undefined
              }
              tone={state.futures.tone}
            />
            <Reading
              label="Support"
              value={state.levels.support ? strike(state.levels.support.strike) : '-'}
              detail={
                state.levels.support
                  ? `puts ${state.levels.support.oi_lakh} lakh, ${signed(state.levels.support.from_peak_pct, 0)}% from peak`
                  : undefined
              }
              tone={state.levels.support && state.levels.support.from_peak_pct <= -10 ? -1 : 0}
            />
            <Reading
              label="Resistance"
              value={state.levels.resistance ? strike(state.levels.resistance.strike) : '-'}
              detail={
                state.levels.resistance
                  ? `calls ${state.levels.resistance.oi_lakh} lakh, ${signed(state.levels.resistance.from_peak_pct, 0)}% from peak`
                  : undefined
              }
              tone={state.levels.resistance && state.levels.resistance.from_peak_pct <= -10 ? 1 : 0}
            />
            <Reading
              label="Volatility"
              value={
                state.volatility.vix !== null
                  ? `India VIX ${state.volatility.vix.toFixed(2)}`
                  : 'Waiting for quotes'
              }
              detail={[
                state.volatility.vix_from_open !== null
                  ? `${signed(state.volatility.vix_from_open, 2)} on the day`
                  : null,
                state.volatility.atm_iv !== null
                  ? `IV at the money ${state.volatility.atm_iv.toFixed(1)}%`
                  : null,
              ]
                .filter(Boolean)
                .join(', ')}
              tone={state.volatility.vix_10m !== null && state.volatility.vix_10m >= 0.25 ? -1 : 0}
            />
            <Reading
              label="Other indices"
              value={
                others.length === 0
                  ? 'Not recorded'
                  : ownSide && others.every(([, o]) => o.side === ownSide)
                    ? 'Agree'
                    : others.map(([name, o]) => `${name} ${o.side ?? 'no lead'}`).join(', ')
              }
              detail={others
                .map(
                  ([name, o]) =>
                    `${name}: ${o.side ? `${o.side}-led` : 'no lead'}${o.since ? ` since ${o.since}` : ''}`
                )
                .join('; ')}
            />
          </div>
          {state.churn.changing_hands && (
            <p className="px-2 pb-2 text-[11px] text-muted-foreground">
              At the money, {state.churn.traded_lakh} lakh traded in 5 minutes against{' '}
              {state.churn.oi_change_lakh} lakh of open interest change: positions are changing
              hands rather than opening.
            </p>
          )}
          {read.series && <FlowChart series={read.series} />}
        </>
      )}

      {read?.events && <Timeline events={read.events} />}
    </PanelShell>
  )
}
