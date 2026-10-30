/**
 * The tool registry, shared by the /tools page and the home page.
 *
 * Lives here rather than inside the page so the home page can state how many
 * tools exist without importing the page component -- and so the number is
 * derived rather than typed. A hardcoded count goes stale the moment a tool is
 * added, which is exactly how the previous "15+" survived three new tools.
 */
export interface Tool {
  title: string
  description: string
  href: string
  color: string
}

export const tools: Tool[] = [
  {
    title: 'Strategy Builder',
    description:
      'Build multi-leg option strategies with live Greeks, payoff diagram and what-if simulators',
    href: '/strategybuilder',
    color: 'bg-fuchsia-700',
  },
  {
    title: 'Strategy Portfolio',
    description: 'Saved strategies across MyTrades and Simulation watchlists',
    href: '/strategybuilder/portfolio',
    color: 'bg-violet-700',
  },
  // Desktop: Portfolio Backtester, SIP Backtester and Portfolio Analyzer removed (pandas, out of scope).
  {
    title: 'Sector Rotation',
    description:
      'Which NSE sectors, stocks and indices are leading, weakening, lagging or improving against a benchmark, with daily or weekly trails',
    href: '/sector-rotation',
    color: 'bg-cyan-700',
  },
  {
    title: 'Option Chain',
    description: 'Real-time option chain with live Greeks, OI data, and quick order placement',
    href: '/optionchain',
    color: 'bg-emerald-700',
  },
  {
    title: 'Option Greeks',
    description: 'Historical IV, Delta, Theta, Vega, and Gamma charts for ATM options',
    href: '/ivchart',
    color: 'bg-violet-700',
  },
  {
    title: 'OI Tracker',
    description: 'Open Interest analysis with CE/PE OI bars, PCR overlay, and ATM strike marker',
    href: '/oitracker',
    color: 'bg-blue-700',
  },
  {
    title: 'OI Range',
    description:
      'Open Interest by strike for a custom range with ATM-relative quick selectors and optional 1-minute auto-refresh',
    href: '/oirange',
    color: 'bg-sky-700',
  },
  {
    title: 'Max Pain',
    description: 'Max Pain strike calculation with visual pain distribution across strikes',
    href: '/maxpain',
    color: 'bg-amber-700',
  },
  {
    title: 'Straddle Chart',
    description:
      'Dynamic ATM Straddle chart with rolling strike, Spot, and Synthetic Futures overlay',
    href: '/straddle',
    color: 'bg-teal-700',
  },
  {
    title: 'Straddle PnL',
    description:
      'Simulated intraday ATM straddle P&L with automated N-point adjustments and trade log',
    href: '/straddlepnl',
    color: 'bg-orange-700',
  },
  {
    title: 'Vol Surface',
    description:
      '3D Implied Volatility surface across strikes and expiries using live option chain data',
    href: '/volsurface',
    color: 'bg-rose-700',
  },
  {
    title: 'GEX Dashboard',
    description: 'Gamma Exposure analysis with OI Walls, Net GEX per strike, and top gamma strikes',
    href: '/gex',
    color: 'bg-indigo-700',
  },
  {
    title: 'Strike Profile',
    description:
      'GEX and DEX heatmap by strike and expiry with per-expiry peaks, gamma flip, call and put walls',
    href: '/strikeprofile',
    color: 'bg-blue-600',
  },
  {
    title: 'Gamma Density',
    description:
      'Γ×OI density and convexity zones with intraday & to-expiry views, ATM IV, and ±1σ/±2σ expected-move bands',
    href: '/gammadensity',
    color: 'bg-lime-700',
  },
  {
    title: 'IV Smile',
    description: 'Implied Volatility smile with Call/Put IV curves, ATM IV, and skew analysis',
    href: '/ivsmile',
    color: 'bg-cyan-700',
  },
  {
    title: 'OI Profile',
    description: 'Futures candlestick with OI butterfly and daily OI change across strikes',
    href: '/oiprofile',
    color: 'bg-orange-700',
  },
  {
    title: 'Writer Flow',
    description:
      'Where option writers are adding, covering or holding, strike by strike through the day, with how long each has lasted',
    href: '/writerflow',
    color: 'bg-emerald-500',
  },
  {
    title: 'Arbitrage',
    description:
      'Realtime futures calendar-spread scanner across NFO & MCX, ranked by executable bid/ask spread % with one-click two-leg orders',
    href: '/arbitrage',
    color: 'bg-green-700',
  },
]
