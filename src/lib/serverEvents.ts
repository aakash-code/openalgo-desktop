/**
 * Server events for code outside React: the custom chart indicators.
 *
 * The app's Socket.IO connection lives in `useSocket`, behind a React context
 * an indicator cannot reach: indicators are plain modules loaded at runtime.
 * `useSocket` hands its connection here, and the indicator loader passes
 * `onServerEvent` to every indicator beside `subscribeQuotes`.
 *
 * Only the events in SERVER_EVENTS can be listened to. They are broadcasts
 * about shared data (a new Writer Flow minute, a new market read), never account or session
 * events, so a third-party indicator learns nothing through them that it could
 * not already fetch.
 *
 * A listener registered before the connection exists, or across a reconnect
 * (`useSocket` makes a new connection each time), is carried over: the socket
 * changes underneath it and the listener does not notice.
 */
import type { Socket } from 'socket.io-client'

export const SERVER_EVENTS = ['writer_flow_update', 'market_read_update'] as const
export type ServerEventName = (typeof SERVER_EVENTS)[number]

type Handler = (data: unknown) => void

const handlers = new Map<string, Set<Handler>>()
// One relay per event name, attached to whichever socket is current.
const relays = new Map<string, Handler>()
let socket: Socket | null = null

function relayFor(name: string): Handler {
  let relay = relays.get(name)
  if (!relay) {
    relay = (data: unknown) => {
      for (const handler of [...(handlers.get(name) ?? [])]) {
        try {
          handler(data)
        } catch {
          // One listener's error does not keep the event from the others.
        }
      }
    }
    relays.set(name, relay)
    socket?.on(name, relay)
  }
  return relay
}

/** Called by `useSocket` when its connection is made and, with null, when it closes. */
export function setServerEventSocket(next: Socket | null): void {
  if (socket === next) return
  if (socket) for (const [name, relay] of relays) socket.off(name, relay)
  socket = next
  if (socket) for (const [name, relay] of relays) socket.on(name, relay)
}

/** The connection a listener is attached to now, if any. For `useSocket`'s teardown. */
export function serverEventSocket(): Socket | null {
  return socket
}

/**
 * Listen for one server event. Returns the function that stops listening;
 * call it when the listener's chart or study goes away.
 */
export function onServerEvent(name: string, handler: Handler): () => void {
  if (!(SERVER_EVENTS as readonly string[]).includes(name)) {
    throw new Error(`onServerEvent: "${name}" is not an event charts can listen to`)
  }
  if (typeof handler !== 'function') throw new Error('onServerEvent needs a function')
  let set = handlers.get(name)
  if (!set) {
    set = new Set()
    handlers.set(name, set)
  }
  set.add(handler)
  relayFor(name)
  return () => {
    const current = handlers.get(name)
    if (!current?.delete(handler) || current.size > 0) return
    handlers.delete(name)
    const relay = relays.get(name)
    if (relay) socket?.off(name, relay)
    relays.delete(name)
  }
}
