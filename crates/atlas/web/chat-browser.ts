#!/usr/bin/env bun
/**
 * The chat page in two real browsers, against a real simulator.
 *
 *   bun run build:chat-core --dev && bun run build
 *   bun chat-browser.ts --sim "$CARGO_TARGET_DIR/debug/simulator" --shots /tmp/chat
 *
 * What `chat-e2e.ts` cannot reach: the `SharedWorker`, its Web Lock, IndexedDB,
 * and the page itself. Two headless Chromium profiles (two people, two
 * browsers) sign in through the form, start a community with a topic, talk in
 * it, and one reloads to read the history back out of its own storage. Driven
 * over the DevTools Protocol, as `tools/cdp_probe.ts` is, so it needs no npm
 * package. `--shots DIR` writes a screenshot of each browser at the end.
 */
import {mkdirSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'

import {Browser, bootSim, bootWeb, procs, shots, waitFor} from './browser-harness'

/**
 * A community's page opens on its public posts; its topics are the other tab
 * of a segmented field (not a button), so the innermost element with the label.
 */
function showTopics(b: Browser): Promise<unknown> {
    return b.eval(`[...document.querySelectorAll('*')].reverse()
        .find((e) => e.innerText?.trim().startsWith('Topics ·'))?.click()`)
}

/** Every browser launched, for the failure report. */
const browsers: Browser[] = []

const started = Date.now()
const step = (s: string) => console.log(`${((Date.now() - started) / 1000).toFixed(1).padStart(5)} s  ${s}`)

try {
    const simHttp = await bootSim()
    const port = await bootWeb(simHttp)
    // The site's front door: the chat, or its welcome card when signed out.
    const url = `http://127.0.0.1:${port}/`
    step(`simulator and web UI up (${url})`)

    const ann = new Browser('ann')
    const bob = new Browser('bob')
    browsers.push(ann, bob)
    await ann.launch(url)
    await bob.launch(url)
    await ann.until('Open the map')
    if (shots) {
        mkdirSync(shots, {recursive: true})
        await ann.shot(path.join(shots, 'welcome.png'))
    }
    await ann.signIn('ann')
    await bob.signIn('bob')
    step('both signed in through the form; the shared worker holds their keys')

    // Ann: a community, with Bob in it and a first topic.
    await ann.eval('document.querySelector("details.chat-new").open = true')
    // The menu's buttons carry a line under their name, so match that.
    if (!(await ann.click('People who share several topics'))) throw new Error('no Start a community')
    await ann.type('input[placeholder="Garage 44"]', 'Atelier Noord')
    await ann.type('input[placeholder="general, meet-ups"]', 'ebbinge quarter')
    await ann.type('input[placeholder="Usernames, separated by commas"]', 'bob')
    await ann.submit('.chat-form')
    await waitFor('the community on the rail', () =>
        ann.eval('[...document.querySelectorAll(".home-tile")].some((b) => b.title === "Atelier Noord")'), 30000)
    step('ann started a community with #ebbinge-quarter and bob in it')

    // Bob: the community appears, and ann's browser adds him to its topic.
    await bob.openTopic('Atelier Noord', 'ebbinge-quarter')
    step("bob is in the community, and ann's browser added him to its topic")

    // They talk.
    await ann.openTopic('Atelier Noord', 'ebbinge-quarter')
    await ann.type('.chat-composer textarea', 'Scaffolding is down on the north side @bob')
    await ann.submit('.chat-composer')
    await bob.until('Scaffolding is down on the north side')
    await bob.type('.chat-composer textarea', 'The brick reads darker than the sample.')
    await bob.submit('.chat-composer')
    await ann.until('The brick reads darker than the sample.')
    // A reaction from Ann on Bob's line.
    await ann.eval(`(() => {
        const msg = [...document.querySelectorAll('.chat-msg')].find((m) => m.innerText.includes('brick reads darker'))
        msg.querySelector('.chat-actions button').click()
    })()`)
    await bob.until('👍 1')
    step('a line each way through the page, with a mention and a reaction')

    // A video call in the topic: ann calls, bob's page rings, he answers, and
    // each sees the other's picture through the forwarder, sealed both ways.
    if (!(await ann.click('Video call'))) throw new Error('no Video call key')
    await bob.until('Incoming call', 30000)
    if (!(await bob.click('Video'))) throw new Error('no Video key on the ringing call')
    const connected = (b: Browser) =>
        waitFor(`${b.name}'s call connected`, () =>
            b.eval('/\\d+:\\d\\d/.test(document.querySelector(".chat-call-bar")?.textContent ?? "")'), 45000)
    await connected(ann)
    await connected(bob)
    // Somebody else's video playing, with frames decoded (not the self view).
    const seesVideo = (b: Browser) =>
        waitFor(`${b.name} sees the other's video`, () =>
            b.eval(`[...document.querySelectorAll('.chat-call-tile:not(.is-self) video')]
                .some((v) => v.videoWidth > 0 && v.currentTime > 0)`), 45000)
    await seesVideo(ann)
    await seesVideo(bob)
    // And it went as VP9 (`webcall.js`, `sendCodecs`), which is what the
    // browser's own outgoing statistics say it encoded.
    const sentCodec = (b: Browser) =>
        b.eval<string | null>(`(async () => {
            const c = globalThis.cartopolisCall
            if (!c) return 'no call on the page'
            const stats = await c.up.getStats()
            let codec = null
            stats.forEach((r) => {
                if (r.type === 'outbound-rtp' && r.kind === 'video' && r.codecId) codec = stats.get(r.codecId)?.mimeType ?? null
            })
            return codec
        })()`)
    for (const b of [ann, bob]) {
        const codec = await sentCodec(b)
        if (codec !== 'video/VP9') throw new Error(`${b.name} sent ${codec}, not video/VP9`)
    }
    if (shots) {
        mkdirSync(shots, {recursive: true})
        await ann.shot(path.join(shots, 'call.png'))
    }
    // Ann hosts and Bob is still in it, so Leave asks: leave, or end it for everyone.
    await ann.click('Leave the call')
    if (!(await ann.click('The call goes on'))) throw new Error('no Leave in the leave menu')
    await waitFor('bob\'s call ended', () => bob.eval('!document.querySelector(".chat-call")'), 20000)
    await Bun.sleep(1000)
    if (await ann.has('Missed call')) throw new Error('the caller was told of a missed call after hanging up')
    step('a video call in the topic: rang, answered, each saw the other in VP9, hung up')

    // Bob reloads: the history comes back out of his browser's own storage.
    await bob.send('Page.reload')
    // The topic returns to the list once the worker has signed in again.
    await bob.openTopic('Atelier Noord', 'ebbinge-quarter')
    await bob.until('Scaffolding is down on the north side')
    step('after a reload bob reads the history back out of IndexedDB')

    // Bob signs in on a second browser after all that was said — a new phone.
    // A device already in the topic adds it, and shares what was said before.
    const bob2 = new Browser('bob2')
    browsers.push(bob2)
    await bob2.launch(url)
    await bob2.signIn('bob')
    await bob2.openTopic('Atelier Noord', 'ebbinge-quarter', 60000)
    await bob2.until('shared by', 30000)
    await bob2.until('Scaffolding is down on the north side')
    step('a second device of bob\'s was added to the topic and handed the earlier messages')

    // Ann makes an invite link; Carol, who is in nothing, opens it and joins
    // straight away although the community asks first.
    await ann.openCommunity('Atelier Noord')
    if (!(await ann.click('Make an invite link'))) throw new Error('no Make an invite link')
    await waitFor('the invite link', () => ann.eval('!!document.querySelector(".chat-invite-link")'), 20000)
    const link = await ann.eval<string>('document.querySelector(".chat-invite-link").innerText')
    const code = new URL(link).searchParams.get('community')
    const carol = new Browser('carol')
    browsers.push(carol)
    await carol.launch(url)
    await carol.signIn('carol')
    await carol.eval(`location.search = '?community=${code}'`)
    await carol.until('You were invited', 30000)
    if (!(await carol.click('Join'))) throw new Error('no Join on the invitation')
    await carol.openTopic('Atelier Noord', 'ebbinge-quarter', 60000)
    step('carol joined with an invite link and was added to the topic')

    // Bob mutes the topic: it says so, and counts nothing.
    await bob.openCommunity('Atelier Noord')
    await showTopics(bob)
    await waitFor('bob\'s Mute key', () => bob.click('Mute'))
    await waitFor('the topic marked muted', () =>
        bob.eval('[...document.querySelectorAll(".chat-item")].some((b) => b.innerText.includes("muted"))'))
    await ann.openTopic('Atelier Noord', 'ebbinge-quarter')
    await ann.type('.chat-composer textarea', 'Muted, this one')
    await ann.submit('.chat-composer')
    await Bun.sleep(1500)
    if (await bob.eval('!![...document.querySelectorAll(".home-tile")].find((b) => b.title === "Atelier Noord")?.querySelector(".chat-count")')) {
        throw new Error('a muted topic counted')
    }
    step('bob muted the topic: marked, and a new line counts nothing')

    if (shots) {
        mkdirSync(shots, {recursive: true})
        await ann.shot(path.join(shots, 'ann.png'))
        await bob.shot(path.join(shots, 'bob.png'))
        // The community's own page, and the directory.
        await ann.openCommunity('Atelier Noord')
        await showTopics(ann)
        await ann.until('Everyone here is in every topic')
        await ann.shot(path.join(shots, 'community.png'))
        await ann.click('Find a community')
        await ann.until('Atelier Noord')
        await Bun.sleep(200)
        await ann.shot(path.join(shots, 'find.png'))
        // The other theme, pinned the way the theme switch pins it.
        await ann.eval('document.documentElement.dataset.theme = "dark"')
        await Bun.sleep(100)
        await ann.shot(path.join(shots, 'ann-dark.png'))
        step(`screenshots in ${shots}`)
    }
    console.log('ok')
} catch (e) {
    console.error(`FAILED: ${(e as Error).message}`)
    for (const b of browsers) {
        const text = await b.eval<string>('document.body.innerText').catch(() => '(page gone)')
        console.error(`--- ${b.name}'s page:\n${text.slice(0, 1500)}`)
        console.error(`--- ${b.name}'s console:\n${b.logs.slice(-30).join('\n')}`)
    }
    const log = await Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')).text().catch(() => '')
    console.error(`--- simulator:\n${log.split('\n').filter((l) => /join|channel|e2ee|warn|error/i.test(l)).slice(-30).join('\n')}`)
    if (shots) {
        mkdirSync(shots, {recursive: true})
        for (const b of browsers) await b.shot(path.join(shots, `${b.name}-failed.png`)).catch(() => undefined)
    }
    process.exitCode = 1
} finally {
    for (const p of procs) p.kill()
}
