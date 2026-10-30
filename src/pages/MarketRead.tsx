import { MarketReadPanel } from '@/components/trading/MarketReadPanel'

/**
 * A routed home for the Market Read panel: a once-a-minute plain-English read
 * of what option writers, futures and volatility are doing across the NIFTY
 * chain. The panel is self-contained (it fetches /writerflow/api/read and
 * subscribes to market_read_update), so the page just gives it a column.
 */
export default function MarketRead() {
  return (
    <div className="mx-auto w-full max-w-2xl p-4">
      <MarketReadPanel />
    </div>
  )
}
