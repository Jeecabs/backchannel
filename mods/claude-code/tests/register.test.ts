import type { On } from 'claude-code'
import { describe, expect, mock, test } from 'claude-code/testing'

const HOME = '/home/someone'
const CONFIG = `${HOME}/.backchannel/config.json`
const RELAY = 'https://relay.example'
const RUN = { origin: { kind: 'composer' as const }, presentation: { isFullscreen: true, columns: 160 } }
const SESSION = { surface: 'terminal' as const, isInteractive: true, cwd: '/work' }

type Relay = (path: string, token: string, body: string) => { status: number; body: unknown }

/**
 * A machine with a config file and a relay. `cli` is what running the
 * `backchannel` command does: `null` for a machine without one.
 */
function world(on: On, config: Record<string, unknown> | null, relay: Relay, cli: null | ((argv: readonly string[]) => { exitCode: number; stdout: string; stderr: string }) = null) {
  const files = new Map<string, string>()
  if (config) files.set(CONFIG, JSON.stringify(config))
  const seen = { toasts: [] as string[], status: [] as (string | undefined)[], requests: [] as { path: string; token: string }[], runs: [] as (readonly string[])[] }

  on('session.start', ($, e) => ({ cwd: e.cwd }))
  on('command.register', ($, e) => ({ value: { command: e.name } }))
  on('ui.toast', ($, e) => {
    seen.toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.status', ($, e) => {
    seen.status.push(e.text)
    return { value: undefined }
  })
  on('fs.read', ($, e) => {
    const text = files.get(e.path)
    if (text === undefined) throw new Error(`no such file: ${e.path}`)
    return { value: text }
  })
  on('fs.write', ($, e) => {
    files.set(e.path, e.text)
    return { value: undefined }
  })
  on('http.fetch', ($, e) => {
    const path = e.url.slice(RELAY.length)
    const token = String((e.init?.headers as Record<string, string> | undefined)?.authorization ?? '').replace('Bearer ', '')
    seen.requests.push({ path, token })
    const answer = relay(path, token, String(e.init?.body ?? ''))
    return { value: { status: answer.status, ok: answer.status < 300, headers: {}, text: JSON.stringify(answer.body) } }
  })
  on('process.run', ($, e) => {
    seen.runs.push(e.argv)
    if (!cli) throw new Error('spawn backchannel ENOENT')
    return { value: { ...cli(e.argv), isStdoutTruncated: false, isStderrTruncated: false } }
  })
  mock.clock(on)
  mock.env(on, { HOME })

  return { files, seen, stored: () => JSON.parse(files.get(CONFIG) ?? 'null') as Record<string, unknown> | null }
}

const REMOTE = { version: 1, mode: 'remote', name: 'bob', relay: RELAY, token: 'bch_old', opted_in: true }
const REFUSED = { status: 401, body: { error: { code: 'unauthorized', message: "That token doesn't work: it was rotated or revoked" } } }
const UNREAD = { status: 200, body: { address: 'bob/human', unread: 0, latest: [] } }

