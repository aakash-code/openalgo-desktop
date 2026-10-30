import { useQuery } from '@tanstack/react-query'
import type * as PlotlyTypes from 'plotly.js'
import { useEffect, useMemo, useState } from 'react'
import {
  type FlowClass,
  type WriterFlowResponse,
  type WriterFlowRow,
  type WriterFlowSide,
  writerFlowApi,
} from '@/api/writer-flow'
import { Card, CardContent } from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import Plot from '@/lib/PlotFlow'
import { oiScale } from '@/lib/writerFlowUnits'
import { useThemeStore } from '@/stores/themeStore'

/**
 * Writer Flow: what option writers did at each strike through the day, read
 * from the OI Flow recorder. Pictures first: a strike ladder of the current
 * streaks, a strike-by-time heatmap with the price path through it, and the
 * call and put totals per step. Colour is what a move means for the
 * underlying: green when call writers cover or put writers add, red the other
 * way round, grey when nothing is moving.
 */

const LABEL: Record<FlowClass, string> = {
  writing: 'Writing',
  covering: 'Covering',
  buying: 'Buying',
  unwinding: 'Unwinding',
  adding: 'Adding',
  reducing: 'Reducing',
  steady: 'Steady',
}
const CODE_LABEL: Record<string, string> = {
  W: 'Writing',
  C: 'Covering',
  B: 'Buying',
  U: 'Unwinding',
  A: 'Adding',
  R: 'Reducing',
  S: 'Steady',
  '': 'No data',
}
const STEP_CHOICES = [
  { value: 'auto', label: 'Auto step' },
  { value: '60', label: '1 min' },
  { value: '180', label: '3 min' },
  { value: '300', label: '5 min' },
] as const

function lakh(n: number): string {
  const sign = n < 0 ? '-' : n > 0 ? '+' : ''
  const a = Math.abs(n)
  if (a >= 1e7) return `${sign}${(a / 1e7).toFixed(2)}Cr`
  if (a >= 1e5) return `${sign}${(a / 1e5).toFixed(2)}L`
  if (a >= 1e3) return `${sign}${(a / 1e3).toFixed(1)}K`
  return `${sign}${Math.round(a)}`
}

function marketOpenNow(): boolean {
  const now = new Date(new Date().toLocaleString('en-US', { timeZone: 'Asia/Kolkata' }))
  const day = now.getDay()
  const mins = now.getHours() * 60 + now.getMinutes()
  return day >= 1 && day <= 5 && mins >= 9 * 60 + 15 && mins <= 15 * 60 + 31
}

