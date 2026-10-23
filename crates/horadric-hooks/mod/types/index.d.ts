/** A file this session changed that other sessions changed too, uncommitted. */
export type Shared = { file: string; path: string; others: string[] }

/**
 * What Horadric knows about this session, as `/horadric/know` answers.
 * `stop` names the subagents the human asked Horadric to stop, each once.
 */
export type Known = { quest?: string; overlaps?: Shared[]; stop?: string[] }

declare module 'claude-code' {
  interface PluginState {
    horadric: { known: Known | null }
  }
}
