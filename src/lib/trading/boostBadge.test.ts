import { describe, expect, it } from 'vitest'
import { badgeAge, badgedWithin, badgeFor, stampBadgeSightings } from './boostBadge'

const WINDOW = 15 * 60_000
const T0 = 1_700_000_000_000
const RUN_UP = { text: 'RUN UP', className: 'text-emerald-500' }
const JUMP = { text: 'JUMP', className: 'text-amber-400' }

describe('badge stickiness', () => {
  /* THE DEFECT THIS EXISTS FOR: the backend classifies only transitions on the
     latest observed minute, so a rank jump is absent from the very next poll.
     A badged-only filter reading the live badge would show the mover for one
     poll and drop it -- reproducing the miss (SOLARINDS, 22-Sep-2026, rank
     79 -> 1 in a 195-row list) that the filter was built to prevent. */
  it('keeps a symbol after its one-minute badge stops being reported', () => {
    const afterJump = stampBadgeSightings(
      new Map(),
      [['intraday_boost:SOLARINDS', JUMP]],
      T0,
      WINDOW
    )
    // Next poll: the event is gone, and some other symbol badges instead.
    const nextPoll = stampBadgeSightings(
      afterJump,
      [['intraday_boost:OIL', RUN_UP]],
      T0 + 60_000,
      WINDOW
    )

    expect(badgedWithin(nextPoll, 'intraday_boost:SOLARINDS', T0 + 60_000, WINDOW)).toBeDefined()
    expect(badgedWithin(nextPoll, 'intraday_boost:OIL', T0 + 60_000, WINDOW)).toBeDefined()
  })

  /* A sticky row must be able to say WHY it is on screen. Without the badge
     text carried alongside the timestamp the row renders bare, which reads as
     the filter having leaked -- the exact confusion the first cut produced. */
  it('remembers which badge it was, so a sticky row can explain itself', () => {
    const seen = stampBadgeSightings(new Map(), [['intraday_boost:SOLARINDS', JUMP]], T0, WINDOW)
    const hit = badgedWithin(seen, 'intraday_boost:SOLARINDS', T0 + 5 * 60_000, WINDOW)
    expect(hit?.text).toBe('JUMP')
    expect(badgeAge(hit!.at, T0 + 5 * 60_000)).toBe('5m')
  })

  it('drops a symbol once the window has passed', () => {
    const seen = stampBadgeSightings(new Map(), [['intraday_boost:SOLARINDS', RUN_UP]], T0, WINDOW)
    expect(badgedWithin(seen, 'intraday_boost:SOLARINDS', T0 + WINDOW - 1, WINDOW)).toBeDefined()
    expect(badgedWithin(seen, 'intraday_boost:SOLARINDS', T0 + WINDOW + 1, WINDOW)).toBeUndefined()
  })

  it('prunes expired entries rather than growing all session', () => {
    const old = stampBadgeSightings(new Map(), [['intraday_boost:STALE', RUN_UP]], T0, WINDOW)
    const fresh = stampBadgeSightings(
      old,
      [['intraday_boost:FRESH', RUN_UP]],
      T0 + WINDOW + 1,
      WINDOW
    )
    expect([...fresh.keys()]).toEqual(['intraday_boost:FRESH'])
  })

  it('never matches a symbol that has not badged', () => {
    expect(badgedWithin(new Map(), 'intraday_boost:QUIET', T0, WINDOW)).toBeUndefined()
  })

  it('keys by list, so one list cannot badge another', () => {
    const seen = stampBadgeSightings(new Map(), [['intraday_boost:SOLARINDS', RUN_UP]], T0, WINDOW)
    expect(badgedWithin(seen, 'breakout_beacon:SOLARINDS', T0, WINDOW)).toBeUndefined()
  })

  it('reads a fresh sighting as now, not 0m', () => {
    expect(badgeAge(T0, T0 + 10_000)).toBe('now')
  })
})

describe('badgeFor', () => {
  it('badges real movement events, including the clean runs', () => {
    for (const event of ['LARGE_JUMP', 'FAST_CLIMB', 'TOP10_ENTRY', 'CLEAN_RUN_UP']) {
      expect(badgeFor(event), event).toBeDefined()
    }
  })

  it('does not badge the non-events, which are most of the list', () => {
    for (const event of ['CLIMBING', 'FALLING', 'NORMAL', 'NEW', 'ABSENT', undefined]) {
      expect(badgeFor(event), String(event)).toBeUndefined()
    }
  })
})

describe('badge text', () => {
  it('is words, never a symbol standing in for an icon', async () => {
    const { MOVEMENT_BADGE } = await import('./boostBadge')
    for (const [event, badge] of Object.entries(MOVEMENT_BADGE)) {
      expect(badge.text, event).toMatch(/^[A-Z0-9 ]+$/)
    }
  })
})
