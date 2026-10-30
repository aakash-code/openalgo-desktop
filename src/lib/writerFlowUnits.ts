/**
 * The unit Writer Flow shows open interest in, from the server's `oi_unit`.
 *
 * NSE and BSE options quote quantity, always a whole number of lots, shown in
 * lakh. MCX quotes lots, shown as they are. Never "contracts": the exchange
 * counts one lot as one contract, so quantity called contracts reads a lot
 * size times too high (65 for NIFTY).
 */
export function oiScale(unit: string | undefined): { divisor: number; word: string } {
  return unit === 'lots' ? { divisor: 1, word: 'lots' } : { divisor: 1e5, word: 'lakh qty' }
}
