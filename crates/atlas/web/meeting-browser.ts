#!/usr/bin/env bun
/**
 * A meeting in three real browsers, against a real simulator (protocol 41,
 * `docs/notes/calls.md` "A call is a meeting").
 *
 *   bun run build:chat-core --dev && bun run build
 *   bun meeting-browser.ts --sim "$CARGO_TARGET_DIR/debug/simulator" --shots /tmp/meeting
 *
 * Ann starts a video call in a topic and so hosts it; Bob and Carol answer.
 * Bob raises a hand and Ann is told; Bob reacts and Ann sees it over his
 * tile, sealed end to end; Ann mutes Carol, who is told and cannot unmute;
 * Ann shares a screen and Bob sees it as the stage; Ann ends the call for
 * everyone. The screen is a canvas standing in for `getDisplayMedia`, which a
 * headless browser has no screen for.
 */
import {mkdirSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'

import {Browser, bootSim, bootWeb, procs, shots, waitFor} from './browser-harness'

const browsers: Browser[] = []
const started = Date.now()
const step = (s: string) => console.log(`${((Date.now() - started) / 1000).toFixed(1).padStart(5)} s  ${s}`)

/** A slide drawn on a canvas, offered as the screen the browser would have asked for. */
const STAND_IN_SCREEN = `(() => {
    const c = Object.assign(document.createElement('canvas'), {height: 720, width: 1280})
    const g = c.getContext('2d')
    let n = 0
    const draw = () => {
        g.fillStyle = '#fbfaf7'; g.fillRect(0, 0, 1280, 720)
        g.fillStyle = '#c06630'; g.font = '600 22px sans-serif'; g.fillText('STREET PARTY 2026', 70, 90)
        g.fillStyle = '#11181b'; g.font = '600 52px sans-serif'; g.fillText('The budget held,', 70, 160); g.fillText('except for the gazebos', 70, 222)
        const bars = [['Stage', 0.74, '#2c917c'], ['Gazebos', 0.51, '#c06630'], ['Food', 0.88, '#2c917c'], ['Sound', 0.38, '#2c917c'], ['Waste', 0.24, '#2c917c']]
        bars.forEach(([label, v, col], i) => {
            const y = 300 + i * 74
            g.fillStyle = '#3b4549'; g.font = '24px sans-serif'; g.fillText(label, 70, y + 26)
            g.fillStyle = '#cfe9e5'; g.fillRect(230, y, 900, 14)
            g.fillStyle = col; g.fillRect(230, y + 18, 900 * v, 16)
        })
        g.fillStyle = '#6a7376'; g.font = '18px sans-serif'; g.fillText('slide 4 of 9 · ' + (n++), 70, 690)
    }
    draw()
    setInterval(draw, 200)
    const stream = c.captureStream(5)
    navigator.mediaDevices.getDisplayMedia = async () => stream
    return true
})()`

/** Click the button whose title or trailing text is `label` inside the people row of `name`. */
const rowAction = (b: Browser, name: string, label: string) =>
    b.eval<boolean>(`(() => {
        const row = [...document.querySelectorAll('.meet-row')].find((r) => r.querySelector('.meet-row-name b')?.textContent.trim().startsWith(${JSON.stringify(name)}))
        const btn = row && [...row.querySelectorAll('button')].find((x) => x.title === ${JSON.stringify(label)} || x.innerText.trim().endsWith(${JSON.stringify(label)}))
        if (!btn) return false
        btn.click()
        return true
    })()`)

/** Click a menu row by its words, once the menu has been drawn (up to 5 s). */
const menuRow = async (b: Browser, label: string) => {
    for (let i = 0; i < 50; i++) {
        const done = await b.eval<boolean>(`(() => {
            const r = [...document.querySelectorAll('.meet-menu-row')].find((x) => x.querySelector('span')?.textContent.trim() === ${JSON.stringify(label)})
            if (!r) return false
            r.click()
            return true
        })()`)
        if (done) return true
        await Bun.sleep(100)
    }
    return false
}

try {
    const simHttp = await bootSim()
    const port = await bootWeb(simHttp)
    const url = `http://127.0.0.1:${port}/`
    step(`simulator and web UI up (${url})`)

    const [ann, bob, carol, dave] = ['ann', 'bob', 'carol', 'dave'].map((n) => new Browser(n))
    browsers.push(ann, bob, carol, dave)
    for (const b of browsers) await b.launch(url)
    for (const b of browsers) await b.signIn(b.name)
    step('four signed in')

    await ann.eval('document.querySelector("details.chat-new").open = true')
    if (!(await ann.click('People who share several topics'))) throw new Error('no Start a community')
    await ann.type('input[placeholder="Garage 44"]', 'Street party crew')
    await ann.type('input[placeholder="general, meet-ups"]', 'q4 planning')
    await ann.type('input[placeholder="Usernames, separated by commas"]', 'bob, carol, dave')
    await ann.submit('.chat-form')
    await waitFor('the community on the rail', () =>
        ann.eval('[...document.querySelectorAll(".home-tile")].some((b) => b.title === "Street party crew")'), 30000)
    for (const b of [bob, carol, dave, ann]) await b.openTopic('Street party crew', 'q4-planning', 60000)
    step('a community with a topic all four are in')

    // Ann plans the meeting for now, with an agenda; Bob says he comes.
    if (!(await ann.click('Plan a meeting: a call at a time, with an agenda'))) throw new Error('no Plan a meeting key')
    await ann.until('Post the plan')
    await ann.type('input[placeholder="Q4 planning"]', 'Q4 planning')
    await ann.eval(`(() => {
        const d = new Date(); d.setSeconds(0, 0)
        const v = new Date(d.getTime() - d.getTimezoneOffset() * 60000).toISOString().slice(0, 16)
        const el = document.querySelector('input[type=datetime-local]')
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v)
        el.dispatchEvent(new Event('input', {bubbles: true}))
    })()`)
    await ann.type('textarea[placeholder^="Budget"]', 'Budget 10\nStage and sound 15')
    if (!(await ann.click('Post the plan'))) throw new Error('no Post the plan')
    await bob.until('Q4 planning', 30000)
    await waitFor('the plan card on bob\'s page', () => bob.eval(`!!document.querySelector('.chat-plan')`), 30000)
    if (!(await bob.click('Going'))) throw new Error('no Going on the plan')
    await waitFor('bob going, on ann\'s card', () =>
        ann.eval(`document.querySelector('.chat-plan .chat-mcard-people')?.innerText.includes('bob') ?? false`), 30000)
    step('ann planned the meeting with an agenda; bob said he comes, and ann saw it')

    // Bob asks a poll; Carol votes; Ann sees the count.
    if (!(await bob.click('Ask a poll'))) throw new Error('no poll key')
    await bob.type('input[placeholder="Which day suits?"]', 'Which day for the party?')
    await bob.type('textarea[placeholder^="Friday"]', 'Friday\nSaturday')
    if (!(await bob.click('Ask'))) throw new Error('no Ask')
    await waitFor('the poll on carol\'s page', () => carol.eval(`!!document.querySelector('.chat-poll')`), 30000)
    await carol.eval(`[...document.querySelectorAll('.chat-poll-bar')].find((b) => b.innerText.includes('Saturday')).click()`)
    await waitFor('carol\'s vote on ann\'s page', () =>
        ann.eval(`[...document.querySelectorAll('.chat-poll-bar')].some((b) => b.innerText.includes('Saturday') && b.querySelector('b')?.textContent === '1')`), 30000)
    step('bob asked a poll; carol voted, and ann saw the count')
    if (shots) {
        mkdirSync(shots, {recursive: true})
        await ann.shot(path.join(shots, 'plan-poll.png'))
    }

    // Ann calls; both answer with their cameras.
    await ann.eval(STAND_IN_SCREEN)
    if (!(await ann.click('Video call'))) throw new Error('no Video call key')
    // A topic's call opens the check before joining first.
    await ann.until('Start a call')
    if (!(await ann.click('Start'))) throw new Error('no Start in the check before joining')
    step('the check before joining opened, and ann started from it')
    for (const b of [bob, carol]) {
        await b.until('Incoming call', 30000)
        if (!(await b.click('Video'))) throw new Error(`no Video key on ${b.name}'s ringing call`)
    }
    const seesOthers = (b: Browser, n: number) =>
        waitFor(`${b.name} sees ${n} others' video`, () =>
            b.eval(`[...document.querySelectorAll('.chat-call-tile:not(.is-self) video')]
                .filter((v) => v.videoWidth > 0 && v.currentTime > 0).length >= ${n}`), 60000)
    for (const b of [ann, bob, carol]) await seesOthers(b, 2)
    step('a three-way video call, everybody seeing everybody')

    // Everybody puts the meeting in the middle; Ann opens the people.
    for (const b of [ann, bob, carol]) await b.click('Put the call in the middle')
    await ann.click('People in the call')
    await waitFor('ann hosts', () =>
        ann.eval(`[...document.querySelectorAll('.meet-row')].some((r) => r.innerText.includes('You') && r.innerText.includes('HOST'))`))
    step('the first one in hosts the call')

    // Bob raises a hand: Ann, hosting, is told, and the queue says who.
    await bob.click('Raise your hand (H)')
    await ann.until('bob raised a hand', 20000)
    await waitFor('the queue in the people panel', () => ann.eval(`document.querySelector('.meet-panel')?.innerText.includes('HANDS UP') ?? false`))
    step('bob raised a hand; ann was told, with Let talk in the notice')

    // Bob reacts, sealed: Ann sees it over his tile.
    await bob.click('React')
    if (!(await bob.click('React 👏'))) throw new Error('no 👏 in the picker')
    await waitFor('the reaction over bob', () => ann.eval(`[...document.querySelectorAll('.meet-tile-react')].some((r) => r.textContent.includes('👏'))`), 20000)
    step('bob reacted, and ann saw it: sealed by bob, opened by ann')

    if (shots) {
        mkdirSync(shots, {recursive: true})
        await ann.shot(path.join(shots, 'host.png'))
    }

    // Ann mutes Carol: Carol is told, and the forwarder holds her sound.
    if (!(await rowAction(ann, 'carol', 'Host actions'))) throw new Error('no host actions for carol')
    if (!(await menuRow(ann, 'Mute'))) throw new Error('no Mute in the host menu')
    await carol.until('A host muted you', 20000)
    await carol.click('A host muted you')
    await carol.until('Raise your hand to ask to talk')
    step('ann muted carol, who was told and could not unmute')
    if (shots) await carol.shot(path.join(shots, 'held.png'))

    // Ann lets Bob talk from the queue.
    await rowAction(ann, 'bob', 'Let talk')
    await waitFor('bob\'s hand down', () => bob.eval(`!document.querySelector('.meet-key.is-hand')`), 20000)
    step('ann let bob talk: his hand went down')

    // Ann shares her screen: Bob's stage becomes it.
    await ann.click('Share your screen')
    await waitFor('ann sharing', () => ann.eval(`!!document.querySelector('.meet-sharing')`), 20000)
    await waitFor('the screen on bob\'s stage', () =>
        bob.eval(`[...document.querySelectorAll('.meet-screen video')].some((v) => v.videoWidth > 0 && v.currentTime > 0)`), 45000)
    await bob.until('ann is presenting')
    step('ann shared a screen; it is the stage on bob\'s page')
    if (shots) {
        await bob.shot(path.join(shots, 'presenting.png'))
        await ann.shot(path.join(shots, 'sharing.png'))
    }

    await ann.click('Stop sharing')

    // Ann spotlights Carol: Carol is the big tile on Bob's page.
    await rowAction(ann, 'carol', 'Host actions')
    if (!(await menuRow(ann, 'Spotlight for everyone'))) throw new Error('no Spotlight in the host menu')
    await waitFor('carol large on bob\'s stage', () => bob.eval(`document.querySelector('.meet-big figcaption')?.innerText.includes('carol') ?? false`), 20000)
    await rowAction(ann, 'carol', 'Host actions')
    await menuRow(ann, 'Stop the spotlight')
    step('ann spotlighted carol for everyone, and took it off again')

    // Breakout rooms: Bob and Carol in two rooms, apart; then back together.
    await ann.click('More')
    await menuRow(ann, 'Breakout rooms…')
    if (!(await ann.click('Open 2 rooms'))) throw new Error('no Open 2 rooms')
    for (const b of [bob, carol]) await b.until('In Room', 20000)
    // Each sees neither the other nor ann: a room's media stays in it.
    await waitFor('bob alone in his room', () =>
        bob.eval(`![...document.querySelectorAll('.chat-call-tile:not(.is-self) figcaption')].some((c) => /carol|ann/.test(c.innerText))`), 20000)
    if (shots) await bob.shot(path.join(shots, 'room.png'))
    await ann.click('Bring everybody back')
    await waitFor('bob back with everybody', () =>
        bob.eval(`[...document.querySelectorAll('.chat-call-tile:not(.is-self) figcaption')].some((c) => c.innerText.includes('carol'))`), 20000)
    step('ann split the call into two rooms, kept apart, and brought everybody back')

    // Recording: refused until the hosts allow it, then everybody is told.
    await ann.click('More')
    await menuRow(ann, 'Anybody may record')
    await ann.eval('document.querySelector(".meet-scrim")?.click()')
    await bob.click('More')
    await waitFor('bob may record', () => menuRow(bob, 'Record on this device'), 20000)
    await ann.until('bob is recording', 20000)
    await carol.until('REC · bob')
    await bob.click('Stop and save')
    step('the hosts allowed recording; bob recorded, and everybody was told')

    // Captions are made on the device or not at all: a headless browser has no
    // on-device speech, so it says so rather than sending the voice away.
    await bob.click('Captions on (C)')
    await waitFor('captions answered', () =>
        bob.eval(`/captions|Captions/.test(document.querySelector('.meet-toasts')?.innerText ?? '') || !!document.querySelector('.meet-key.is-sel[title^="Captions off"]')`), 20000)
    step('captions: on-device only, and said so where the browser cannot')

    // A waiting room: Dave answers the ringing call and waits; Ann lets him in.
    await ann.click('More')
    await menuRow(ann, 'Members wait to be let in')
    await ann.eval('document.querySelector(".meet-scrim")?.click()')
    await dave.until('Incoming call', 20000)
    if (!(await dave.click('Answer'))) throw new Error('no Answer on dave\'s ringing call')
    await dave.until('Waiting for a host to let you in', 20000)
    await ann.until('dave is waiting', 20000)
    if (shots) await ann.shot(path.join(shots, 'knock.png'))
    if (!(await ann.click('Admit'))) throw new Error('no Admit in the knock notice')
    await waitFor('dave in the call', () =>
        dave.eval(`!document.querySelector('.meet-waiting') && !!document.querySelector('.chat-call-tile:not(.is-self)')`), 30000)
    step('dave waited until ann let him in')

    // Notes: Ann notes a decision; Bob sees it in his notes panel.
    for (const b of [ann, bob]) await b.click('Notes, polls and the agenda')
    await ann.click('Decision')
    await ann.type('textarea[placeholder="What was decided"]', 'The gazebos stay in the budget')
    await ann.submit('.meet-note-add')
    await waitFor('the decision on bob\'s panel', () =>
        bob.eval(`document.querySelector('.meet-notes')?.innerText.includes('The gazebos stay') ?? false`), 30000)
    step('ann noted a decision; bob saw it')

    // The agenda: Ann starts the first item; Bob sees where the meeting is.
    for (const b of [ann, bob]) await b.eval(`[...document.querySelectorAll('.meet-tab')].find((t) => t.innerText.startsWith('Agenda')).click()`)
    if (!(await ann.click('Start "Budget"'))) throw new Error('no Start on the agenda')
    await waitFor('the agenda on bob\'s panel', () =>
        bob.eval(`document.querySelector('.meet-agenda li.is-now')?.innerText.includes('Budget') ?? false`), 30000)
    step('ann started the agenda; bob saw the item and its clock')
    if (shots) await bob.shot(path.join(shots, 'agenda.png'))

    // Ann ends it for everyone.
    await ann.click('Leave the call')
    if (!(await menuRow(ann, 'End for everyone'))) throw new Error('no End for everyone')
    for (const b of [bob, carol, dave]) {
        await waitFor(`${b.name}'s call ended`, () => b.eval('!document.querySelector(".chat-call")'), 20000)
        await b.until('ann ended the call for everyone')
    }
    step('ann ended the call for everyone')

    // Ann's device, hosting, posted what the meeting came to.
    await waitFor('the record on bob\'s page', () =>
        bob.eval(`document.querySelector('.chat-record')?.innerText.includes('The gazebos stay') ?? false`), 30000)
    step('the meeting record arrived in the chat, with the decision in it')
    if (shots) await bob.shot(path.join(shots, 'record.png'))
    // A page that kept working past an error it did not catch is not a pass.
    const errors = browsers.flatMap((b) => b.logs.filter((l) => /^uncaught|call: closing/.test(l)).map((l) => `${b.name}: ${l}`))
    if (errors.length) throw new Error(`errors in the pages:\n${errors.join('\n')}`)
    console.log('ok')
} catch (e) {
    console.error(`FAILED: ${(e as Error).message}`)
    for (const b of browsers) {
        const text = await b.eval<string>('document.body.innerText').catch(() => '(page gone)')
        console.error(`--- ${b.name}'s page:\n${text.slice(0, 1500)}`)
        console.error(`--- ${b.name}'s console:\n${b.logs.slice(-30).join('\n')}`)
    }
    const log = await Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')).text().catch(() => '')
    console.error(`--- simulator:\n${log.split('\n').filter((l) => /call|join|warn|error/i.test(l)).slice(-30).join('\n')}`)
    if (shots) {
        mkdirSync(shots, {recursive: true})
        for (const b of browsers) await b.shot(path.join(shots, `${b.name}-failed.png`)).catch(() => undefined)
    }
    process.exitCode = 1
} finally {
    for (const p of procs) p.kill()
}
