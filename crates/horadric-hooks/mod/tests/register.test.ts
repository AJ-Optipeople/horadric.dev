import { describe, expect, mock, test } from 'claude-code/testing'

import { band, stamp } from '../hooks/register'

// `as never` on a spawn: the engine fills in the rest of what it takes.
const TAGGED = { HORADRIC_SESSION: 'tile-1', HORADRIC_OWNER_PORT: '43118' }
// What the engine draws above the prompt when no mod draws there.
const ENGINE_BAND = { type: 'Box' as const, props: { key: 'engine' } }
const KNOWS_NOTHING = { value: { status: 200, ok: true, headers: {}, text: '{}' } }

describe('horadric mod', () => {
  test('posts the roster to the owner when it changes, and only then', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    const posts: { url: string; headers?: Record<string, string>; body: any }[] = []
    on('http.fetch', ($, e) => {
      if (e.url.endsWith('/know')) return KNOWS_NOTHING
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
      if (e.url.endsWith('/know')) return KNOWS_NOTHING
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

  test('reports when each agent started, from its spawn or first sight', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    const posts: any[] = []
    on('http.fetch', ($, e) => {
      if (e.url.endsWith('/know')) return KNOWS_NOTHING
      posts.push(JSON.parse(e.init?.body ?? '{}'))
      return { value: { status: 200, ok: true, headers: {}, text: '' } }
    })
    let agents: any[] = []
    on('agent.list', () => ({ value: agents }))
    on('agent.spawn', () => ({ model: 'haiku', agentId: 's1' }))
    await $.session.start({ cwd: '/p', surface: null, isInteractive: false })
    const t0 = clock.now()
    await $.agent.spawn({ prompt: 'p', description: 'scan', subagentType: 'Explore' } as never)
    await clock.advance(4000)
    agents = [
      { id: 's1', type: 'Explore', status: 'running', description: 'scan' },
      { id: 'f1', type: 'Plan', status: 'running', description: 'plan' },
    ]
    await clock.advance(2000)
    const roster = posts.filter(p => p.hook_event_name === 'HoradricAgents').at(-1)
    expect(roster.agents[0].started_ms).toBe(t0)
    expect(roster.agents[1].started_ms).toBe(t0 + 6000)
  })

  test('stops each subagent Horadric names, by TaskStop', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    let answer: object = { stop: ['a1', 'a2'] }
    on('http.fetch', () => {
      const text = JSON.stringify(answer)
      answer = {}
      return { value: { status: 200, ok: true, headers: {}, text } }
    })
    on('agent.list', () => ({ value: [] }))
    const stopped: unknown[] = []
    on('tool.call', { tool: 'TaskStop' }, ($, e) => {
      stopped.push((e as any).task_id)
      return { result: 'stopped' }
    })
    on('ui.render', () => ENGINE_BAND)
    await $.session.start({ cwd: '/p', surface: null, isInteractive: true })
    await clock.advance(3000)
    expect(stopped).toEqual(['a1', 'a2'])
    await clock.advance(3000)
    expect(stopped).toEqual(['a1', 'a2'])
    // A stop alone is nothing to show above the prompt.
    const band = await $.ui.mount({
      plugin: 'horadric',
      surface: 'terminal',
      component: 'AbovePrompt',
      props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 80 } as never,
    })
    expect(await band.drawn()).toEqual(expect.objectContaining({ type: 'Box', props: { key: 'engine' } }))
  })

  test('stamp keeps a spawn not yet listed and forgets an old one gone', async () => {
    const since = new Map([['spawned', 30_000], ['gone', 0]])
    stamp(since, [{ id: 'new', type: 'Plan', status: 'running', description: '' }], 61_000)
    expect([...since]).toEqual([['spawned', 30_000], ['new', 61_000]])
  })

  test('the band shows the quest and overlaps, never a stop alone', async () => {
    expect(band(null)).toBeNull()
    expect(band({ stop: ['a1'] })).toBeNull()
    expect(band({ quest: 'q', stop: ['a1'] })).toEqual({ quest: 'q', overlaps: undefined })
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

  test('shows the quest and a file another session changed above the prompt', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    let answer: object = {}
    const asked: (Record<string, string> | undefined)[] = []
    on('http.fetch', ($, e) => {
      if (e.url === 'http://127.0.0.1:43118/horadric/know') asked.push(e.init?.headers)
      return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(answer) } }
    })
    on('agent.list', () => ({ value: [] }))
    on('ui.render', () => ENGINE_BAND)
    await $.session.start({ cwd: '/p', surface: null, isInteractive: true })
    const band = await $.ui.mount({
      plugin: 'horadric',
      surface: 'terminal',
      component: 'AbovePrompt',
      props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 80 } as never,
    })
    await clock.advance(3000)
    expect(asked[0]?.['X-Horadric-Session']).toBe('tile-1')
    expect(await band.find({ text: 'Quest' })).toBeUndefined()

    answer = {
      quest: 'Serve the API',
      overlaps: [{ file: 'overlap.rs', path: 'C:/app/overlap.rs', others: ['fix-a', 'fix-b'] }],
    }
    await clock.advance(3000)
    expect(await band.find({ text: 'Serve the API' })).toBeDefined()
    expect(await band.find({ text: 'overlap.rs' })).toBeDefined()
    expect(await band.find({ text: ' also changed by fix-a and fix-b, not committed' })).toBeDefined()

    answer = {}
    await clock.advance(3000)
    expect(await band.find({ text: 'Serve the API' })).toBeUndefined()
  })

  test('tells Claude of a shared file with the next prompt, once', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    const answer = { overlaps: [{ file: 'overlap.rs', path: 'C:/app/overlap.rs', others: ['fix-a'] }] }
    on('http.fetch', () => ({ value: { status: 200, ok: true, headers: {}, text: JSON.stringify(answer) } }))
    on('agent.list', () => ({ value: [] }))
    on('prompt.submit', ($, e) => ({ text: e.text, context: e.context }))
    await $.session.start({ cwd: '/p', surface: null, isInteractive: true })
    await clock.advance(3000)

    const first = await $.prompt.submit({ text: 'go on' } as never)
    expect(first.context?.join(' ')).toContain('C:/app/overlap.rs (also changed by fix-a)')
    const second = await $.prompt.submit({ text: 'and then' } as never)
    expect(second.context ?? []).toEqual([])
  })

  test('a closed Horadric shows nothing above the prompt', async ($, on) => {
    mock.env(on, TAGGED)
    on('session.id', () => ({ value: 'conv-1' }))
    on('session.start', ($, e) => ({ cwd: e.cwd }))
    const clock = mock.clock(on)
    on('http.fetch', () => {
      throw new Error('connection refused')
    })
    on('agent.list', () => ({ value: [] }))
    on('ui.render', () => ENGINE_BAND)
    await $.session.start({ cwd: '/p', surface: null, isInteractive: true })
    await clock.advance(3000)
    const band = await $.ui.mount({
      plugin: 'horadric',
      surface: 'terminal',
      component: 'AbovePrompt',
      props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 80 } as never,
    })
    expect(await band.drawn()).toEqual(expect.objectContaining({ type: 'Box', props: { key: 'engine' } }))
  })
})
