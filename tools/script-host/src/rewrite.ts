/**
 * Rewrite an author's script so the host can pause it.
 *
 * JavaScriptCore can stop a running script (a watchdog, `vm` timeouts) but never
 * suspend one, and the Luau host's whole scheduling model rests on suspending:
 * a handler that overruns its slice is descheduled and resumed next tick, so
 * `while (true) {}` costs its author a slow prim and its neighbours nothing
 * (docs/notes/prim-scripting.md). So the suspension is written into the script:
 *
 * - every function becomes a generator, wrapped by `__fn` so it is still a
 *   plain callable for builtins (`arr.map(f)`) — see `runtime.ts`;
 * - every call becomes `yield* __call(f, args)`, so a pause deep in a call chain
 *   unwinds to the host and resumes where it was;
 * - every loop body and every function entry starts with the fuel check, the
 *   counterpart of Luau's interrupt, which fires on the same two things.
 *
 * The rewrite is also the first gate of the sandbox. It refuses what it cannot
 * meter (classes, getters, async functions, the author's own generators) and what
 * reaches outside the script (`import()`), and it reserves every `__` name for the
 * runtime, since an author who could assign `__f` would have unlimited fuel.
 */
import * as acorn from 'acorn'
import {generate} from 'astring'

/** A refusal with the line the author has to fix. */
export class ScriptError extends Error {
    constructor(
        message: string,
        readonly line?: number,
    ) {
        super(line ? `line ${line}: ${message}` : message)
    }
}

type Node = {type: string; loc?: {start: {line: number}}; [key: string]: any}

const ident = (name: string): Node => ({type: 'Identifier', name})
const lit = (value: unknown): Node => ({type: 'Literal', value})

/** `if (--__f < 0 || ((__f & 1023) === 0 && __mem())) yield;` — the fuel check. */
function fuelCheck(): Node {
    const dec = {type: 'UpdateExpression', operator: '--', prefix: true, argument: ident('__f')}
    const out = {type: 'BinaryExpression', operator: '<', left: dec, right: lit(0)}
    const every = {
        type: 'BinaryExpression',
        operator: '===',
        left: {type: 'BinaryExpression', operator: '&', left: ident('__f'), right: lit(1023)},
        right: lit(0),
    }
    const mem = {type: 'CallExpression', callee: ident('__mem'), arguments: [], optional: false}
    const test = {
        type: 'LogicalExpression',
        operator: '||',
        left: out,
        right: {type: 'LogicalExpression', operator: '&&', left: every, right: mem},
    }
    return {
        type: 'IfStatement',
        test,
        consequent: {type: 'ExpressionStatement', expression: {type: 'YieldExpression', delegate: false, argument: null}},
        alternate: null,
    }
}

function refuse(node: Node, message: string): never {
    throw new ScriptError(message, node.loc?.start.line)
}

/**
 * A call, as an expression that asks the callee what it is:
 *
 *     (__c1 = f, __a1 = [args], __g1 = __gens.get(__c1))
 *         ? yield* __apply(__g1, undefined, __a1)   // the author's function: pausable
 *         : __apply(__c1, undefined, __a1)          // a built-in or the API: a plain call
 *
 * Inline rather than through a helper generator, because a generator is the
 * expensive part — about 136 ns to create and delegate to on 1.4.4, against 7 ns
 * for a plain call — and a helper put one on *every* call, `Math.sin` included.
 * The temporaries are per call site (`__c1`, `__c2`, …) and per function, so a
 * nested call in the arguments or a recursive call cannot overwrite them.
 */
