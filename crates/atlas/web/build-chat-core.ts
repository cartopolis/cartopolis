#!/usr/bin/env bun
/**
 * Build the private-chat engine for the browser (`crates/web_chat`) into
 * `src/worker/pkg/`, where the chat worker imports it from.
 *
 *   bun run build:chat-core              # web-release, what the site ships
 *   bun run build:chat-core --dev        # unoptimised, for a quick loop
 *
 * cargo, then `wasm-bindgen --target web`. The `wasm-bindgen` on the PATH must
 * be the exact version `Cargo.lock` pins — its output is generated against the
 * crate's ABI and a mismatch fails at load, not at build — so this checks first
 * and says which one to install. `WASM_BINDGEN=/path/to/wasm-bindgen` picks
 * another binary.
 *
 * Not threaded, unlike the viewer's build: the worker needs no threads, and a
 * module that imports no shared memory loads in any worker on stable Rust.
 */
import {existsSync, mkdirSync, rmSync} from 'node:fs'
import path from 'node:path'

const web = import.meta.dir
const repo = path.resolve(web, '../../..')
const out = path.join(web, 'src/worker/pkg')
const dev = process.argv.includes('--dev')
const profile = dev ? 'dev' : 'web-release'
const bindgen = process.env.WASM_BINDGEN || 'wasm-bindgen'

function run(cmd: string[], cwd = repo): string {
    const p = Bun.spawnSync(cmd, {cwd, stderr: 'inherit', stdout: 'pipe'})
    if (p.exitCode !== 0) {
        throw new Error(`${cmd.join(' ')} failed (${p.exitCode})`)
    }
    return p.stdout.toString()
}

const lock = await Bun.file(path.join(repo, 'Cargo.lock')).text()
const want = lock.match(/name = "wasm-bindgen"\nversion = "([^"]+)"/)?.[1]
const have = run([bindgen, '--version']).trim().split(/\s+/)[1]
if (want !== have) {
    console.error(`wasm-bindgen ${have} on the PATH, but Cargo.lock wants ${want}.`)
    console.error(`cargo install wasm-bindgen-cli --version ${want} --locked, or set WASM_BINDGEN.`)
    process.exit(1)
}

run(['cargo', 'build', '-p', 'cartopolis_web_chat', '--target', 'wasm32-unknown-unknown', '--profile', profile])

const meta = JSON.parse(run(['cargo', 'metadata', '--format-version', '1', '--no-deps', '--offline']))
const dir = profile === 'dev' ? 'debug' : profile
const wasm = path.join(meta.target_directory, 'wasm32-unknown-unknown', dir, 'cartopolis_web_chat.wasm')

if (existsSync(out)) rmSync(out, {recursive: true})
mkdirSync(out, {recursive: true})
run([bindgen, '--target', 'web', '--out-dir', out, '--out-name', 'cartopolis_web_chat', wasm])

const bytes = await Bun.file(path.join(out, 'cartopolis_web_chat_bg.wasm')).bytes()
const kb = (n: number) => `${Math.round(n / 1024)} KB`
console.log(`built src/worker/pkg/ (${profile}): module ${kb(bytes.byteLength)}, ${kb(Bun.gzipSync(bytes).byteLength)} gzipped`)
