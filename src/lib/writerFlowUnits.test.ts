import { describe, expect, it } from 'vitest'
import { oiScale } from './writerFlowUnits'

describe('the unit Writer Flow shows open interest in', () => {
  it('shows NSE and BSE quantity in lakh, never as contracts', () => {
    // NIFTY 23900 CE held 5,130,775 on 19-Oct-2026: 78,935 lots of 65. The
    // exchange counts a lot as one contract, so "51 lakh contracts" read 65
    // times too high.
    const s = oiScale('quantity')
    expect(s.divisor).toBe(1e5)
    expect(s.word).toBe('lakh qty')
  })

  it('shows MCX lots as lots, not divided into lakh', () => {
    // CRUDEOIL 9000 PE held 6,094 lots: as lakh it drew 0.06.
    expect(oiScale('lots')).toEqual({ divisor: 1, word: 'lots' })
  })

  it('reads an older OpenAlgo that does not say as quantity', () => {
    expect(oiScale(undefined)).toEqual(oiScale('quantity'))
  })
})