function callSite(n: Node, optional: boolean): Node {
    const scope = scopes[scopes.length - 1]
    const i = ++scope.calls
    const c = ident(`__c${i}`)
    const a = ident(`__a${i}`)
    const g = ident(`__g${i}`)
    const o = ident(`__o${i}`)
    const assign = (left: Node, right: Node): Node => ({type: 'AssignmentExpression', operator: '=', left, right})
    const isNull = (e: Node): Node => ({type: 'BinaryExpression', operator: '==', left: e, right: lit(null)})
    const apply = (fn: Node, self: Node): Node => ({type: 'CallExpression', optional: false, callee: ident('__apply'), arguments: [fn, self, a]})
    const callee = n.callee
    const member = callee.type === 'MemberExpression'
    const self = member ? o : ident('undefined')

    const steps: Node[] = []
    if (member) {
        steps.push(assign(o, callee.object))
        steps.push(assign(c, {type: 'MemberExpression', object: o, property: callee.property, computed: callee.computed, optional: false}))
    } else {
        steps.push(assign(c, callee))
    }
    steps.push(assign(a, {type: 'ArrayExpression', elements: n.arguments}))
    const lookup = {type: 'CallExpression', optional: false, callee: {type: 'MemberExpression', object: ident('__gens'), property: ident('get'), computed: false, optional: false}, arguments: [c]}
    steps.push(assign(g, lookup))
    let expr: Node = {
        type: 'ConditionalExpression',
        test: {type: 'SequenceExpression', expressions: steps},
        consequent: {type: 'YieldExpression', delegate: true, argument: apply(g, self)},
        alternate: apply(c, self),
    }
    if (optional) {
        // `a?.b()` and `f?.()`: nothing is evaluated past a nullish link. The
        // first step is the object (or the callee), so test it before the rest.
        const [first, ...rest] = steps
        const guard: Node = member
            ? {type: 'LogicalExpression', operator: '||', left: isNull(first), right: isNull(rest[0])}
            : isNull(first)
        expr = {
            type: 'ConditionalExpression',
            test: guard,
            consequent: ident('undefined'),
            alternate: {...expr, test: {type: 'SequenceExpression', expressions: member ? rest.slice(1) : rest}},
        }
    }
    return expr
}

/** `let __c1, __a1, __g1, __o1, …` for a function's call sites. */
function temporaries(scope: Scope): Node[] {
    if (scope.calls === 0) return []
    const declarations: Node[] = []
    for (let i = 1; i <= scope.calls; i++) {
        for (const p of ['c', 'a', 'g', 'o']) declarations.push({type: 'VariableDeclarator', id: ident(`__${p}${i}`), init: null})
    }
    return [{type: 'VariableDeclaration', kind: 'let', declarations}]
}

/** Turn any function node into `__fn(function* (...__a) { let [params] = __a; check; body })`. */
function generatorOf(node: Node): Node {
    if (node.async) refuse(node, 'async functions cannot be paused by the host; use sleep() to wait')
    if (node.generator) refuse(node, 'generator functions are reserved for the host')
    const body: Node =
        node.body.type === 'BlockStatement'
            ? node.body
            : {type: 'BlockStatement', body: [{type: 'ReturnStatement', argument: node.body}]}
    // Parameters move into the body: their defaults may call functions, and a
    // call is a `yield*`, which a generator's parameter list may not contain.
    // The call temporaries come first, since a default may itself be a call.
    const prologue: Node[] = temporaries(scopes[scopes.length - 1])
    if (node.params.length) {
        prologue.push({
            type: 'VariableDeclaration',
            kind: 'let',
            declarations: [{type: 'VariableDeclarator', id: {type: 'ArrayPattern', elements: node.params}, init: ident('__a')}],
        })
    }
    prologue.push(fuelCheck())
    body.body.unshift(...prologue)
    let gen: Node = {
        type: 'FunctionExpression',
        id: null,
        params: [{type: 'RestElement', argument: ident('__a')}],
        body,
        generator: true,
        async: false,
    }
    // An arrow's `this` is its surroundings', which a generator does not inherit.
    if (node.type === 'ArrowFunctionExpression') {
        gen = {
            type: 'CallExpression',
            optional: false,
            callee: {type: 'MemberExpression', object: gen, property: ident('bind'), computed: false, optional: false},
            arguments: [{type: 'ThisExpression'}],
        }
    }
    return {type: 'CallExpression', callee: ident('__fn'), arguments: [gen], optional: false}
}

