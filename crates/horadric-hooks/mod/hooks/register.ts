import type { AgentInfo, EngineInterface, Register } from 'claude-code'

// Horadric's hooks say a subagent used a tool, never what kind it is or that
// it finished. This mod reports both to the Horadric that started the
// session, on the same listener and with the same tag as the hooks. A
// session Horadric did not start has no tag and the mod does nothing.

const POLL_MS = 2000

type Owner = { url: string; tag: string }

async function owner($: EngineInterface): Promise<Owner | undefined> {
  const tag = await $.env.get('HORADRIC_SESSION')
  const port = await $.env.get('HORADRIC_OWNER_PORT')
  if (!tag || !port || !/^\d+$/.test(port)) return undefined
  return { url: `http://127.0.0.1:${port}/horadric/hook`, tag }
}

// Never awaited by a hook and never thrown: a closed Horadric must cost the
// session nothing.
function post($: EngineInterface, to: Owner, body: Record<string, unknown>): void {
  $.session
    .id()
    .then(session_id =>
      $.http.fetch(to.url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Horadric-Session': to.tag },
        body: JSON.stringify({ session_id, ...body }),
      }),
    )
    .catch(() => undefined)
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

export const register: Register = on => {
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
    }
    return next(e)
  })

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
}
