export type BackchannelItem = {
  dseq: number
  id: string
  from: string
  board: string
  kind: string
  preview: string
  reasons: string[]
}

export type BackchannelView = {
  /** "ok" once a poll worked; "off" when this machine hasn't opted in; "error" when the last poll failed. */
  status: 'unknown' | 'ok' | 'off' | 'error'
  address: string
  unread: number
  latest: BackchannelItem[]
  message: string
}

declare module 'claude-code' {
  interface PluginState {
    'backchannel': {
      view: BackchannelView
      /** Highest delivery already announced with a toast. */
      toasted: number
      /** Highest delivery the person hid the band for. */
      hiddenAt: number
    }
  }
}