export default function WriterFlow() {
  const { mode, appMode } = useThemeStore()
  const isAnalyzer = appMode === 'analyzer'
  const isDark = mode === 'dark' || isAnalyzer

  const colors = useMemo(
    () => ({
      bull: '#16a34a',
      bear: '#dc2626',
      quiet: isDark ? '#64748b' : '#94a3b8',
      neutral: isDark ? '#1e293b' : '#f1f5f9',
      text: isDark ? '#e2e8f0' : '#1f2937',
      grid: isDark ? 'rgba(148,163,184,0.15)' : 'rgba(100,116,139,0.18)',
      spot: isDark ? '#facc15' : '#0f172a',
      hoverBg: isDark ? (isAnalyzer ? '#2d2545' : '#1e293b') : '#ffffff',
    }),
    [isDark, isAnalyzer]
  )

  const sessionsQuery = useQuery({
    queryKey: ['writerflow-sessions'],
    queryFn: writerFlowApi.sessions,
    refetchInterval: 60_000,
  })
  const sessions = sessionsQuery.data?.sessions ?? []
  const today = sessions[0]?.trade_date
  const underlyings = useMemo(
    () => [...new Set(sessions.filter((s) => s.trade_date === today).map((s) => s.underlying))],
    [sessions, today]
  )

  const [underlying, setUnderlying] = useState('')
  const [step, setStep] = useState<'auto' | '60' | '180' | '300'>('auto')
  useEffect(() => {
    if (!underlying && underlyings.length)
      setUnderlying(underlyings.includes('NIFTY') ? 'NIFTY' : underlyings[0])
  }, [underlying, underlyings])

  const live = marketOpenNow()
  const flowQuery = useQuery({
    queryKey: ['writerflow', underlying, step],
    queryFn: () => writerFlowApi.flow({ underlying, step }),
    enabled: !!underlying,
    refetchInterval: live ? 60_000 : false,
  })
  const flow = flowQuery.data

  const baseLayout = useMemo<Partial<PlotlyTypes.Layout>>(
    () => ({
      paper_bgcolor: 'rgba(0,0,0,0)',
      plot_bgcolor: 'rgba(0,0,0,0)',
      font: { color: colors.text, size: 11 },
      hoverlabel: { bgcolor: colors.hoverBg, font: { color: colors.text } },
      margin: { l: 60, r: 20, t: 30, b: 40 },
      showlegend: false,
    }),
    [colors]
  )

  return (
    <div className="space-y-4 py-4">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-2xl font-bold">Writer Flow</h1>
        <Select value={underlying} onValueChange={setUnderlying}>
          <SelectTrigger className="w-40" aria-label="Underlying">
            <SelectValue placeholder="Underlying" />
          </SelectTrigger>
          <SelectContent>
            {underlyings.map((u) => (
              <SelectItem key={u} value={u}>
                {u}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={step} onValueChange={(v) => setStep(v as typeof step)}>
          <SelectTrigger className="w-32" aria-label="Step">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {STEP_CHOICES.map((s) => (
              <SelectItem key={s.value} value={s.value}>
                {s.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Legend colors={colors} />
        {flow?.status === 'success' && (
          <span className="ml-auto text-xs text-muted-foreground">
            {flow.underlying} {flow.expiry} | spot {flow.spot?.toFixed(1)} | step{' '}
            {(flow.step_seconds ?? 60) / 60} min | updated {flow.times?.at(-1)}
            {live ? ' | live' : ''}
          </span>
        )}
      </div>

      <FlowBody
        flow={flow}
        loading={sessionsQuery.isLoading || flowQuery.isLoading}
        failed={sessionsQuery.isError || flowQuery.isError}
        noSessions={sessionsQuery.isSuccess && sessions.length === 0}
        recorderOn={sessionsQuery.data?.recorder_enabled ?? false}
        colors={colors}
        baseLayout={baseLayout}
      />
    </div>
  )
}

function Legend({ colors }: { colors: { bull: string; bear: string; quiet: string } }) {
  const chip = (color: string, text: string) => (
    <span className="flex items-center gap-1 text-xs text-muted-foreground">
      <span className="inline-block h-3 w-3 rounded-sm" style={{ background: color }} />
      {text}
    </span>
  )
  return (
    <div className="flex flex-wrap items-center gap-3">
      {chip(colors.bull, 'Bullish: call writers covering, put writers adding')}
      {chip(colors.bear, 'Bearish: call writers adding, put writers covering')}
      {chip(colors.quiet, 'Steady')}
    </div>
  )
}

type Colors = {
  bull: string
  bear: string
  quiet: string
  neutral: string
  text: string
  grid: string
  spot: string
  hoverBg: string
}

function FlowBody({
  flow,
  loading,
  failed,
  noSessions,
  recorderOn,
  colors,
  baseLayout,
}: {
  flow: WriterFlowResponse | undefined
  loading: boolean
  failed: boolean
  noSessions: boolean
  recorderOn: boolean
  colors: Colors
  baseLayout: Partial<PlotlyTypes.Layout>
}) {
  if (loading) return <Message text="Loading the day's writer flow..." />
  if (failed)
    return (
      <Message text="Could not load Writer Flow. Check that OpenAlgo is running, then reload." />
    )
  if (noSessions)
    return (
      <Message
        text={
          recorderOn
            ? 'Nothing recorded yet. The recorder samples every minute while the market is open.'
            : 'The Writer Flow recorder is off. Set OI_FLOW_RECORDER_ENABLED=true in .env and restart OpenAlgo.'
        }
      />
    )
  if (flow?.status !== 'success') return <Message text={flow?.message || 'Choose an underlying.'} />

  return (
    <div className="grid gap-4 lg:grid-cols-5">
      <Card className="lg:col-span-2">
        <CardContent className="p-2">
          <Ladder flow={flow} colors={colors} baseLayout={baseLayout} />
        </CardContent>
      </Card>
      <Card className="lg:col-span-3">
        <CardContent className="p-2">
          <Heatmap flow={flow} colors={colors} baseLayout={baseLayout} />
        </CardContent>
      </Card>
      <Card className="lg:col-span-5">
        <CardContent className="p-2">
          <Totals flow={flow} colors={colors} baseLayout={baseLayout} />
        </CardContent>
      </Card>
    </div>
  )
}

function Message({ text }: { text: string }) {
  return (
    <Card>
      <CardContent className="py-16 text-center text-sm text-muted-foreground">{text}</CardContent>
    </Card>
  )
}

function biasColor(side: WriterFlowSide | null, c: Colors): string {
  if (!side || side.bias === 0) return c.quiet
  return side.bias > 0 ? c.bull : c.bear
}

function sideHover(name: string, row: WriterFlowRow, side: WriterFlowSide | null): string {
  if (!side) return `${row.strike} ${name}: no data`
  const w = side.window_change
  const win = ['10', '20']
    .map((k) => (w[k] == null ? '' : `${k}m ${lakh(w[k] as number)}`))
    .filter(Boolean)
    .join(', ')
  const head =
    side.cls === 'steady'
      ? `Steady for ${side.steady_minutes.toFixed(0)} min`
      : `${LABEL[side.cls]} for ${side.streak_minutes.toFixed(0)} min (${lakh(side.streak_oi_change)})`
  return `<b>${row.strike} ${name}</b><br>${head}<br>OI ${lakh(side.oi)}${win ? `<br>${win}` : ''}`
}

/** The current streak at every strike: calls to the left, puts to the right. */
function Ladder({
  flow,
  colors,
  baseLayout,
}: {
  flow: WriterFlowResponse
  colors: Colors
  baseLayout: Partial<PlotlyTypes.Layout>
}) {
  const rows = flow.ladder ?? []
  const y = rows.map((r) => r.strike)
  const active = (s: WriterFlowSide | null) => !!s && s.cls !== 'steady'
  const unit = oiScale(flow.oi_unit)
  const size = (s: WriterFlowSide | null) =>
    active(s) ? Math.abs(s!.streak_oi_change) / unit.divisor : 0
  const maxSize = Math.max(1, ...rows.map((r) => Math.max(size(r.ce), size(r.pe))))
  // Words only where a streak has lasted and moved enough to matter; every bar
  // labelled made the ladder unreadable. Hover still names the rest.
  const short = (s: WriterFlowSide | null) =>
    active(s) && s!.streak_minutes >= 5 && size(s) >= maxSize * 0.15
      ? `${LABEL[s!.cls]} ${s!.streak_minutes.toFixed(0)}m`
      : ''
  const quietDot = (side: 'ce' | 'pe') => {
    const quiet = rows.filter((r) => r[side] && r[side]!.cls === 'steady')
    return {
      type: 'scatter',
      mode: 'markers',
      x: quiet.map(() => (side === 'ce' ? -maxSize * 0.04 : maxSize * 0.04)),
      y: quiet.map((r) => r.strike),
      marker: {
        color: colors.quiet,
        size: quiet.map((r) => Math.min(18, 5 + r[side]!.steady_minutes / 2)),
      },
      hovertext: quiet.map((r) => sideHover(side === 'ce' ? 'CE' : 'PE', r, r[side])),
      hoverinfo: 'text',
    } as Partial<PlotlyTypes.PlotData>
  }
  const bars = (side: 'ce' | 'pe'): Partial<PlotlyTypes.PlotData> => ({
    type: 'bar',
    orientation: 'h',
    y,
    x: rows.map((r) => (side === 'ce' ? -size(r.ce) : size(r.pe))),
    marker: { color: rows.map((r) => biasColor(r[side], colors)) },
    text: rows.map((r) => short(r[side])),
    textposition: 'outside',
    cliponaxis: false,
    hovertext: rows.map((r) => sideHover(side === 'ce' ? 'CE' : 'PE', r, r[side])),
    hoverinfo: 'text',
  })
  const layout: Partial<PlotlyTypes.Layout> = {
    ...baseLayout,
    title: { text: 'Who is acting at each strike now', font: { size: 13 } },
    barmode: 'overlay',
    height: Math.max(420, rows.length * 18 + 80),
    xaxis: {
      title: { text: `${unit.word} moved in the current streak` },
      zeroline: true,
      zerolinecolor: colors.grid,
      gridcolor: colors.grid,
      // Room past the longest bar for its label, which sits outside it.
      range: [-maxSize * 1.6, maxSize * 1.6],
      tickvals: [-maxSize, -maxSize / 2, 0, maxSize / 2, maxSize],
      ticktext: [maxSize, maxSize / 2, 0, maxSize / 2, maxSize].map((v) => v.toFixed(1)),
    },
    yaxis: { gridcolor: colors.grid, dtick: rows.length > 20 ? 100 : 50, autorange: true },
    annotations: [
      {
        text: 'CALLS',
        x: -maxSize * 0.6,
        y: 1.02,
        yref: 'paper',
        showarrow: false,
        font: { size: 11 },
      },
      {
        text: 'PUTS',
        x: maxSize * 0.6,
        y: 1.02,
        yref: 'paper',
        showarrow: false,
        font: { size: 11 },
      },
    ],
    shapes:
      flow.spot != null
        ? [
            {
              type: 'line',
              xref: 'paper',
              x0: 0,
              x1: 1,
              y0: flow.spot,
              y1: flow.spot,
              line: { color: colors.spot, width: 1, dash: 'dot' },
            },
          ]
        : [],
  }
  return (
    <Plot
      data={[bars('ce'), bars('pe'), quietDot('ce'), quietDot('pe')]}
      layout={layout}
      config={{ displayModeBar: false, responsive: true }}
      style={{ width: '100%' }}
      useResizeHandler
    />
  )
}

/** Strikes by time, calls and puts side by side, the price path through both. */
function Heatmap({
  flow,
  colors,
  baseLayout,
}: {
  flow: WriterFlowResponse
  colors: Colors
  baseLayout: Partial<PlotlyTypes.Layout>
}) {
  const times = flow.times ?? []
  const strikes = flow.strikes ?? []
  const toLakh = (m: (number | null)[][]) =>
    m.map((row) => row.map((v) => (v == null ? null : v / 1e5)))
  const ce = toLakh(flow.heat?.ce ?? [])
  const pe = toLakh(flow.heat?.pe ?? [])
  // Scale to the 95th percentile, so one huge minute does not wash the rest out.
  const mags = [...ce.flat(), ...pe.flat()]
    .filter((v): v is number => v != null)
    .map(Math.abs)
    .sort((a, b) => a - b)
  const zmax = Math.max(0.5, mags[Math.floor(mags.length * 0.95)] ?? 1)
  const scale: [number, string][] = [
    [0, colors.bear],
    [0.5, colors.neutral],
    [1, colors.bull],
  ]
  const hover = (side: 'ce' | 'pe') =>
    strikes.map((k, i) =>
      times.map((t, j) => {
        const v = (side === 'ce' ? flow.heat?.ce : flow.heat?.pe)?.[i]?.[j]
        const code = (side === 'ce' ? flow.heat_cls?.ce : flow.heat_cls?.pe)?.[i]?.[j] ?? ''
        return `<b>${k} ${side.toUpperCase()}</b> ${t}<br>${CODE_LABEL[code]}${v ? ` ${lakh(Math.abs(v))}` : ''}`
      })
    )
  const heat = (
    side: 'ce' | 'pe',
    z: (number | null)[][],
    xaxis: string
  ): Partial<PlotlyTypes.PlotData> =>
    ({
      type: 'heatmap',
      x: times,
      y: strikes,
      z,
      zmin: -zmax,
      zmax,
      zmid: 0,
      colorscale: scale,
      showscale: false,
      xaxis,
      yaxis: 'y',
      hovertext: hover(side),
      hoverinfo: 'text',
      xgap: 0,
      ygap: 1,
    }) as unknown as Partial<PlotlyTypes.PlotData>
  const spotPath = (xaxis: string): Partial<PlotlyTypes.PlotData> =>
    ({
      type: 'scatter',
      mode: 'lines',
      x: times,
      y: (flow.totals ?? []).map((t) => t.spot),
      xaxis,
      yaxis: 'y',
      line: { color: colors.spot, width: 1.5 },
      hoverinfo: 'skip',
    }) as unknown as Partial<PlotlyTypes.PlotData>
  const events = flow.events ?? []
  const marks = (side: 'CE' | 'PE', xaxis: string): Partial<PlotlyTypes.PlotData> => {
    const own = events.filter((e) => e.side === side)
    return {
      type: 'scatter',
      mode: 'markers',
      x: own.map((e) => e.time),
      y: own.map((e) => e.strike),
      xaxis,
      yaxis: 'y',
      marker: {
        symbol: 'diamond',
        size: own.map((e) => 6 + e.minutes / 4),
        color: own.map((e) => (e.bias > 0 ? colors.bull : e.bias < 0 ? colors.bear : colors.quiet)),
        line: { color: colors.text, width: 1 },
      },
      hovertext: own.map(
        (e) =>
          `<b>${e.time} ${e.strike} ${e.side}</b><br>${LABEL[e.kind]} for ${e.minutes} min (${lakh(e.oi_change)})`
      ),
      hoverinfo: 'text',
    } as unknown as Partial<PlotlyTypes.PlotData>
  }
  const tickEvery = Math.max(1, Math.round(times.length / 8))
  const xaxisCommon = {
    type: 'category' as const,
    tickvals: times.filter((_, i) => i % tickEvery === 0),
    gridcolor: colors.grid,
    tickangle: 0,
  }
  const layout: Partial<PlotlyTypes.Layout> = {
    ...baseLayout,
    title: { text: 'Through the day: calls | puts, with the price path', font: { size: 13 } },
    height: Math.max(420, strikes.length * 18 + 80),
    xaxis: { ...xaxisCommon, domain: [0, 0.49], title: { text: 'Calls' } },
    xaxis2: { ...xaxisCommon, domain: [0.51, 1], title: { text: 'Puts' } },
    yaxis: { gridcolor: colors.grid },
  } as Partial<PlotlyTypes.Layout>
  return (
    <Plot
      data={[
        heat('ce', ce, 'x'),
        heat('pe', pe, 'x2'),
        spotPath('x'),
        spotPath('x2'),
        marks('CE', 'x'),
        marks('PE', 'x2'),
      ]}
      layout={layout}
      config={{ displayModeBar: false, responsive: true }}
      style={{ width: '100%' }}
      useResizeHandler
    />
  )
}

/** Call and put OI change per step across the strikes recorded, with the price line. */
function Totals({
  flow,
  colors,
  baseLayout,
}: {
  flow: WriterFlowResponse
  colors: Colors
  baseLayout: Partial<PlotlyTypes.Layout>
}) {
  const totals = flow.totals ?? []
  const unit = oiScale(flow.oi_unit)
  const x = totals.map((t) => t.time)
  const data: Partial<PlotlyTypes.PlotData>[] = [
    {
      type: 'bar',
      name: 'Call OI change',
      x,
      y: totals.map((t) => t.ce_change / unit.divisor),
      marker: { color: colors.bear },
      hovertemplate: '%{x}<br>Calls %{y:.2f}L<extra></extra>',
    },
    {
      type: 'bar',
      name: 'Put OI change',
      x,
      y: totals.map((t) => t.pe_change / unit.divisor),
      marker: { color: colors.bull },
      hovertemplate: '%{x}<br>Puts %{y:.2f}L<extra></extra>',
    },
    {
      type: 'scatter',
      mode: 'lines',
      name: flow.underlying,
      x,
      y: totals.map((t) => t.spot),
      yaxis: 'y2',
      line: { color: colors.spot, width: 1.5, dash: 'dot' },
      hovertemplate: `%{x}<br>${flow.underlying} %{y:.1f}<extra></extra>`,
    },
  ]
  const tickEvery = Math.max(1, Math.round(x.length / 12))
  const layout: Partial<PlotlyTypes.Layout> = {
    ...baseLayout,
    title: { text: 'Call and put OI change per step', font: { size: 13 } },
    height: 280,
    barmode: 'group',
    showlegend: true,
    legend: { orientation: 'h', y: 1.15, x: 1, xanchor: 'right' },
    xaxis: {
      type: 'category',
      tickvals: x.filter((_, i) => i % tickEvery === 0),
      gridcolor: colors.grid,
    },
    yaxis: { title: { text: unit.word }, gridcolor: colors.grid, zerolinecolor: colors.grid },
    yaxis2: { overlaying: 'y', side: 'right', showgrid: false, tickformat: ',.0f' },
    // Room for the price axis on the right, which a 5-digit index otherwise
    // clipped to "23."
    margin: { ...baseLayout.margin, r: 70 },
  }
  return (
    <Plot
      data={data}
      layout={layout}
      config={{ displayModeBar: false, responsive: true }}
      style={{ width: '100%' }}
      useResizeHandler
    />
  )
}
