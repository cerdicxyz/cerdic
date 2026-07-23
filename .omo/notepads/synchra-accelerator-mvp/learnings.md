# Learnings — synchra-accelerator-mvp

Conventions, patterns, and successful approaches discovered during work on this plan.

_Auto-scaffolded by /start-work. Append new entries below - never overwrite._

---

## #1 — pnpm + Cargo dual-workspace monorepo pattern

**Pattern:** A single repo with both pnpm-workspace.yaml and Cargo.toml at the
root, where the Cargo workspace picks up the Rust crates and the pnpm workspace
picks up the TS packages. `packages/engine` is intentionally listed in
pnpm-workspace.yaml but has no `package.json` (it carries Cargo.toml instead) —
pnpm 10 silently skips it from `-r` scope. The fan-out scripts work cleanly:

- `pnpm -r build` reports `Scope: 3 of 4 workspace projects` (3 TS packages)
- `cargo build --workspace` (in #4) drives the Rust side
- `pnpm -r --if-present run <script>` makes the root scripts robust to packages
  that don't yet define a given script (e.g. `@synchra/frontend` stub has no
  `test` script yet but pnpm won't fail the root fan-out).

**Why this matters:** avoids the two antipatterns of (a) forking pnpm and
Cargo into separate repos (cross-cutting refactors get painful) and (b)
pretending engine is a pnpm package and running `pnpm` on top of an
all-Cargo `packages/engine` (no real benefit, adds noise).

## #1 — Pinning `.tool-versions` for asdf

**Pattern:** The `.tool-versions` file uses the `nodejs` (not `node`) tool name
because that is the asdf plugin ID for the official asdf-nodejs plugin.
Solc 0.8.24 is pinned via the asdf-solidity plugin. For CI environments that
don't use asdf, the `.tool-versions` file is informational and the actual
toolchain is set by CI config (todo #6).

**Concrete content:**
```
nodejs 20.11.0
rust 1.78.0
solc 0.8.24
```

## #1 — pnpm 10 + lockfileVersion

**Pattern:** pnpm 10 defaults to `lockfileVersion: '9.0'`. With no real
dependencies in the stub package.json files, pnpm-lock.yaml contains only the
importer shells (one entry per workspace) and no resolution block. The lockfile
grows as each subsequent todo (#2, #3, #4, #5) adds real dependencies. CI
should use `pnpm install --frozen-lockfile` to enforce lockfile respect (todo #6).

## #1 — pnpm `Scope: 3 of 4` is expected, not a bug

**Verification tip:** pnpm's "Scope: N of M workspace projects" output is the
fastest sanity check that the workspace list is parsed correctly. M should
equal 4 here (3 TS packages + the engine path listed but skipped). If pnpm
emits a warning about an unmatched glob, the workspace list is wrong.

---

## #2 — Shared package dual-export pattern: `main: dist/index.js` + `types: dist/index.d.ts`

**Pattern:** For a TS-only shared package consumed by other TS packages in
the workspace, the `package.json` "exports" pair is:

```json
"main": "dist/index.js",
"types": "dist/index.d.ts",
"type": "module"
```

with `tsconfig.json` set to `module: "ESNext"`, `moduleResolution: "Bundler"`,
`declaration: true`, `outDir: "dist"`, `rootDir: "src"`. The `"type": "module"`
makes the package ESM; sibling packages (`packages/frontend`, future
`packages/engine` TS bindings) can `import { MarketPosition } from
"@synchra/shared"` and the `.js` extensions in `src/index.ts`'s
`export * from "./types.js"` work cleanly under NodeNext / Bundler resolution
but get rewritten in the emitted `.d.ts` to `./types` (note the missing `.js`).

**Why this matters:** Without `type: "module"` and a Bundler/NodeNext
resolution, the `.js` extensions on re-exports fail with "cannot find module
./types.js" under older tsconfigs. With them, both the source-level imports
and the emitted declarations resolve correctly.

**Verification:** After `pnpm --filter @synchra/shared build`, check
`dist/index.d.ts` re-exports `./types.js` and `./constants.js` and
`dist/constants.d.ts` shows `readonly [number, number]` tuple types
(proving the `as const`-style annotation is preserved through emit).

---

## #2 — Bigint for on-chain numeric surfaces, plain `number` for bps constants

**Pattern:** Type fields that mirror Solidity `int256` / `uint256` (size,
entryPrice, margin, leverage, cumulative, etc.) as `bigint`. Keep protocol
constants that are bps percentages (`IMR_BPS = 500`, `T2_HAIRCUT_BPS = [200,
500]`) as plain `number`. The split mirrors the on-chain / off-chain
boundary: anything that may cross the contract ABI is `bigint`; anything that
stays in the risk-engine math is `number`.

**Why this matters:** Mixing causes two failure modes. (a) `bigint` in bps
arithmetic forces a `BigInt()` wrapper everywhere (`(IMR_BPS_BIG *
BigInt(10_000)) / BPS_DENOM`) and breaks `toFixed()`, percentage formatters,
and JSON serialization (`JSON.stringify` throws on bigint). (b) `number` for
uint256 loses precision above 2^53, which a single position can easily
exceed (e.g. a 100M USDC position at $60k BTC is 6e15 — already over 2^53).

---

## #2 — Monotonicity test for overlapping tier ranges uses floors, not full ranges

**Pattern:** T2=[200,500], T3=[1000,2000], T4=[1500,3500] deliberately
OVERLAP at the boundaries (T3 ceiling 2000 > T4 floor 1500). The "tiers get
riskier" invariant is therefore NOT "no overlap" — it's "the floor of each
tier is strictly greater than the floor of the prior tier." Asserting
`T1_HAIRCUT < T2_HAIRCUT_BPS[0] < T3_HAIRCUT_BPS[0] < T4_HAIRCUT_BPS[0]`
(0 < 200 < 1000 < 1500) captures the spirit of monotonicity without
overfitting to a no-overlap rule the paper doesn't require.

**Why this matters:** A naive "tier N+1 floor > tier N ceiling" assertion
fails against these constants and forces the constants to be re-shaped.
The risk engine's actual use of the ranges (a continuous function of
volatility) intentionally permits overlap so the engine can quote mid-tier
haircuts for assets mid-rebalance.

---

## #2 — Template literal type for checksum-style hex addresses

**Pattern:** `export const USYC_ARC_TESTNET: `0x${string}` = "0x...";` makes
the address type-checked as a 0x-prefixed string literal. Consumers get a
compile-time guarantee they aren't passing a raw string where an address is
expected. Combined with a runtime `expect(addr).toMatch(/^0x[0-9a-fA-F]{40}$/)`
test, the address surface is type-safe at compile time AND format-safe at
test time. The emitted `.d.ts` preserves the template literal type.

---

## #5 — create-next-app@14 in a non-empty workspace dir requires clearing the stub first

**Pattern:** `packages/frontend/` already carried a stub `package.json` from
the monorepo bootstrap (#1). `npx create-next-app@14 packages/frontend ...`
refuses to scaffold into a non-empty target. The cleanest fix is to
temporarily move (or delete) the stub out of the way, run the scaffolder,
then overwrite the generated `package.json` with the workspace-correct
version (matching name `@synchra/frontend`, the actual dep set, the script
list, and `transpilePackages` + `@synchra/shared` wiring).

```bash
mv packages/frontend/package.json /tmp/frontend-stub-package.json
npx --yes create-next-app@14 packages/frontend \
  --typescript --app --no-src-dir --import-alias "@/*" \
  --use-pnpm --eslint --no-tailwind
# then write the real package.json
```

**Why this matters:** Skipping the move step hangs the scaffolder on an
interactive "directory not empty" prompt under non-TTY shells, and
overwriting `package.json` afterwards is safer than re-deriving the entire
`pnpm install` from a partially-correct scaffolder output (which loses the
workspace name, drops App Kit, etc.).

## #5 — `transpilePackages` is the linchpin for `import from "@synchra/shared"`

**Pattern:** `@synchra/shared` (set up in #2) ships `"main": "dist/index.js"`
in its `package.json`. The frontend importing `from "@synchra/shared"` would
therefore expect a prebuilt `dist/`. Two ways to wire this: (a) require the
shared package to be built before any frontend work, or (b) add
`transpilePackages: ["@synchra/shared"]` to `next.config.mjs` so Next's
bundler compiles the workspace's TS sources on the fly. We chose (b) — it
removes a fragile ordering constraint between `pnpm --filter @synchra/shared
build` and `pnpm --filter @synchra/frontend dev`/`build`, which would break
every fresh clone and every CI cold cache.

**Why this matters:** Without `transpilePackages`, `next dev` errors with
"Module not found: Can't resolve '@synchra/shared'" on the first run because
the workspace package's `dist/` doesn't exist until the shared build script
runs. CI has no clean way to encode "always build shared first."

## #5 — `publicRuntimeConfig` vs `NEXT_PUBLIC_*`: secrets go nowhere on the client

**Pattern:** `ARC_TESTNET_RPC`, `BACKEND_API_URL`, and `USYC_ADDRESS` are
non-secret values that the browser needs (e.g. to construct a viem client
or call `axios.get(`${BACKEND_API_URL}/...`)). They are exposed via
`publicRuntimeConfig` in `next.config.mjs` and read in components with
`getConfig()`. `CIRCLE_API_KEY` and `CIRCLE_ENTITY_SECRET` are *not* in
`publicRuntimeConfig` — they would leak to every visitor. They belong to
the backend (#29), which signs transactions server-side and only returns
the walletId + balances to the client.

**Why this matters:** The plan and the owner decision pin
Developer-Controlled Wallets (server-side custody) precisely so the
frontend never has the API key. The `publicRuntimeConfig` slot is a
positive affordance — by reserving it for non-secret bridge addresses, the
attestation that no Circle key is reachable from the client is structural,
not a "be careful" rule.

## #5 — vitest 2 + @playwright/test 1.48+ both work alongside `next lint`

**Pattern:** The frontend ships two test runners with non-overlapping
concerns. `vitest run` is the unit/integration runner
(`tests/**/*.test.ts(x)`), driven by `pnpm test`. `playwright test` is the
E2E browser runner (`tests/e2e/**/*.spec.ts`), driven by `pnpm test:e2e`.
`next lint` covers ESLint/TypeScript hygiene, driven by `pnpm lint`. All
three coexist without config collisions because (a) vitest.config.ts scopes
vitest to `tests/**/*.test.{ts,tsx}` — explicitly NOT `_app/`, and (b)
playwright.config.ts scopes playwright to `tests/e2e/**`. The frontend
builds with the default Next 14 ESLint flat config plus the
`@next/next/recommended` ruleset from `eslint-config-next`; no manual
`.eslintrc` tweaks are needed for the placeholder page.

**Why this matters:** Easy to mis-step by enabling vitest in the same globs
as Next's hot-reload, which would make every `app/page.tsx` save kick off
a vitest re-run. Scoping both runners to a `tests/` subtree (alongside
`app/`) keeps the boundaries clean.