describe('register', () => {
  test('a token revoked elsewhere is said once, and /backchannel says what to run', async ($, on) => {
    const w = world(on, REMOTE, () => REFUSED)

    await $.session.start(SESSION)

    expect(w.seen.status.at(-1)).toBe('backchannel: sign in again')
    expect(w.seen.toasts).toEqual(["Backchannel: this machine's token no longer works. Run /backchannel for what to do."])

    const { text } = await $.command.run({ command: 'backchannel', args: '', ...RUN })

    expect(text).toContain("this machine's token doesn't work any more")
    expect(text).toContain('/backchannel signin <code>')
    expect(text).not.toContain('Backchannel said no')
    // Still signed out after another look: nothing new to announce.
    expect(w.seen.toasts.length).toBe(1)
  })

  test('/backchannel rotate, with no backchannel command, swaps the stored token once the new one works', async ($, on) => {
    const good = new Set(['bch_old'])
    const w = world(on, REMOTE, (path, token) => {
      if (!good.has(token)) return REFUSED
      if (path === '/v1/call/rotate_token') {
        good.add('bch_new')
        return { status: 200, body: { text: 'Made a new token', label: 'human', token: 'bch_new' } }
      }
      return UNREAD
    })

    await $.session.start(SESSION)
    const { text } = await $.command.run({ command: 'backchannel', args: 'rotate', ...RUN })

    expect(text).toContain('Rotated.')
    expect(text).not.toContain('bch_')
    expect(w.stored()?.token).toBe('bch_new')
    expect(w.stored()?.name).toBe('bob')
    // The new token was used after it was saved, which is what retires the old one.
    expect(w.seen.requests.at(-1)?.token).toBe('bch_new')
    expect(w.seen.runs).toEqual([['backchannel', 'rotate']])
  })

  test('/backchannel rotate keeps the old token when the new one does not work', async ($, on) => {
    const w = world(on, REMOTE, (path, token) => {
      if (path === '/v1/call/rotate_token') return { status: 200, body: { text: 'Made a new token', label: 'human', token: 'bch_lost' } }
      return token === 'bch_old' ? UNREAD : REFUSED
    })

    await $.session.start(SESSION)
    const { text } = await $.command.run({ command: 'backchannel', args: 'rotate', ...RUN })

    expect(text).toContain('the old one is back in place and still good')
    expect(w.stored()?.token).toBe('bch_old')
  })

  test('/backchannel rotate hands over to the backchannel command when there is one', async ($, on) => {
    const w = world(on, REMOTE, () => UNREAD, argv => ({ exitCode: argv[1] === 'rotate' ? 0 : 2, stdout: 'Rotated. This machine now uses a new token (7KQ2M9XD).\n', stderr: '' }))

    await $.session.start(SESSION)
    const { text } = await $.command.run({ command: 'backchannel', args: 'rotate', ...RUN })

    expect(text).toBe('Rotated. This machine now uses a new token (7KQ2M9XD).')
    // The command changed the file itself; the mod asked the relay for nothing but what is unread.
    expect(w.seen.requests.every(r => r.path === '/v1/unread')).toBe(true)
    expect(w.stored()?.token).toBe('bch_old')
  })

  test('/backchannel signin trades a code or a link for a token and stores it', async ($, on) => {
    const w = world(on, { ...REMOTE, display_name: 'Bob', default_subscriptions: ['smoko', 'ops/**'] }, (path, token, body) => {
      if (path === '/v1/signin') {
        return JSON.parse(body).code === 'SIGN-7KQ2-M9XD-4TRW-B1ZP'
          ? { status: 200, body: { status: 'joined', org: 'acme', name: 'bob', token: 'bch_again' } }
          : { status: 403, body: { error: { code: 'forbidden', message: "that sign-in code isn't valid" } } }
      }
      return token === 'bch_again' ? UNREAD : REFUSED
    })

    await $.session.start(SESSION)
    expect(w.seen.status.at(-1)).toBe('backchannel: sign in again')

    const wrong = await $.command.run({ command: 'backchannel', args: 'signin SIGN-0000-0000-0000-0000', ...RUN })
    expect(wrong.text).toContain("that sign-in code isn't valid")
    expect(w.stored()?.token).toBe('bch_old')

    const { text } = await $.command.run({ command: 'backchannel', args: `signin ${RELAY}/join?signin=SIGN-7KQ2-M9XD-4TRW-B1ZP`, ...RUN })
    expect(text).toBe(`Signed in again as bob in the org acme on ${RELAY}. The code is used up.`)
    expect(w.stored()).toEqual({ version: 1, mode: 'remote', name: 'bob', display_name: 'Bob', bio: '', relay: RELAY, token: 'bch_again', opted_in: true, default_subscriptions: ['smoko', 'ops/**'] })
    // Signed in again: the status line is clear.
    expect(w.seen.status.at(-1)).toBe(undefined)
  })

  test('/backchannel login with a relay sends the token nowhere else', async ($, on) => {
    const w = world(on, null, () => UNREAD)

    await $.session.start(SESSION)
    const { text } = await $.command.run({ command: 'backchannel', args: `login bch_mine ${RELAY}/`, ...RUN })

    expect(text).toContain(`you're logged in to ${RELAY}`)
    expect(w.stored()?.relay).toBe(RELAY)
    expect(w.seen.requests.length).toBeGreaterThan(0)
    expect(w.seen.requests.every(r => r.token === 'bch_mine')).toBe(true)
  })
})
