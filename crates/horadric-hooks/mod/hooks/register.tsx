import { atom, read, update } from 'claude-code'
import type { AgentInfo, EngineInterface, Register } from 'claude-code'

import type { Known, Shared } from '../types'

// Horadric's hooks say a subagent used a tool, never what kind it is or that
// it finished. This mod reports both to the Horadric that started the
// session, on the same listener and with the same tag as the hooks. It also
// asks that Horadric what it knows about the session (the quest it holds,
// files another session is changing too) and shows it above the prompt. A
// session Horadric did not start has no tag and the mod does nothing.

const POLL_MS = 2000
const KNOW_MS = 3000

const known = atom({ plugin: 'horadric', key: 'known' } as const, null)

type Owner = { port: string; tag: string }

async function owner($: EngineInterface): Promise<Owner | undefined> {
  const tag = await $.env.get('HORADRIC_SESSION')
  const port = await $.env.get('HORADRIC_OWNER_PORT')
  if (!tag || !port || !/^\d+$/.test(port)) return undefined
  return { port, tag }
}

const url = (to: Owner, path: string) => `http://127.0.0.1:${to.port}/horadric/${path}`

// Never awaited by a hook and never thrown: a closed Horadric must cost the
// session nothing.
function post($: EngineInterface, to: Owner, body: Record<string, unknown>): void {
  $.session
    .id()
    .then(session_id =>
      $.http.fetch(url(to, 'hook'), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Horadric-Session': to.tag },
        body: JSON.stringify({ session_id, ...body }),
      }),
    )
    .catch(() => undefined)
}

// A closed or busy Horadric knows nothing, so the band goes away rather than
// show what was true before.
async function ask($: EngineInterface, to: Owner): Promise<Known | null> {
  try {
    const r = await $.http.fetch(url(to, 'know'), {
      method: 'POST',
      headers: { 'X-Horadric-Session': to.tag },
      body: '{}',
    })
    if (!r.ok) return null
    const k = JSON.parse(r.text) as Known
    return k.quest || k.overlaps?.length ? k : null
  } catch {
    return null
  }
}

export function roster(agents: AgentInfo[]) {
  return agents.map(a => ({
    agent_id: a.id,
    agent_type: a.type,
    status: a.status,
    description: a.description,
    parent_id: a.parentId,
    teammate_id: a.teammateId,
  }))
}

const others = (s: Shared) =>
  s.others.length === 1 ? s.others[0]! : `${s.others.slice(0, -1).join(', ')} and ${s.others.at(-1)}`

/** What Claude is told beside a prompt about the files it shares. */
export function sharedContext(shared: Shared[]): string | undefined {
  if (!shared.length) return undefined
  const files = shared.map(s => `${s.path} (also changed by ${others(s)})`).join(', ')
  return (
    `Horadric: other sessions in this working tree have changed files you changed and not ` +
    `committed them: ${files}. Look at \`git diff\` on those files before you change them ` +
    `further, keep what is theirs, and stage only your own changes when you commit.`
  )
}

export const register: Register = on => {
  // What Claude was last told, so a prompt carries the overlap once and
  // again only when it changes.
  let told = ''

  on('session.start', async ($, e, next) => {
    const to = await owner($)
    if (to) {
      let last = '[]'
      $.clock.every(POLL_MS, async () => {
        const agents = JSON.stringify(roster(await $.agent.list()))
        if (agents === last) return
        last = agents
        post($, to, { hook_event_name: 'HoradricAgents', agents: JSON.parse(agents) })
      })
      let seen = 'null'
      const look = async () => {
        const k = await ask($, to)
        if (JSON.stringify(k) === seen) return
        seen = JSON.stringify(k)
        await update($, known, () => k)
      }
      void look()
      $.clock.every(KNOW_MS, look)
    }
    return next(e)
  })

  on('prompt.submit', async ($, e, next) => {
    const shared = (await read($, known))?.overlaps ?? []
    const now = JSON.stringify(shared)
    const text = now === told ? undefined : sharedContext(shared)
    told = now
    return text ? next({ ...e, context: [...(e.context ?? []), text] }) : next(e)
  }).catch(($, e, next) => next(e))

  on('agent.spawn', async ($, e, next) => {
    const started = await next(e)
    const to = await owner($)
    if (to && started.agentId) {
      post($, to, {
        hook_event_name: 'HoradricSpawn',
        agent_id: started.agentId,
        agent_type: e.subagentType,
        description: e.description,
        model: started.model,
        parent_id: e.parentAgentId,
        workflow_run: e.workflow?.runId,
      })
    }
    return started
  }).catch(($, e, next) => next(e))

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const k = await read($, known)
    if (e.props.hasSurvey || !k) return next(e)
    const { Box, Text } = $.ui.resolve(e)
    return (
      <Box flexDirection="column">
        {k.quest ? (
          <Box key="quest" flexDirection="row">
            <Text color="subtle">Quest </Text>
            <Text color="suggestion" wrap="truncate-end">
              {k.quest}
            </Text>
          </Box>
        ) : null}
        {(k.overlaps ?? []).map(s => (
          <Box key={`shared:${s.path}`} flexDirection="row">
            <Text color="warning">{s.file}</Text>
            <Text color="subtle" wrap="truncate-end">
              {` also changed by ${others(s)}, not committed`}
            </Text>
          </Box>
        ))}
      </Box>
    )
  })
}
