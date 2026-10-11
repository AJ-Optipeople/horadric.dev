import { describe, expect, mock, test } from 'claude-code/testing'

// `as never` on a spawn: the engine fills in the rest of what it takes.
const TAGGED = { HORADRIC_SESSION: 'tile-1', HORADRIC_OWNER_PORT: '43118' }

describe('horadric mod', () => {
  test('posts the roster to the owner when it changes, and only then', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    const posts: { url: string; headers?: Record<string, string>; body: any }[] = []
    on('http.fetch', ($, e) => {
      posts.push({ url: e.url, headers: e.init?.headers, body: JSON.parse(e.init?.body ?? '{}') })
      return { value: { status: 200, ok: true, headers: {}, text: '' } }
    })
    let agents = [{ id: 'a1', type: 'Explore', status: 'running', description: 'look' }]
    on('agent.list', () => ({ value: agents as any }))
    await $.session.start({ cwd: '/p', surface: null, isInteractive: false })

    await clock.advance(2000)
    expect(posts.length).toBe(1)
    expect(posts[0]!.url).toBe('http://127.0.0.1:43118/horadric/hook')
    expect(posts[0]!.headers?.['X-Horadric-Session']).toBe('tile-1')
    expect(posts[0]!.body.hook_event_name).toBe('HoradricAgents')
    expect(posts[0]!.body.session_id).toBe('conv-1')
    expect(posts[0]!.body.agents[0]).toEqual(
      expect.objectContaining({ agent_id: 'a1', agent_type: 'Explore', status: 'running' }),
    )

    await clock.advance(2000)
    expect(posts.length).toBe(1)

    agents = [{ ...agents[0]!, status: 'completed' }]
    await clock.advance(2000)
    expect(posts.length).toBe(2)
    expect(posts[1]!.body.agents[0].status).toBe('completed')
  })

  test('reports a spawn with its agent type', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    const posts: any[] = []
    on('http.fetch', ($, e) => {
      posts.push(JSON.parse(e.init?.body ?? '{}'))
      return { value: { status: 200, ok: true, headers: {}, text: '' } }
    })
    on('agent.spawn', () => ({ model: 'haiku', agentId: 'a2' }))
    const started = await $.agent.spawn({ prompt: 'p', description: 'scan', subagentType: 'Explore' } as never)
    await clock.settle()
    expect(started.agentId).toBe('a2')
    expect(posts).toEqual([
      expect.objectContaining({
        hook_event_name: 'HoradricSpawn',
        agent_id: 'a2',
        agent_type: 'Explore',
        description: 'scan',
      }),
    ])
  })

  test('stays silent in a session Horadric did not start', async ($, on) => {
    mock.env(on, {})
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    let posted = 0
    on('http.fetch', () => (posted++, { value: { status: 200, ok: true, headers: {}, text: '' } }))
    on('agent.list', () => ({ value: [{ id: 'a1', type: 'Explore', status: 'running', description: '' }] as any }))
    on('agent.spawn', () => ({ model: 'haiku', agentId: 'a3' }))
    await $.session.start({ cwd: '/p', surface: null, isInteractive: false })
    await $.agent.spawn({ prompt: 'p', description: 'd', subagentType: 'Explore' } as never)
    await clock.advance(10000)
    expect(posted).toBe(0)
  })

  test('a closed Horadric costs the spawn nothing', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    on('http.fetch', () => {
      throw new Error('connection refused')
    })
    on('agent.spawn', () => ({ model: 'haiku', agentId: 'a4' }))
    const started = await $.agent.spawn({ prompt: 'p', description: 'd', subagentType: 'Explore' } as never)
    await clock.settle()
    expect(started.agentId).toBe('a4')
  })
})
