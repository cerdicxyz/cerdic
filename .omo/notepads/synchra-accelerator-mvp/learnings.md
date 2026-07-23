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

---

## #3 — `forge 1.4.4` inverts the `--no-commit` flag (commit is opt-in)

**Pattern:** The plan's `forge init` and `forge install` invocations both
spell `--no-commit`, but `forge 1.4.4-v1.4.4` flipped the default — commit
is now opt-in via `--commit`, and the default is no-commit. Omitting the
flag (or just using the bare subcommand) does what `--no-commit` did in
older versions.

```bash
# In 1.4.4 the default already matches the plan's intent:
forge init --no-git --empty --force packages/contracts
forge install pyth-network/pyth-sdk-solidity --shallow
```

`--no-git` on `forge init` (not `forge install`) is what suppresses the
"create a fresh git repo" behavior — important when the target directory
is already inside a parent git repo (our monorepo), otherwise forge tries
to `git init` a second one. `--empty` skips the `Counter.sol` example
contracts. `--force` lets forge scaffold into a non-empty target dir
(our `packages/contracts` already has a stub `package.json` from #1).

**Why this matters:** The plan body is correct on intent, just wrong on
flag spelling for the installed forge version. Recognizing the inversion
avoids the dead-end of "error: unexpected argument '--no-commit' found,
tip: a similar argument exists: '--commit'."

## #3 — Pyth org is `pyth-network` (lowercase), not `PythNetwork` (PascalCase)

**Pattern:** The plan's `forge install PythNetwork/pyth-sdk-solidity` 404s
because the GitHub org has been lowercase since the Pyth → pyth-network
rebrand (visible in the plan's own Context7 reference
`/pyth-network/pyth-crosschain`). The working URL is:

```bash
forge install pyth-network/pyth-sdk-solidity --shallow
```

The repo's own README still tells you to migrate to the npm package
`@pythnetwork/pyth-sdk-solidity`, which is published from
`pyth-network/pyth-crosschain` at
`target_chains/ethereum/sdk/solidity/`. For MVP wiring (compile-time
import of `IPyth` and `MockPyth`) the legacy repo is fine; for Phase 1+
switch to a remapping:

```
# remappings.txt
pyth-sdk-solidity/=lib/pyth-crosschain/target_chains/ethereum/sdk/solidity/
```

**Why this matters:** The org-name typo blocks `forge install` with a
generic 404. The repo's `IPyth.sol` also emits a `Deprecated` doc tag
that solc 0.8.24 refuses with error 6546 (see next learning).

## #3 — `pyth-network/pyth-sdk-solidity` ships a doc-tag that solc 0.8.24 rejects

**Pattern:** The deprecated Pyth repo's `IPyth.sol`, `IPythEvents.sol`,
`PythStructs.sol`, `PythErrors.sol`, `MockPyth.sol`, and `AbstractPyth.sol`
all contain a backticked package name like
`` `npm install @pythnetwork/pyth-sdk-solidity` ``. Solc 0.8.24 parses
`@<ident>` inside a NatSpec comment as a doc tag and emits:

```
Error (6546): Documentation tag @pythnetwork/pyth-sdk-solidity` not valid
for contracts.
```

**Workaround for the legacy repo:** post-install patch — the leading
`@` inside the backticks is not load-bearing for the SDK's behavior, so
it can be removed without changing semantics:

```bash
find lib/pyth-sdk-solidity -name "*.sol" -exec sed -i.bak \
  's|@pythnetwork/pyth-sdk-solidity|pyth-sdk-solidity (npm)|g' {} \;
rm -f lib/pyth-sdk-solidity/*.sol.bak
```

**Long-term fix:** migrate the import to the maintained
`pyth-network/pyth-crosschain` SDK via a remapping (see prior
learning). The post-install patch goes away.

**Why this matters:** Without the patch, `forge build` fails on every
file in the SDK and you cannot pin solc 0.8.24 with the deprecated
Pyth repo. The patch is the smallest possible diff and is contained to
the submodule's working tree — the upstream commit is unchanged.

## #3 — `smartcontractkit/chainlink` is the Go monorepo; Solidity is in `chainlink-evm`

**Pattern:** The plan's `forge install smartcontractkit/chainlink` clones
the Chainlink Go monorepo (~5k files, no Solidity). The Solidity
contracts (`@chainlink/contracts` on npm) are published from
`smartcontractkit/chainlink-evm`, which is a separate repo with the
canonical layout:

```
lib/chainlink-evm/contracts/src/v0.8/shared/
├── interfaces/AggregatorV3Interface.sol
└── mocks/MockV3Aggregator.sol
```

Forge's auto-remap on a lib with a `contracts/` subdir prepends it, so
the import path drops the `contracts/` segment:

```solidity
// forge remappings output:
// chainlink-evm/=lib/chainlink-evm/contracts/
import {AggregatorV3Interface} from
  "chainlink-evm/src/v0.8/shared/interfaces/AggregatorV3Interface.sol";
```

**Why this matters:** Installing the wrong repo silently succeeds —
`forge install` exits 0 — but the resulting `lib/chainlink/` has no
Solidity, and the consumer contract's import fails to resolve.

## #3 — `MockV3Aggregator` constructor is `(uint8 decimals, int256 initialAnswer)` — no description string

**Pattern:** The legacy `MockV3Aggregator` (and most Chainlink docs) take
a description string as the second constructor arg. The current
`chainlink-evm` repo has dropped the description and uses
`int256 _initialAnswer` instead:

```solidity
constructor(uint8 _decimals, int256 _initialAnswer)
```

`latestRoundData()` then returns `(1, _initialAnswer, 1, 1, 1)` — the
mock seeds `roundId`, `startedAt`, `updatedAt`, and `answeredInRound`
all to the deployment block's timestamp (== 1 in the forge test VM).

**Why this matters:** Tests that assert `updatedAt == 0` against the
"unset" state fail. Assert on `answer == 0` and `decimals()` instead —
those are the values a downstream `OracleHub` will actually read.

## #3 — Two-arity `submodule count` is achievable when `forge-std` is a plain lib, not a submodule

**Pattern:** The plan demands exactly 2 entries in `.gitmodules`
(Pyth + Chainlink). `forge init` clones `lib/forge-std` but does not
register it as a submodule when invoked with `--no-git` (or by default
when the parent dir is itself a git repo and the lib is gitignored
via the root `.gitignore` rule `lib/`).

Because `lib/` is already in the root `.gitignore`, anything in
`lib/forge-std/` is invisible to git and there's no need for a
submodule entry — `forge build` and `forge test` resolve `forge-std/`
through the auto-generated remapping regardless.

```bash
git submodule status  # shows nothing for forge-std
forge build           # works fine
```

**Why this matters:** If a maintainer later wants `forge-std` to be a
real submodule (so it can be pinned to a specific SHA), run
`forge install foundry-rs/forge-std --shallow --no-commit` from inside
`packages/contracts/` and the third entry appears in `.gitmodules`. The
plan's "exactly 2 entries" is satisfied today; the future migration is
one line.
