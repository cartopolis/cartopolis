#!/usr/bin/env bun
/**
 * A lost phone, in real browsers against a real simulator: the chat backup
 * and its recovery key (`cartopolis_chat::backup`).
 *
 *   bun run build:chat-core --dev && bun run build
 *   bun recovery-browser.ts --sim "$CARGO_TARGET_DIR/debug/simulator" --shots /tmp/recovery
 *
 * Ann and Bob talk in a community's topic; Bob sets up a recovery key. Then
 * both browsers go, so nobody's device is online to add a new one or share
 * what was said. Bob signs in on a fresh browser, types his key, and reads the
 * topic at once, from his backup. When Ann comes back, her browser adds Bob's
 * new one to the topic and he can write in it again.
 */
import {mkdirSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'

import {Browser, bootSim, bootWeb, procs, shots, waitFor} from './browser-harness'

const browsers: Browser[] = []
const started = Date.now()
const step = (s: string) => console.log(`${((Date.now() - started) / 1000).toFixed(1).padStart(5)} s  ${s}`)
const shot = async (b: Browser, name: string) => {
    if (!shots) return
    mkdirSync(shots, {recursive: true})
    await Bun.sleep(200)
    await b.shot(path.join(shots, name))
}

/** Press Bring back once the page has drawn it enabled for what was typed. */
async function bringBack(b: Browser): Promise<void> {
    await waitFor('Bring back enabled', () =>
        b.eval(`[...document.querySelectorAll('button')].some((x) => x.innerText.trim() === 'Bring back' && !x.disabled)`))
    await b.click('Bring back')
}

/** The community page's topic cards, rather than the sidebar's joined topics. */
async function openTopicCard(b: Browser, community: string, topic: string): Promise<void> {
    await waitFor(`${community} on ${b.name}'s rail`, () =>
        b.eval(`!![...document.querySelectorAll('.home-tile')].find((t) => t.title === ${JSON.stringify(community)})`), 30000)
    await b.openCommunity(community)
    await waitFor(`the topics tab for ${b.name}`, () =>
        b.eval(`(() => {
            const tab = [...document.querySelectorAll('*')].reverse().find((e) => e.innerText?.trim().startsWith('Topics ·'))
            tab?.click()
            return !!tab
        })()`))
    const card = `[...document.querySelectorAll('.chat-card')].find((c) => c.innerText.includes(${JSON.stringify(topic)}) && !c.disabled)`
    await waitFor(`a readable #${topic} for ${b.name}`, () => b.eval(`!!${card}`), 30000)
    await b.eval(`${card}.click()`)
}

try {
    const simHttp = await bootSim()
    const port = await bootWeb(simHttp)
    const url = `http://127.0.0.1:${port}/`
    step(`simulator and web UI up (${url})`)

    const ann = new Browser('ann')
    const bob = new Browser('bob')
    browsers.push(ann, bob)
    await ann.launch(url)
    await bob.launch(url)
    await ann.signIn('ann')
    await bob.signIn('bob')

    await ann.eval('document.querySelector("details.chat-new").open = true')
    if (!(await ann.click('People who share several topics'))) throw new Error('no Start a community')
    await ann.type('input[placeholder="Garage 44"]', 'Atelier Noord')
    await ann.type('input[placeholder="general, meet-ups"]', 'ebbinge quarter')
    await ann.type('input[placeholder="Usernames, separated by commas"]', 'bob')
    await ann.submit('.chat-form')
    await bob.openTopic('Atelier Noord', 'ebbinge-quarter', 60000)
    await ann.openTopic('Atelier Noord', 'ebbinge-quarter')
    await ann.type('.chat-composer textarea', 'Scaffolding is down on the north side')
    await ann.submit('.chat-composer')
    await bob.until('Scaffolding is down on the north side')
    await bob.type('.chat-composer textarea', 'The brick reads darker than the sample.')
    await bob.submit('.chat-composer')
    await ann.until('The brick reads darker than the sample.')
    step('ann and bob talked in #ebbinge-quarter')

    // Bob is offered a recovery key, makes one, and is shown it once.
    await bob.until('Lose this device and you lose these chats', 30000)
    await shot(bob, 'offer.png')
    if (!(await bob.click('Set up'))) throw new Error('no Set up on the offer')
    await waitFor('the recovery key', () => bob.eval('!!document.querySelector(".chat-recovery-key")'))
    const key = await bob.eval<string>('document.querySelector(".chat-recovery-key").innerText.trim()')
    if (!/^[0-9A-Z]{4}(-[0-9A-Z]{4}){5}$/.test(key)) throw new Error(`a recovery key that looks like ${key}`)
    await shot(bob, 'key.png')
    await bob.click("I've kept it")
    await waitFor('the key gone from the screen', async () => !(await bob.has(key)))
    step(`bob made a recovery key (${key.slice(0, 4)}-…) and was shown it once`)
    // Sealed and stored as soon as the key is made.
    await Bun.sleep(3000)

    // Both browsers close, Bob's for good, as a lost phone. Closed, not
    // navigated away: a chat worker can outlive its page, and the server asks
    // a device it still thinks is connected to add a new one, then does not
    // ask again for two minutes (`e2ee::ASK_AGAIN_MS`).
    for (const b of [ann, bob]) {
        // No answer comes from a browser that closes, and nothing more is asked of it.
        await Promise.race([b.send('Browser.close'), Bun.sleep(2000)])
        browsers.splice(browsers.indexOf(b), 1)
    }
    await Bun.sleep(3000)
    step('ann and bob closed their browsers: no device in the topic is online')

    // A new browser for Bob: the backup is there, and the key opens it.
    const bob2 = new Browser('bob2')
    browsers.push(bob2)
    await bob2.launch(url)
    await bob2.signIn('bob')
    await bob2.until('Your chats have a backup', 30000)
    await shot(bob2, 'locked.png')
    // A wrong key is refused, and says so.
    await bob2.type('.chat-recovery input', 'AAAA-AAAA-AAAA-AAAA-AAAA-AAAA')
    await bringBack(bob2)
    await bob2.until('does not open this backup')
    await bob2.type('.chat-recovery input', key.toLowerCase().replaceAll('-', ' '))
    await bringBack(bob2)
    await bob2.until('Brought back 2 messages in 1 chat', 30000)
    step('a wrong key was refused; the right one, typed loosely, opened the backup')

    await openTopicCard(bob2, 'Atelier Noord', 'ebbinge-quarter')
    await bob2.until('Earlier messages, from your backup')
    await bob2.until('Scaffolding is down on the north side')
    await bob2.until('You can write here again as soon as somebody in this chat is online')
    await shot(bob2, 'restored.png')
    step("bob's new browser reads the topic from his backup with nobody else online")

    // Ann comes back; her browser adds Bob's new one, and he can write again.
    const annAgain = new Browser('ann')
    browsers.push(annAgain)
    await annAgain.launch(url, ann.profile)
    await annAgain.openTopic('Atelier Noord', 'ebbinge-quarter', 60000)
    step('ann is back in the topic')
    await waitFor("bob2's composer", () => bob2.eval('!!document.querySelector(".chat-composer textarea")'), 90000)
    await bob2.type('.chat-composer textarea', 'New phone, same chats.')
    await bob2.submit('.chat-composer')
    await annAgain.until('New phone, same chats.', 30000)
    await shot(bob2, 'rejoined.png')
    step("ann came back, her browser added bob's new one, and his line reached her")
    console.log('ok')
} catch (e) {
    console.error(`FAILED: ${(e as Error).message}`)
    for (const b of browsers) {
        const text = await b.eval<string>('document.body.innerText').catch(() => '(page gone)')
        console.error(`--- ${b.name}'s page:\n${text.slice(0, 1500)}`)
        console.error(`--- ${b.name}'s console:\n${b.logs.slice(-30).join('\n')}`)
    }
    const log = await Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')).text().catch(() => '')
    console.error(`--- simulator:\n${log.split('\n').filter((l) => /backup|picture|e2ee|warn|error/i.test(l)).slice(-30).join('\n')}`)
    if (shots) {
        mkdirSync(shots, {recursive: true})
        for (const b of browsers) await b.shot(path.join(shots, `${b.name}-failed.png`)).catch(() => undefined)
    }
    process.exitCode = 1
} finally {
    for (const p of procs) p.kill()
}
