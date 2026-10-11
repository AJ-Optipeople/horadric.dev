/** A file this session changed that other sessions changed too, uncommitted. */
export type Shared = { file: string; path: string; others: string[] }

/** What Horadric knows about this session, as `/horadric/know` answers. */
export type Known = { quest?: string; overlaps?: Shared[] }

declare module 'claude-code' {
  interface PluginState {
    horadric: { known: Known | null }
  }
}
