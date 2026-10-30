/**
 * How a backend rank-movement event is shown, in one place.
 *
 * Two scripts already drifted from the engine by restating its rule, and this
 * table is the same hazard on the frontend: the TradeFinder panel and the boost
 * strikes panel must agree on what counts as a badge and what it looks like, or
 * a stock appears in one and not the other.
 *
 * Badges are words, not symbols: /trading draws no glyphs of its own, and an
 * arrow or a diamond told a screen reader nothing.
 */

export interface BadgeStyle {
  text: string
  className: string
}

/** Salient events worth a chip. CLIMBING/FALLING/NEW/NORMAL/ABSENT are absent
 * on purpose: the rank-delta arrow already covers small moves, and a symbol
 * that has left the list has no current event at all. */
export const MOVEMENT_BADGE: Record<string, BadgeStyle> = {
  EXTREME_JUMP: { text: 'JUMP', className: 'text-amber-500' },
  LARGE_JUMP: { text: 'JUMP', className: 'text-amber-400' },
  FAST_CLIMB: { text: 'FAST', className: 'text-emerald-500' },
  TOP5_ENTRY: { text: 'IN T5', className: 'text-emerald-500' },
  TOP10_ENTRY: { text: 'IN T10', className: 'text-emerald-500' },
  TOP20_ENTRY: { text: 'IN T20', className: 'text-emerald-400' },
  TOP5_RE_ENTRY: { text: 'BACK T5', className: 'text-emerald-500' },
  TOP10_RE_ENTRY: { text: 'BACK T10', className: 'text-emerald-500' },
  TOP20_RE_ENTRY: { text: 'BACK T20', className: 'text-emerald-400' },
  SUSTAINED_TOP5: { text: 'HELD T5', className: 'text-sky-400' },
  SUSTAINED_TOP10: { text: 'HELD T10', className: 'text-sky-400' },
  SUSTAINED_TOP20: { text: 'HELD T20', className: 'text-sky-500' },
  TOP5_EXIT: { text: 'OUT T5', className: 'text-red-500' },
  TOP10_EXIT: { text: 'OUT T10', className: 'text-red-500' },
  TOP20_EXIT: { text: 'OUT T20', className: 'text-red-500' },
  FAST_DROP: { text: 'DROP', className: 'text-red-500' },
  CLEAN_RUN_UP: { text: 'RUN UP', className: 'text-emerald-500' },
  // The only day measured so far, 17-Sep-2026, was a strongly rising one -- 64
  // of the list up against 15 down -- so its verdict on down runs says more
  // about that day than about the signal. The market does not only go up, so
  // the badge is shown; it is marked untested rather than judged.
  CLEAN_RUN_DOWN: { text: 'RUN DOWN', className: 'text-red-500' },
}

/** Events whose badge is not yet backed by evidence, so the tooltip says so
 * rather than letting the chip imply the same standing as the rest. */
export const UNPROVEN_EVENTS = new Set(['CLEAN_RUN_DOWN'])

export function badgeFor(event: string | undefined): BadgeStyle | undefined {
  return event ? MOVEMENT_BADGE[event] : undefined
}

/** What a row last badged as, and when. Keeping the badge itself -- not just a
 * timestamp -- is what lets a row held by the sticky window still show the
 * reason it is on screen. A row with nothing rendered beside it reads as a
 * filter that leaked, which is how the first version of this looked. */
export interface BadgeSighting extends BadgeStyle {
  at: number
}

/** Record that each `[key, badge]` (`view:symbol`) was badged at `now`, keeping
 * earlier sightings still inside `windowMs` and dropping the rest.
 *
 * The merge is the point. The backend classifies only transitions on the latest
 * observed minute, so LARGE_JUMP and TOP10_ENTRY are gone by the next poll --
 * replacing the map instead of merging would forget the mover a minute after it
 * moved, which is exactly the miss the badged-only filter exists to prevent.
 * Pruning here rather than at the point of use keeps the map bounded by the
 * window instead of by everything that badged all session. */
export function stampBadgeSightings(
  prev: Map<string, BadgeSighting>,
  seen: Iterable<[string, BadgeStyle]>,
  now: number,
  windowMs: number
): Map<string, BadgeSighting> {
  const cutoff = now - windowMs
  const next = new Map([...prev].filter(([, s]) => s.at >= cutoff))
  for (const [key, badge] of seen) next.set(key, { ...badge, at: now })
  return next
}

/** The sighting for `key` if it is still inside the window, else undefined. */
export function badgedWithin(
  seenAt: Map<string, BadgeSighting>,
  key: string,
  now: number,
  windowMs: number
): BadgeSighting | undefined {
  const s = seenAt.get(key)
  return s && s.at >= now - windowMs ? s : undefined
}

/** "just now" / "4m" -- how long ago a sticky row last badged. */
export function badgeAge(at: number, now: number): string {
  const mins = Math.floor((now - at) / 60_000)
  return mins < 1 ? 'now' : `${mins}m`
}

/** A minute-of-day as a clock time: 574 reads as 09:34.
 *
 * The recorder samples twice a minute, so the value can carry a half -- 604.5
 * is 10:04:30. Shown to the second in that case, because a trader reading "the
 * move turned at 10:04" wants to know which half of the minute when the badge
 * itself is promised inside forty seconds. */
export function minuteOfDay(min: number): string {
  const whole = Math.floor(min)
  const seconds = Math.round((min - whole) * 60)
  const hh = String(Math.floor(whole / 60)).padStart(2, '0')
  const mm = String(whole % 60).padStart(2, '0')
  return seconds ? `${hh}:${mm}:${String(seconds).padStart(2, '0')}` : `${hh}:${mm}`
}
