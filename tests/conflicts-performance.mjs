import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { performance } from 'node:perf_hooks'
import { fileURLToPath } from 'node:url'

// Resolve Vite's compiler through Vite instead of depending on npm hoisting.
const { build } = createRequire(import.meta.resolve('vite'))('esbuild')

const triggers = ['Right', 'Middle', 'X1', 'X2']
const regions = [null, 'Top', 'Left', 'Bottom']
const wheels = [[], ['Up'], ['Down'], ['Up', 'Down'], ['Down', 'Up']]

function fixture(count, mixed) {
  return Array.from({ length: count }, (_, i) => {
    const context = mixed ? i % 80 : 0
    return {
      id: `g-${i}`, name: `gesture ${i}`, enabled: true, deletable: true,
      trigger: triggers[context % triggers.length],
      region: regions[Math.floor(context / 4) % regions.length],
      wheel: wheels[Math.floor(context / 16)],
      left_click: false, dwell_ms: 0, fire_mode: 'OnRelease',
      action: { kind: 'none' }, times: [],
      points: Array.from({ length: 64 }, (_, p) => {
        const angle = p / 63 * Math.PI * (1.1 + (i % 7) / 50)
        return [100 * Math.cos(angle), (70 + (i % 9) * 5) * Math.sin(angle)]
      }),
    }
  })
}

function assertEquivalent(gestures, threshold) {
  const expected = findConflicts(gestures, threshold)
  const actual = cachedConflicts(gestures, threshold)
  assert.deepEqual([...actual.ids], [...expected.ids])
  assert.equal(actual.pairs.length, expected.pairs.length)
  for (const [i, pair] of actual.pairs.entries()) {
    assert.equal(pair.a, expected.pairs[i].a)
    assert.equal(pair.b, expected.pairs[i].b)
  }
}

const conflictPath = fileURLToPath(new URL('../src/conflicts.ts', import.meta.url))
const { outputFiles } = await build({
  entryPoints: [conflictPath],
  bundle: true,
  format: 'esm',
  write: false,
  plugins: [{
    name: 'expose-conflict-test-helpers',
    setup(esbuild) {
      esbuild.onLoad({ filter: /[\\/]src[\\/]conflicts\.ts$/ }, async ({ path }) => ({
        contents: (await readFile(path, 'utf8')) +
          '\nexport { sameTriggerContext, hasMouseTrigger, scoreDirect, stretchToSquare, ASPECT_DISCOUNT }',
        loader: 'ts',
      }))
    },
  }],
})
const { findConflicts, sameTriggerContext, hasMouseTrigger, scoreDirect, stretchToSquare, ASPECT_DISCOUNT } =
  await import(`data:text/javascript;base64,${Buffer.from(outputFiles[0].contents).toString('base64')}`)

// Cache stretches only within one report. Production scoring and trigger rules
// are reused, and both directions are scored because their penalties differ.
function cachedConflicts(gestures, threshold) {
  const list = gestures.filter((g) => g.enabled && g.action.kind !== 'unsupported' && hasMouseTrigger(g))
  const pairs = []
  const ids = new Set()
  const stretched = new Map()
  const getStretch = (points) => {
    if (!stretched.has(points)) stretched.set(points, stretchToSquare(points))
    return stretched.get(points)
  }
  for (let i = 0; i < list.length; i++) {
    for (let j = i + 1; j < list.length; j++) {
      const a = list[i], b = list[j]
      if (!sameTriggerContext(a, b)) continue
      let score = 0
      if (a.points.length === b.points.length) {
        if (a.points.length === 0) {
          score = 1
        } else {
          score = Math.max(scoreDirect(a.points, b.points), scoreDirect(b.points, a.points))
          const sa = getStretch(a.points), sb = getStretch(b.points)
          if (sa && sb) score = Math.max(score, Math.max(scoreDirect(sa, sb), scoreDirect(sb, sa)) * ASPECT_DISCOUNT)
        }
      }
      if (score < threshold) continue
      pairs.push({ a, b })
      ids.add(a.id)
      ids.add(b.id)
    }
  }
  return { pairs, ids }
}

function main() {
  const edge = fixture(16, false)
  edge[1].enabled = false
  edge[2].action = { kind: 'unsupported' }
  Object.assign(edge[3], { points: [], trigger: null, hotkey: ['Ctrl', 'K'] })
  Object.assign(edge[4], { points: [], region: 'Top', trigger: null, dwell_ms: 300 })
  Object.assign(edge[5], { points: [], region: 'Top', trigger: null, dwell_ms: 600 })
  Object.assign(edge[6], { points: [], region: 'Top', trigger: null, dwell_ms: 0 })
  Object.assign(edge[7], { points: [], region: 'Top', trigger: null, dwell_ms: 0 })
  edge[8].left_click = true
  edge[9].wheel = ['Up', 'Down']
  edge[10].wheel = ['Down', 'Up']
  edge[11].points = edge[11].points.slice(0, 3)
  for (const threshold of [0, 0.84, 0.97, 1]) assertEquivalent(edge, threshold)

  for (const [count, mixed] of [[8, false], [20, false], [60, false], [320, true]]) {
    const gestures = fixture(count, mixed)
    const baseline = () => findConflicts(gestures, 0.84)
    const prototype = () => cachedConflicts(gestures, 0.84)
    assertEquivalent(gestures, 0.84)
    for (let i = 0; i < 4; i++) { baseline(); prototype() }

    const median = (samples) => samples.sort((a, b) => a - b)[Math.floor(samples.length / 2)]
    const measure = (run) => {
      const start = performance.now()
      for (let i = 0; i < 3; i++) run()
      return (performance.now() - start) / 3
    }
    const before = [], after = []
    for (let i = 0; i < 7; i++) {
      if (i % 2 === 0) { before.push(measure(baseline)); after.push(measure(prototype)) }
      else { after.push(measure(prototype)); before.push(measure(baseline)) }
    }
    console.log(`${count} gestures, ${mixed ? 'mixed contexts' : 'one context'}: ` +
      `${median(before).toFixed(2)} -> ${median(after).toFixed(2)} ms/check`)
  }
  console.log('Conflict pair order, object identity and ID order matched.')
}
main()