/** The function being rewritten: how many call sites it has, for their temporaries. */
type Scope = {calls: number}
const scopes: Scope[] = []

const isFunction = (n: Node) => n.type === "FunctionDeclaration" || n.type === "FunctionExpression" || n.type === "ArrowFunctionExpression"

function walk(node: any, inChain: boolean): any {
    if (!node || typeof node !== "object") return node
    if (Array.isArray(node)) return node.map((n) => walk(n, inChain))
    const n = node as Node
    if (isFunction(n)) {
        scopes.push({calls: 0})
        try {
            return walkInner(n, inChain)
        } finally {
            scopes.pop()
        }
    }
    return walkInner(n, inChain)
}

function walkInner(n: Node, inChain: boolean): any {

    switch (n.type) {
        case 'Identifier':
            if (n.name.startsWith('__')) refuse(n, `names starting with __ are reserved for the host (${n.name})`)
            if (n.name === 'arguments') refuse(n, 'use rest parameters (...args) instead of arguments')
            return n
        case 'ImportExpression':
            return refuse(n, 'scripts cannot import modules')
        case 'ClassDeclaration':
        case 'ClassExpression':
            return refuse(n, 'classes are not supported yet; use functions and objects')
        case 'YieldExpression':
        case 'AwaitExpression':
            return refuse(n, 'yield and await are reserved for the host; use sleep() to wait')
        case 'Property':
            if (n.kind !== 'init') refuse(n, 'getters and setters are not supported')
            break
    }

    const chain = n.type === 'ChainExpression' || inChain
    for (const key of Object.keys(n)) {
        if (key === 'type' || key === 'loc' || key === 'start' || key === 'end') continue
        n[key] = walk(n[key], chain && n.type !== 'FunctionExpression' && n.type !== 'ArrowFunctionExpression')
    }

    switch (n.type) {
        case 'FunctionDeclaration':
            return {
                type: 'VariableDeclaration',
                kind: 'var',
                declarations: [{type: 'VariableDeclarator', id: n.id, init: generatorOf(n)}],
            }
        case 'FunctionExpression':
        case 'ArrowFunctionExpression':
            return generatorOf(n)
        case 'Property':
            // `{ f() {} }` — after the rewrite its value is a call, which a method cannot be.
            n.method = false
            return n
        case 'WhileStatement':
        case 'DoWhileStatement':
        case 'ForStatement':
        case 'ForInStatement':
        case 'ForOfStatement': {
            const body: Node = n.body.type === 'BlockStatement' ? n.body : {type: 'BlockStatement', body: [n.body]}
            body.body.unshift(fuelCheck())
            n.body = body
            return n
        }
        case 'CallExpression':
            return callSite(n, chain || n.optional)
        case 'ChainExpression':
            // Its calls are now `yield*`s, which short-circuit themselves.
            return n.expression
    }
    return n
}

/** The rewritten script: the body of a generator that defines the handlers and returns them. */
export function rewrite(source: string, handlers: readonly string[]): string {
    let ast: Node
    try {
        ast = acorn.parse(source, {ecmaVersion: 2023, sourceType: 'script', locations: true}) as unknown as Node
    } catch (e: any) {
        throw new ScriptError(e.message.replace(/ \(\d+:\d+\)$/, ''), e.loc?.line)
    }
    scopes.length = 0
    scopes.push({calls: 0})
    const body = walk(ast, false)
    body.body.unshift(...temporaries(scopes.pop()!))
    // Handlers are found by name after the top level ran, as the Luau host's
    // `raw_get` does: a `typeof` reads the binding and runs no author code.
    const found = handlers.map((h) => `${h}: typeof ${h} === "function" ? ${h} : undefined`).join(', ')
    return `${generate(body)}\nreturn {${found}};`
}
