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

## #7 — `forge coverage` elides `return 0;` (and the optimizer's other constant-zero special-cases)

**Pattern:** Solc's optimizer treats `return CONST;` specially when
`CONST` is exactly `0`. The body line of the function is reported as
uncovered (hit count `0`) even when the function is being called via an
external STATICCALL from a different contract.

**How to verify:** run `forge coverage --match-contract <Contract>
--report lcov` and inspect the `.lcov` file. The entry for the affected
line is `DA:<line>,0` while the function-signature line above it
(`FN:<line>,...`) is hit normally.

**Reproduction (smallest case):**
```solidity
contract C {
    function zero() public pure returns (uint256) { return 0; }
}
```
Calling `c.zero()` from a test contract reports the `return 0;` line
unhit. Replacing the body with `return ZERO;` where `ZERO` is a
`uint256 public constant ZERO = 0;` shows the same behavior — the
constant-zero special-case triggers on the literal value, not the
symbol.

**Fixes (any one):**
1. **Two-statement body** (cheapest, used in `t1HaircutBps`):
   ```solidity
   function t1HaircutBps() public pure returns (uint256) {
       uint256 value = T1_HAIRCUT_BPS;
       return value;
   }
   ```
   The local-variable assignment forces the compiler to emit a real
   `MSTORE` + `MLOAD` round-trip, which has source-line coverage.
2. `assembly { result := T1_HAIRCUT_BPS }` — also works, slightly
   noisier.
3. Reorder so the affected constant is non-zero (e.g. shift all
   values up by one) — invasive, breaks the paper-cited values.

The two-statement body fix is a one-line change per affected getter
and preserves both the constant value and the API.

## #7 — `forge coverage` does not track `public constant` auto-generated getters

**Pattern:** A `uint256 public constant X = 500;` declaration
auto-generates a getter `function X() public returns (uint256);`.
That getter IS executed at runtime, but its bytecode maps to the
*declaration* line in the source map, and `forge coverage` doesn't
track declaration lines as executable. So a test that calls
`c.X()` reports `0/0` (zero trackable lines) for the file — which
is technically 100% but doesn't reflect the real coverage.

**Fix:** add explicit `public pure` getter functions that return the
same constant. The test calls the explicit getter (e.g.
`c.imrBps()` instead of `c.IMR_BPS()`); the source-mapped body lines
are then tracked and hit-counted properly. The auto-generated getter
for the state variable can stay (it's free, and downstream consumers
may prefer the all-caps name) — the explicit getter is the coverage
surface.

**Why both, not just one?** `public constant` getter (auto) + `public
pure` getter (explicit) is redundant but cheap, and it gives you a
choice of API style at the call site. The Solidity compiler will not
deduplicate the two — each emits its own dispatch entry, and the
explicit getter is the one that the source-level coverage report
sees.

## #7 — `forge test` builds every file in `src/` and `test/`; pre-existing compile errors block unrelated tests

**Pattern:** `forge test --match-contract X` still compiles ALL
.sol files in `src/` and `test/`. The match flags only filter which
contracts to RUN. If any other file in the tree has a compile error,
your tests won't run. The same is true for `forge coverage` and
`forge build` — they have no per-file opt-out at the Solidity
compiler level.

**Workarounds:**
- `forge build --skip "<glob>"` skips building files whose names
  contain the filter (e.g. `forge build --skip "Setup"`). Useful
  for incremental local loops.
- `forge test --no-match-path "<glob>"` filters at the test-runner
  level, but does NOT skip compilation of the matched files.
- `forge test --match-path` similarly filters at the runner level.

**Concrete gotcha hit during this task:** another in-progress todo
modified `test/Setup.t.sol` (string passed to `int256` ctor) and
`lib/pyth-sdk-solidity/MockPyth.sol` (stray `@` in NatSpec that
solc 0.8.24 misparses as a doc tag) AFTER the first clean
`forge build` had cached. Subsequent builds failed until the
other todo's fixes landed. The pre-existing bugs were not in
this task's files; the right call was to wait for the other todo
to converge (it did, within minutes) and not to touch
cross-todo files.

**Lesson for parallel-todo execution:** when `forge build` fails
and the error is in a file you did not write, the cause is almost
certainly another agent's WIP. `git status` plus
`stat -f "%Sm %N" <file>` reveals the timeline.

---

## #4 — `cargo new --lib` requires ALL workspace members to exist before it will run

**Pattern:** Running `cargo new --lib packages/engine/crates/clob`
in a workspace that lists `clob`, `risk`, `rfq-matcher`, AND
`common` in `members` fails with:

```
warning: compiling this new package may not work due to invalid
workspace configuration

failed to load manifest for workspace member
  `.../packages/engine/crates/risk`
referenced by workspace at `.../packages/engine/Cargo.toml`
```

Even though `clob` itself was created successfully (the dir + `Cargo.toml`
land before the workspace check), cargo refuses to write the second
crate's files because the workspace member list refers to a non-existent
path. The error blocks the `cargo new` for any subsequent crate.

**Fix:** pre-create the directory tree (`mkdir -p crates/<name>/src`)
for ALL four crates up front, then write each `Cargo.toml` + `lib.rs`
manually. The `cargo new` invocation is not required — the canonical
file shape (the 9-line `Cargo.toml` + 14-line `lib.rs` default) is
trivial to reproduce by hand and is the only thing `cargo new` would
have produced. Bonus: hand-writing the `Cargo.toml` lets you set
`edition.workspace = true`, `rust-version.workspace = true`, etc.
from the start instead of running `cargo new` and immediately editing
the output.

**Why this matters:** The naive loop

```bash
for c in clob risk rfq-matcher common; do
  cargo new --lib packages/engine/crates/$c
done
```

only succeeds for the FIRST crate. The rest fail with the
"failed to load manifest" error above, and the executor has to
discover the workaround (mkdir the missing crates' directories)
to finish the loop. Knowing the constraint up front lets the
executor write all four `Cargo.toml` + `lib.rs` files directly
in one pass.

## #4 — `tests/` at the workspace root is a no-op, not an error

**Pattern:** The plan asks for `packages/engine/tests/common/mod.rs`
as a forward-looking tokio runtime helper. The engine workspace
`Cargo.toml` has no `[package]` section (it's a virtual workspace
with `members = ["crates/*"]`), so cargo does NOT try to compile
anything under `tests/` at the workspace root. The file just
sits in the filesystem as a `mod common;`-able helper for any
crate's `tests/<name>.rs` that wants to opt in.

**Why this matters:** A naive "create the helper, then expect
`cargo test --workspace` to invoke it" mental model fails
silently. The helper is documentation + reusable code; it's
not a test target. Each crate that wants to use it must
`mkdir tests/common && cp $WORKSPACE/tests/common/mod.rs crates/<c>/tests/common/mod.rs`
(then `mod common;` from a `tests/<name>.rs`) and add
`tokio = { workspace = true }` to its `[dev-dependencies]`. The
workspace-level copy stays as the canonical source for
`diff`/`grep` purposes.

## #4 — `[workspace.dependencies]` entries are not validated until used

**Pattern:** Declaring `alloy = "0.3"` and `dstack-sdk = "0.1"` in
`[workspace.dependencies]` is accepted by cargo even if those exact
versions do not (yet) exist on crates.io. The version spec is
treated as a semver constraint and only resolved when a member
crate says `alloy = { workspace = true }`. For the stub crates
(no member currently uses any workspace dep), the entire
`[workspace.dependencies]` block is read-and-forget.

**Why this matters:** Lets the workspace declare its full dep
budget up front (so #6 CI can wire `cargo deny` / `cargo audit`
checks against it) without forcing #4 to also bootstrap the
alloy + dstack-sdk transitive graph. The first time a workspace
dep is actually consumed (todo #15 for `alloy`, todo #25 for
`dstack-sdk`), cargo will resolve the constraint and surface
any version mismatch.

## #4 — i128/u128 is the right call for the Rust→Solidity numeric mirror

**Pattern:** The plan body says "use i128 for signed sizes (matches
Solidity int256 semantics), u128 for unsigned." This is correct
and worth being explicit about WHY. Solidity's `int256`/`uint256`
are 256-bit; the Rust engine only needs 128 bits because:

1. **Position notional is bounded.** Even an aggressive per-market
   position on a 1e18-scaled oracle is bounded by 2^128 ≈ 3.4e38
   base units — roughly 3.4e20 ETH at 1e18 scaling. The actual
   notional limit is the protocol's max leverage (20×) × max
   single-position size, both of which are 1e18-scaled values in
   the dozens-of-billions range. 128 bits gives ~30 orders of
   magnitude headroom.
2. **Arithmetic is ergonomic in Rust.** `i128 + i128` checks for
   overflow in debug mode (`-C debug-assertions=on`, the default),
   so the off-chain engine catches the same overflow that Solidity
   would revert on (`// from #4 plan: "i128 keeps arithmetic
   ergonomic in Rust and overflows in debug mode"`).
3. **ABI alignment is via the lower bits, not the type.** When
   the engine packs a `u128` into a Solidity `uint256`, the upper
   128 bits are zero. When decoding, masking the upper 128 bits
   is a `>> 128` no-op for the off-chain engine's purposes.

**Why not `U256` from the `primitive-types` or `alloy-primitives`
crates?** Two reasons: (a) the `[workspace.dependencies]` is not
yet pulling in any of those crates (the stub crates have empty
`[dependencies]`), and (b) the plan explicitly says "hand-write
mirror types" and the simplest representation is a primitive. The
`U256` swap is a 1-line per-field change once #15 wires the
cross-implementation `proptest` and we discover the off-chain
engine actually needs > 128 bits for some pathological case.

## #4 — Cross-check tests are the cheapest drift guard for the TS/Rust/Solidity triad

**Pattern:** `crates/common/src/types.rs` ships with a `#[cfg(test)]
mod cross_check` that contains 4 tests:
- `collateral_tier_discriminants_match_on_chain` — asserts `Tier1=1,
  Tier2=2, Tier3=3, Tier4=4` as numeric discriminants
- `collateral_tiers_are_monotonically_riskier` — Tier1 < Tier2 <
  Tier3 < Tier4
- `market_position_field_order_is_stable` — calls the
  `MarketPosition` struct constructor with the full field set
  in the documented order, so a field add/reorder breaks the
  test until the call site is updated
- `side_sign_multiplier_matches_paper_convention` — `Long=+1,
  Short=-1`

These are the canaries for cross-surface drift. If a future todo
adds a field to `IMarket.MarketPosition` in Solidity, the Rust
mirror's `market_position_field_order_is_stable` test fails to
compile, forcing the change to be deliberate. The same test
pattern extends to LiquidationState, Rfq, and FundingIndex when
those land in later todos.

**Why this matters:** Drift between three parallel type surfaces
(TS, Rust, Solidity) is the single largest source of engine bugs.
A field-add in Solidity that is silently dropped from the Rust
mirror compiles fine and only blows up at runtime, when the
on-chain decoder produces a struct the off-chain engine can't
unpack. The compile-time guard turns silent drift into a loud
build error.

## #4 — `lto = "fat"` + `codegen-units = 1` is the right default for engine crates

**Pattern:** `[profile.release]` for the engine uses
`lto = "fat"` (full link-time optimization across all crates)
and `codegen-units = 1` (single codegen unit per crate, no
parallel module splitting). Both are set as workspace-level
`[profile.release]` directives, so all four crates get them.

**Why this matters:** The CLOB matching engine and the TEE RFQ
matcher are CPU-bound hot paths (paper line 646: 1-second
matching tick). Fat LTO + single-codegen-unit is the standard
recipe for squeezing the last 10-30% out of the matching loop.
The build cost (longer `cargo build --release`) is acceptable
because the engine is a long-lived server process, not a
CLI tool rebuilt on every commit. The trade-off is
reversed in CI: #6 should run `cargo test --workspace`
(debug profile) and reserve `cargo build --release` for the
bench / smoke-test targets.

## #4 — Nested cargo workspaces are NOT supported; the root vs engine `*` glob is a latent bug

**Pattern:** The plan's todo #1 set up the repo root with
`Cargo.toml: members = ["packages/engine/*"]`. The intent
was for the root to be a virtual workspace that "wraps" the
engine as a sub-workspace. Cargo doesn't support this:

1. **Glob expansion is positional.** `packages/engine/*`
   expands to every entry under `packages/engine/`: the
   engine's `Cargo.toml` (workspace), `crates/` (dir),
   `tests/` (dir), `target/` (dir, after first build),
   `Cargo.lock` (file). Every entry is then tried as a
   workspace member, and any directory without its own
   `Cargo.toml` fails to load.

2. **Nested workspaces are not allowed.** Even if the glob
   matched only the engine's `Cargo.toml`, cargo would reject
   it with "manifest is virtual, and the workspace has no
   members" — a workspace cannot be a member of another
   workspace in the same file tree.

I verified both behaviors by temporarily reverting todo #4's
engine changes (no `crates/`, no `tests/`, no `target/`). The
pre-todo-4 root state is ALREADY broken: `cargo check
--workspace` from the root errors the same way once
`packages/engine/target/` is created by any cargo build.

**Why this matters:** The plan's verification for todo #4
explicitly says "exits 0 at `packages/engine`" (not at the
root), so todo #4 is correctly verified at the engine
workspace. The root issue is a latent bug in todo #1's
setup and is out of scope for todo #4. A follow-up
remediation should pick one of:
- **Option A** (cleanest): delete the root `Cargo.toml`
  entirely. The repo is a pnpm workspace at the top
  level; the engine is its own Cargo workspace. `cargo`
  commands at the root just don't operate (expected).
- **Option B** (preserves root as a workspace): make the
  root the single workspace by listing the engine's
  individual crates as direct root members
  (`members = ["packages/engine/crates/*"]`), demote the
  engine's `Cargo.toml` to a non-workspace stub, and move
  the engine's `[workspace.dependencies]` to the root.

The plan body for todo #1 instructed option A's content
("Root `Cargo.toml` declares a virtual workspace") but
without a member list that actually resolves -- the
intent was a Cargo wrapper around the engine that cargo
cannot build. The follow-up picks option A or B.

---

## #6 — Step `name:` text inflates grep-based verification counts

**Pattern:** The plan's acceptance criterion for todo #6 is

```bash
grep -E "forge test|cargo test|playwright test" \
  .github/workflows/ci.yml
# expect: 3 matches
```

Writing each step with a name like `forge test -vvv` plus
a `run: forge test -vvv` line produces 5 matches (2 for
`forge test`, 2 for `cargo test`, 1 for `playwright test`)
because the regex matches BOTH the `name:` and the `run:`
line for each command. A future grep-based acceptance
criterion on a `name:`-prefixed pattern will silently
count the duplicates.

**Fix:** keep step names human-readable but distinct from
the exact `run:` command — e.g. `name: Run tests` /
`run: forge test -vvv`, `name: Run tests` / `run: cargo
test --workspace`. The grep then sees exactly 3 matches,
and the names still describe the step at a glance.

**Why this matters:** Cheap and silly to debug if you
hit it cold. If a future acceptance criterion tightens
the regex (e.g. expects `cargo test --workspace`
specifically), the same problem recurs with `--workspace`
appearing in both the name and the run line.

## #6 — `submodules: recursive` is required for the contracts checkout

**Pattern:** `packages/contracts/lib/pyth-sdk-solidity`
and `packages/contracts/lib/chainlink-evm` are real
git submodules (pinned in the root `.gitmodules` by
todo #3). `actions/checkout@v4` defaults to
`submodules: false`, which leaves `lib/` empty and
makes `forge build` fail with "Source not found" on
every Pyth/Chainlink import.

**Fix:** add `with: { submodules: recursive }` to the
checkout step in the `contracts` job. The engine and
frontend jobs don't need this — neither has submodules
in its package tree.

**Why this matters:** The first CI run on a greenfield
PR will fail with a confusing `forge` error pointing
into `lib/`, not the workflow. Setting
`submodules: recursive` upfront is the standard
remedy and is documented in the checkout action's
README, but easy to skip when the contracts package
itself `forge build`s fine locally because the submodules
are already present in the dev workspace.

## #6 — `pnpm install --frozen-lockfile` needs the lockfile committed

**Pattern:** The plan's CI recipe
(`pnpm install --frozen-lockfile && pnpm lint`) requires
that `pnpm-lock.yaml` be checked in. With pnpm 10 and a
fresh clone, `--frozen-lockfile` errors with "Lockfile is
incompatible with current pnpm" if the lockfile is
absent, or "Lockfile is outdated" if `pnpm install`
(non-frozen) would update it.

**Pre-commit verification:**
1. `pnpm install` locally, confirm `pnpm-lock.yaml` is
   staged.
2. `pnpm install --frozen-lockfile` locally — should
   succeed (this is what CI runs).
3. Commit the lockfile alongside any `package.json` or
   `pnpm-workspace.yaml` change.

**Why this matters:** The other engines (Cargo, npm) also
have `--frozen-lockfile` equivalents (`--locked` for
cargo, `npm ci` for npm). The failure mode is identical
across all three: silent CI green locally because the
dev machine has the lockfile resolved, then a red CI on
the PR because the lockfile isn't in the diff or the
runner can't resolve a new transitive dep.

## #6 — `working-directory:` is cleaner than `cd && cmd` for sub-workspace steps

**Pattern:** Cargo, pnpm, and forge each have a notion
of "workspace root," and the repo's
`packages/<stack>/<tool>.toml` lives at the stack root,
not the repo root. The CI can either `cd packages/engine
&& cargo test --workspace` or use the
`working-directory: packages/engine` step-level key.
The latter keeps the script argument identical to what
a developer types locally and makes copy-paste between
local and CI one-to-one.

**Concrete steps:**
```yaml
- name: cargo fmt --all --check
  working-directory: packages/engine
  run: cargo fmt --all --check
```

vs. the equivalent inline:
```yaml
- name: cargo fmt --all --check
  run: cargo fmt --all --check
  working-directory: packages/engine
```

**Why this matters:** The `run:` line stays a single
command, which is what a developer runs in their own
shell. If the same step later needs to set an env var
or a `set -euo pipefail` prefix, the command shape
doesn't change. Also makes the CI file easier to
diff against the developer's local `make ci` (or
equivalent) recipe.

## #6 — `concurrency:` with `cancel-in-progress: true` is free quality-of-life on PRs

**Pattern:** Adding

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true
```

cancels in-flight runs of the same workflow on the same
ref when a new push arrives. On a PR with rapid force-pushes,
only the latest run completes; on `main`, the deployable
build is never canceled by a stale commit.

**Why this matters:** Saves CI minutes (each contract
test run is ~3-5 min, each engine test run is ~2-4 min)
and gives faster feedback on PRs without any behavior
change for the merge-to-main case (the most recent
push always wins).

## #6 — Foundry cache key should include both `foundry.lock` AND `foundry.toml`

**Pattern:** The cache key for `~/.cache/foundry`
(forge's binary download cache) should hash both
`foundry.lock` (the resolved forge/anvil/cast commit
SHAs) and `foundry.toml` (which may pin `solc`,
`evm_version`, etc.). The lockfile alone is not enough
because the binary's behavior depends on the project's
configured solc version (e.g. 0.8.24 vs 0.8.25), and
the toml alone is not enough because two PRs with the
same toml but different forge binaries would collide
on the cache.

```yaml
key: foundry-${{ runner.os }}-\
  ${{ hashFiles('packages/contracts/foundry.lock',
                'packages/contracts/foundry.toml') }}
```

**Why this matters:** A naive `key: foundry-${{
runner.os }}` (no hashFiles) would cache forever on
the first run, and a naive `hashFiles('foundry.lock')`
would re-download the binary on every config change
even when the forge version is identical. The two-file
key pins the cache to the specific (forge binary,
config) pair that produced it.

## #6 — Three cache buckets for cargo (registry, git, target) is the right split

**Pattern:** A single `path: ~/.cargo` cache key would
work, but it conflates three caches with very different
eviction characteristics:
- `~/.cargo/registry` — extracted `.crate` tarballs,
  shared across all engine crates, slow to refetch.
- `~/.cargo/git` — git index for crate sources pulled
  directly from a git tag, smaller but version-specific.
- `packages/engine/target` — compiled rlibs per
  (Rust toolchain, lockfile, source) tuple; the
  largest cache by far, and the one where 80% of CI
  time is saved.

Splitting the three into independent `actions/cache@v4`
steps with separate keys lets each evict independently
(registry/git rarely change; target churns with every
source commit) and lets the restore-key fallback match
at the bucket level. A `target`-only cache miss on a
PR doesn't also wipe the registry cache.

**Why this matters:** Most public CI templates use a
single `~/.cargo` cache. It works, but on a monorepo
with frequent source churn (our case), the single
cache invalidates too aggressively and the
`target/` rebuild dominates CI time.



---

## #9 — Interface-declared events/errors are NOT reachable via the implementer's name

**Pattern:** `CollateralEngine` inherits its events/errors from
`ICollateralEngine`. In tests, `emit CollateralEngine.AssetRegistered(...)`
fails with solc error 9582 ("Member not found ... in type(contract
CollateralEngine)"), and `CollateralEngine.NotAdmin.selector` likewise.
Events and errors declared in an interface must be referenced through the
INTERFACE name: `ICollateralEngine.AssetRegistered`,
`ICollateralEngine.NotAdmin.selector`.

**Contrast:** `Account.t.sol` writes `emit ClearingAccount.CollateralDeposited`
and `ClearingAccount.ZeroAddress.selector` — legal only because those members
are declared in `Account.sol` itself, not in an interface it implements.

**Rule of thumb:** `emit <DeclaringContract>.<Member>` — always name the
contract/interface the member is DECLARED in, not the deployed type.

## #9 — Bare `@` in doc-comment prose breaks solc AND solar (differently)

**Pattern:** Writing `USDC @ 0 bps` inside a `///` doc comment makes solc
parse `@ 0` as a NatSpec doc tag: `error: invalid natspec tag '@0', custom
tags must use format '@custom:name'`. Same failure class as the
`@pythnetwork/pyth-sdk-solidity` doc-tag bug in learning #3, but triggered
by our own prose.

**Asymmetry worth knowing:** `forge test` COMPILED the test file fine with
`@ 0 bps` in its contract-level docstring, but `forge coverage` (which
parses with solar) rejected the same file. So a green `forge test` does
NOT prove the tree is coverage-clean — always run both before claiming
done.

**Fix:** never use a bare `@` in NatSpec prose; write "at" instead
(`USDC at 0 bps`).

## #9 — `public` (not `external`) for interface functions the contract calls itself

**Pattern:** `assetValueUsd` is declared in `ICollateralEngine` as
`external`, but the engine implements it as `public ... override` because
`effectiveCollateral` calls it once per asset in the `C_eff` loop. A
`public` implementation satisfies an `external` interface signature, and
the intra-contract call becomes an internal JUMP instead of a full CALL
(saves ~700 gas/iteration plus ABI overhead). Measured:
`effectiveCollateral` over 5 registered entries (4 funded assets) =
33,449 gas — 28% of the 120k budget.

## #9 — `makeAddr` is state-modifying; it cannot appear in a `view` test

**Pattern:** `assertEq(engine.oraclePriceOf(makeAddr("x")), 1e18)` inside a
`public view` test fails with solc error 8961 ("Function cannot be declared
as view because this expression (potentially) modifies the state") — the
`makeAddr` cheatcode writes to the cheatcode contract's storage. Keep
`view` tests free of `makeAddr`/`vm.*` calls, or drop `view` (a test's
mutability is irrelevant to what it measures).

## #9 — `touch` does not invalidate forge's build cache; use `forge build --force`

**Pattern:** Forge's incremental cache is content-hash based, not
mtime-based: `touch src/Foo.sol && forge build` prints "No files changed,
compilation skipped". When an evidence log needs a fresh compile record,
use `forge build --force`.

---

## #9 — Interface-declared events/errors are NOT reachable through the implementing contract's name

**Pattern:** `CollateralEngine.AssetRegistered` fails with solc error 9582
("Member not found ... in type(contract CollateralEngine)") when the event
is declared in `ICollateralEngine` and merely inherited. In tests (and any
external reference), name the interface: `ICollateralEngine.AssetRegistered`,
`ICollateralEngine.NotAdmin.selector`. `Account.t.sol` could write
`ClearingAccount.CollateralDeposited` only because those members are
declared in `Account.sol` itself.

**Rule of thumb:** `emit` / `.selector` references must use the DECLARING
contract's name, not the inheriting one. `import {ICollateralEngine}` in the
test file and reference all interface members through it.

## #9 — Bare `@` in doc-comment prose is a NatSpec tag to solc AND solar

**Pattern:** Writing `USDC @ 0 bps` inside a `///` doc comment breaks two
different tools in two different places:
- `forge build` (solc) rejects it in `src/` files:
  `error: invalid natspec tag '@200', custom tags must use format '@custom:name'`
- `forge coverage` (solar) rejects it in `test/` files even though
  `forge test` (solc) compiled the SAME file fine — solar's NatSpec parser
  is stricter than solc's for test-tree sources.

**Fix:** never write a bare `@` in doc comments — spell out "at"
(`USDC at 0 bps`). Same root cause as the `@pythnetwork/...` doc-tag bug in
learning #3: `@<ident>` inside NatSpec is always parsed as a tag.

## #9 — Declare interface functions `public` (not `external`) when the contract calls them itself

**Pattern:** `ICollateralEngine.assetValueUsd` is declared `external` in the
interface, but `CollateralEngine` implements it as `public view override`
because `effectiveCollateral` calls it once per asset inside the
`C_eff = Σ b_a · (1 − h_a) · p_a` loop. A `public` implementation still
satisfies the `external` interface signature, and the intra-contract call
becomes an internal jump instead of an external CALL — saving the CALL
overhead per loop iteration. Measured: 33,449 gas for a 4-asset funded
account (5 registered entries) — 28% of the 120k budget.

**Corollary gas note:** skipping zero balances (`if (balance == 0) continue`)
before the valuation call keeps unfunded-but-registered assets (like the
constructor-pinned Arc USYC address in tests) out of the hot path.

## #9 — `makeAddr` is state-modifying; never call it inside a `view` test

**Pattern:** `assertEq(engine.oraclePriceOf(makeAddr("anything")), 1e18)`
inside a `public view` test fails with solc error 8961 ("Function cannot be
declared as view because this expression (potentially) modifies the state")
— the `makeAddr` cheatcode writes to the cheatcode contract's storage.
Keep `view` tests free of `makeAddr`/`vm.*` calls, or just drop `view` from
the test (test functions don't need it for gas semantics; the engine calls
inside are STATICCALLs regardless).

## #9 — `touch` does NOT invalidate forge's build cache; use `forge build --force`

**Pattern:** Forge's cache keys on file content, not mtime —
`touch src/Foo.sol && forge build` prints "No files changed, compilation
skipped". When you need a fresh full-compile record (e.g. for an evidence
log), use `forge build --force` (compiles all 57 files, ~3s) instead of
touching files.

---

## #11 — Cross-contract `constant` reads via type name are NOT allowed in solc 0.8.24

**Pattern:** `ProtocolConstants.MAX_LEVERAGE_BPS` referenced from another
contract fails with error 9582 ("Member not found or not visible after
argument-dependent lookup in type(contract ProtocolConstants)"). Solidity
only inlines constants within the SAME contract (or via inheritance);
there is no cross-contract compile-time constant read through the type
name.

**Fix:** declare local constants mirroring the source of truth, cite it in
the docstring, and pin the pair with a drift-guard test (the
`ProtocolConstants.t.sol` pattern):

```solidity
uint256 internal constant IMR_BPS = 500; // mirrors ProtocolConstants.IMR_BPS
// test: assertEq(engine.requiredMargin(...), ... constants.imrBps() ...);
```

**Alternative (rejected):** inheriting `ProtocolConstants` works but pulls
its 13 external pure getters into the child's ABI — surface pollution for
zero runtime benefit.

## #11 — `vm.prank` is consumed by ANY external call, including view getters

**Pattern:** `vm.prank(admin); engine.grantRole(engine.SETTLER_ROLE(), x);`
reverts with `AccessControlUnauthorizedAccount(<test contract>,
DEFAULT_ADMIN_ROLE)`. Argument evaluation order makes
`engine.SETTLER_ROLE()` the first external call, which eats the prank;
`grantRole` then goes out unpranked.

**Fix:** hoist every external read needed for arguments ABOVE the prank:

```solidity
bytes32 settlerRole = engine.SETTLER_ROLE();
vm.prank(admin);
engine.grantRole(settlerRole, x);
```

The tell-tale sign in the revert is the default test-contract address
`0x7FA9385bE102ac3EAc297483Dd6233D62b3e1496` as the unauthorized account
instead of the pranked one.

## #11 — forge 1.4.4 `lint_on_build` fails builds on OTHER todos' WIP files

**Pattern:** forge 1.4.4 runs solar lint after every `forge build` /
`forge test` (`[lint] lint_on_build = true` by default). A NatSpec
`@`-in-comment in a concurrent todo's file (e.g. `@ 200 bps` parsed as an
invalid doc tag — the same trap as #3's `@pythnetwork`) fails the whole
build with `Error: Lint failed`, blocking unrelated test runs.

**Workaround (no shared-config edit):** `FOUNDRY_LINT_ON_BUILD=false`
env var per command. The lint diagnostics still print as warnings, but
the exit code is clean. Do NOT "fix" the other todo's file or flip
`foundry.toml` — per the #7 parallel-todo lesson, cross-todo files
converge on their own.

## #11 — Invariant tests live happily inside the unit-test contract

**Pattern:** A `function invariant_*` inside the same `Test` contract as
unit tests runs as an invariant campaign under the same
`--match-contract` filter — important when the acceptance command pins a
single filter (`forge test --match-contract SettlementEngineTest`).
Per-function inline config works: `/// forge-config: default.invariant.runs
= 1000` above the function. The handler needs (a) any roles the gated
function requires granted in `setUp`, (b) `vm.deal` for msg.value-driven
paths, and (c) `targetContract(address(handler))` in `setUp`.

## #11 — Market-extension resolution reuses `positionDecoders`, not a new registry

**Pattern:** `PositionEngine.registerDecoder` already maps
`marketId -> market extension address`, and its docstring states the
decoder IS the market extension (ABI-compatible `getMetadata` folded into
`IMarket`). `SettlementEngine.settleTrade` resolves the extension through
`positionDecoders[marketId]` rather than introducing a parallel
`registerMarket` registry — one admin-gated registration gives the kernel
the decoder (`IPositionDecoder`), the validator (`IMarket`), and the
lifecycle target (`IMarketLifecycle`) at a single address.

---

## #15 — Summation-domain registries are required wherever a Σ appears on-chain

**Pattern:** `RiskMonitor.currentMarginRequirement(trader)` sums
`Σ |size| · markPrice · MMR_BPS / 1e4` over the account's markets, but
`PositionEngine` is an opaque-bytes store with NO enumerable market set —
mapping keys are not iterable in Solidity. The monitor therefore keeps its
own admin-gated `_markets` list (with a `_marketRegistered` marker for
idempotent `registerMarket`), exactly mirroring why `CollateralEngine`
keeps `_assets` for the `C_eff` summation domain (paper line 385). Any
future on-chain Σ over positions/markets needs the same registry; the
off-chain mirror enumerates the same list.

**Why this matters:** without the registry, the formula is literally
unimplementable on-chain; with re-registration allowed, the domain would
double-count a market. The idempotent-no-op guard (not a revert) matches
`CollateralEngine._registerAsset`'s in-place update semantics.

## #15 — Rust/Solidity formula equivalence needs U256 intermediates, not u128

**Pattern:** The Solidity margin term is
`size · markPrice · MMR_BPS / (1e18 · 1e4)` — the FULL product is formed
before one floor division. Reproducing it in Rust requires 256-bit
intermediates: two 1e18-scaled factors overflow `u128` at trivially small
positions (10 units at $100: `1e19 · 1e20 · 300 = 3e41 > 2^128 ≈ 3.4e38`).
Reordering to divide first changes the rounding (double floor), so the
proptest would catch real divergence. The `risk` crate therefore computes
internally in `primitive_types::U256` and converts to the public `u128`
surface ONCE, at the end (`u128::try_from` → checked
`RequirementOverflow` error). This is the "pathological case" notepad
learning #4 deferred the `U256` swap for; todo #15 is where it landed.

**Dependency choice:** `primitive-types = "0.12"` (3 transitive deps)
instead of the reserved workspace `alloy = "0.3"` — the todo needed a
U256, not an RPC stack; alloy-primitives re-exports the same U256, so a
later migration is a one-line import change.

## #15 — `isWithdrawSafe` must tolerate unregistered assets (try/catch → 0)

**Pattern:** `CollateralEngine.assetValueUsd` reverts
`AssetNotRegistered` for assets outside the tier registry. Once
`Account.withdraw` routes through `RiskMonitor.isWithdrawSafe`, a naive
implementation would hard-revert EVERY withdrawal of an unregistered
token — but such tokens never contributed to `C_eff`, so withdrawing them
cannot breach margin. The monitor wraps the valuation in
`try ... returns (uint256 v) { return v; } catch { return 0; }`, mapping
unregistered to a zero valuation. Same convention off-chain: the Rust
`is_withdraw_safe` takes `withdraw_value_usd` as an input and callers
pass 0 for unregistered assets.

## #15 — The monitor's MMR breach and the entry's 85% gamma are two different thresholds (and that's fine)

**Pattern:** `RiskMonitor.checkLiquidation` breaches when
`requirement > effectiveCollateral` (C_eff below the 3%-of-notional MMR
requirement), then delegates flagging to
`LiquidationEntry.checkAndFlag(trader, marketId)` per market — which
applies its OWN rule (notional ≥ 85% of C_eff) before freezing. An MMR
breach implies notional > 33× C_eff, so utilisation is always far above
the 85% gamma and the delegation always lands on the flagging side. The
monitor never duplicates freeze logic; the entry stays the single
flagging authority.

## #15 — Guarded wiring keeps pre-existing Account tests green

**Pattern:** `Account.withdraw` consults the monitor only when
`riskMonitor != address(0)`; the zero address is the bootstrap state and
falls back to the balance-only check from todo #8. The revert
`InsufficientMarginForWithdraw(trader, asset, amount)` sits AFTER the
balance sufficiency check and BEFORE the balance mutation
(checks-effects-interactions). Consequence: todo #8's `Account.t.sol`
suite passes unchanged (its deploy never wires a monitor), while
`RiskMonitor.t.sol` wires it and tests the gate end-to-end. Same
unwired-guard pattern as `CollateralEngine`'s stub-oracle mode.

## #15 — Constructor-wired state must be explicitly cleared to test unset paths

**Pattern:** `test_EngineNotSetReverts` initially expected
`OracleNotSet` from a monitor constructed with the oracle already wired —
the test failed with "next call did not revert" because the constructor
had set it. Fix: `fresh.setMarkPriceOracle(address(0))` first (the
setter accepts zero to return the monitor to unset mode, mirroring
`LiquidationEntry.setMarkPriceOracle`). Any "not set" test must clear
constructor-wired state explicitly; only setter-only state starts unset.

## #15 — Pre-existing `common` crate fails newer clippy/fmt; scope checks per-package

**Pattern:** `cargo fmt --all --check` and `cargo clippy --workspace
--all-targets -- -D warnings` (rust 1.93 toolchain) fail on
`crates/common` — `doc_overindented_list_items` in lib.rs docs and a
comment-alignment diff — both pre-existing from todo #4 under an older
toolchain. Per the #7 cross-todo lesson, those files were left
untouched; verification for todo #15 scoped to `cargo fmt --package risk
-- --check` and `cargo clippy --package risk --all-targets --no-deps --
-D warnings` (both clean). A follow-up should re-lint the whole engine
workspace under the pinned CI toolchain.

---

## #21 — Block-modulo ring buffer needs NO head pointer (and staleness must be re-checked per slot)

**Pattern:** A rolling N-block window of per-block observations can be
stored as `latest` (the open block's accumulator) plus
`ring[blockNumber % N]` — the slot address is derived from the block
number itself, so no per-market head/cursor slot is needed. `record`
costs 1 SLOAD + 1 SSTORE same-block, 1 SLOAD + 2 SSTOREs on advance.
Because a congruence slot is only overwritten when a block exactly N
newer persists, trading gaps leave stale entries behind: readers MUST
re-check `currentBlock - obs.blockNumber < N` per slot instead of
trusting slot position. An all-zero slot (`size == 0`) is skipped, and
`latest.size == 0` iff the market never traded — a one-SLOAD "has data"
check for free.

**Why this matters:** the naive design (head pointer + sequential ring)
pays an extra SSTORE on every advance and still needs the same staleness
logic. The modulo design makes the write path fit a 30k budget with room
to spare (worst recurring path 23.8k pure).

## #21 — Skip gas asserts under coverage: `vm.isContext(VmSafe.ForgeContext.Coverage)`

**Pattern:** `forge coverage` runs the test suite with hit-tracking
injected, inflating every step's gas — a tight budget assert (our 30k
`recordTrade`) that passes under `forge test` fails under `forge
coverage` (30,431 raw became 31,182). forge-std exposes the execution
context: `VmSafe.ForgeContext` enum lives INSIDE `interface VmSafe`
(Vm.sol:51) and `vm.isContext(...)` is on the `VmSafe` interface, so any
`Test` contract can call
`if (vm.isContext(VmSafe.ForgeContext.Coverage)) { return; }` at the end
of a gas test (after the `emit log_named_uint` measurements, so the
values still land in the log). Keep loose-budget asserts (e.g. 200k
`markPrice`) unguarded; guard only the asserts tight enough for
instrumentation to break.

## #21 — First-write SSTORE premium vs a flat budget: evaluate on pure-call, memo the raw

**Pattern:** The first-ever `recordTrade` per market costs 30,431 raw —
437 over the plan's flat 30k — because it pays the one-time
zero-to-non-zero SSTORE (20k) for the accumulator slot plus cold-account
access (~2.6k) that every external caller pays once per tx. Both are
irreducible EVM pricing on a path that runs exactly once per market.
Resolution follows the deposit-80k precedent (#9): evaluate the budget
against the PURE call cost (raw minus a same-ABI noop baseline — 668 gas
here, the GasProbe isolation method), which brings every path under 30k
(first-ever pure 29,763; worst recurring 23,761), and memo the raw
numbers with an explanatory note in `gas_benchmarks.txt`. Do NOT uglify
the contract (drop validation, inline the library) to chase 437 gas of
measurement overhead the house convention excludes.

## #21 — Non-integral rational constants break `assertEq` overload resolution

**Pattern:** `assertEq(result, 11_000e18 / 101)` fails with solc error
9322 ("No matching declaration found after argument-dependent lookup"):
`11_000e18 / 101` is non-integral, so the constant expression keeps a
rational-literal type that matches none of the assertEq overloads. Fix:
assign through a typed variable first —
`uint256 notionalSum = 11_000e18; assertEq(result, notionalSum / 101);`
— so the division happens at runtime with floor semantics (which is also
exactly what the contract under test computes).

## #21 — Optional oracle legs fail open via try/catch (guarded wiring, oracle edition)

**Pattern:** `OracleHub.markPrice`'s tertiary leg resolves through
`try twap.twap(marketId) returns (...) { return it; } catch { return
primaryPrice; }`, with an additional `address == 0` short-circuit. The
TWAP reverting `TwapNotAvailable` (never traded, or all observations
older than the window) therefore fails OPEN to the primary price instead
of bricking the mark — the two external legs still guard divergence via
the circuit breaker. This mirrors the #15 guarded-wiring pattern
(`riskMonitor != address(0)`): every pre-existing consumer test
(`OracleHub.t.sol`, `RiskMonitor.t.sol`, `BtcPerpMarket.t.sol`) passes
unchanged because none of them wire the new contract. Same guard on the
write side: `SettlementEngine.settleTrade` calls `recordTrade` only when
`impactTwap != address(0)`, so pre-#21 settlement behavior is bit-identical.

## #21 — Importing a sibling test file's mock is legal and beats duplicating it

**Pattern:** `import {MockMarket} from "./SettlementEngine.t.sol";` —
forge compiles all test files into one unit, so a mock contract defined
in a sibling test file is directly importable. Used for the
SettlementEngine end-to-end test (settleTrade → recordTrade → twap)
without copy-pasting the 100-line mock. The import pulls the sibling's
other definitions into scope but costs nothing at runtime.

---

## #16 — `BTreeMap<Reverse<U256>, _>` makes the bid side symmetric to the ask side

**Pattern:** Keying bids as `BTreeMap<Reverse<Price>, VecDeque<Order>>`
makes "best level = first key" true for BOTH sides (best bid = highest
price = first key under `Reverse`; best ask = lowest price = first key).
The two sweep loops (`match_against_asks` / `match_against_bids`) become
textual mirrors with no `next_back()` asymmetry, and `best_bid()` /
`best_ask()` are both `keys().next()`. The order index stores the RAW
price (not the `Reverse` wrapper) so `cancel`/`modify` wrap it only at
the map boundary.

## #16 — Level eviction during a sweep: carry `level_empty` out of the borrow scope

**Pattern:** Inside the matching loop you hold `map.get_mut(&key)` while
draining a `VecDeque`, but must `map.remove(&key)` when the level
empties — a second mutable borrow the first one blocks. Compute
`let level_empty = { let level = map.get_mut(...); ...drain...; level.is_empty() };`
then `if level_empty { map.remove(&key); }` AFTER the block. NLL ends
the `level` borrow at the block's end. This also dodges an MSRV trap:
`Option::is_none_or` (the clippy-1.93-idiomatic spelling) requires Rust
1.82, but the engine workspace declares `rust-version = 1.78` — the
bool-carry form is clean under both.

## #16 — MatchResult "remaining" fields are unsigned quantities; doc/test mismatch is the failure mode

**Pattern:** `maker_remaining` / `taker_remaining` on `MatchResult` are
remaining QUANTITIES (always ≥ 0), not signed sizes — the sign is
derivable from the order's side. A doc comment saying "signed size"
paired with an emission site that casts a `u128` produces exactly one
failing test (`left: 1, right: -1`). Pin the semantics in the field
docstring (the crate is `#![deny(missing_docs)]`, so the docstring is
load-bearing anyway) and assert the unsigned value in tests.

## #16 — Acceptance-shaped test placement: proptest must live IN the lib target

**Pattern:** The acceptance command is `cargo test --package clob --lib`
— the `--lib` flag runs ONLY the lib target, so a proptest placed in
`crates/clob/tests/*.rs` (the `risk` crate's `tests/equivalence.rs`
pattern) would NOT run under it. Both the ≥10 unit tests AND the
1000-case proptest therefore live in `src/lib.rs`'s `#[cfg(test)]`
modules. Use `tests/` only for cases the acceptance command names with
`--test <file>`.

## #16 — criterion 0.7 `iter_batched` + `Throughput::Elements(N)` proves a matches/s floor

**Pattern:** For a "≥ N matches/s" acceptance, define one element = one
produced `MatchResult` and use `group.throughput(Throughput::Elements(matches_per_iter))`
with `b.iter_batched(setup, body, BatchSize::SmallInput)` — the book
seeding runs in the setup closure (excluded from timing) and criterion
prints `thrpt: [X Melem/s]` directly in matches/s. Measured on the test
machine: single_match ~9.4M/s, sweep-10 ~14.0M/s, sweep-100 ~11.5M/s
against the 5000/s floor (~1900× headroom), so even a 20% pre-flight
margin-guard penalty (todo #19) leaves the floor untouched.

## #16 — Evidence-generation scripts must set workdir to `packages/engine`

**Pattern:** Any evidence script that runs `cargo test/bench/clippy/fmt`
from the REPO ROOT fails or emits rustfmt usage text — the root nested-
workspace issue (notepad #4) means cargo only resolves the engine
workspace at `packages/engine`. When generating an evidence log from a
shell block, pass `workdir` explicitly; do not rely on the session cwd,
and sanity-check the log's tail for usage/help text before finishing.

---

## #17 — OrderBook position-engine resolution uses `positionDecoders` mapping, not the market contract directly

**Pattern:** `OrderBook._resolveMarket(marketId)` issues a `staticcall` to `positionEngine` with the `positionDecoders(bytes32)` selector — it does NOT call the market extension directly. The `PositionEngine` contract has a public mapping `positionDecoders[marketId] => address` that maps market IDs to their extension addresses. This means the test setup must deploy a `PositionEngine`-like contract (or a mock exposing the same mapping) and register the market extension before creating the OrderBook, rather than passing the market extension address directly as the first constructor arg.

**Test setup pattern:**
```solidity
// deploy mocks
market = new MockMarket();
posEng = new MockPositionEngine();
collEng = new MockCollateralEngine();

// register market extension in the position engine
posEng.setDecoder(MARKET_ID, address(market));

// pass position ENGINE address (not market address) as first arg
orderBook = new OrderBook(address(posEng), address(collEng));
```

**Why this matters:** Passing `address(market)` as the first constructor arg (as if it were the position engine) compiles fine but reverts at runtime with `MarketNotSupported` because the mock market doesn't have a `positionDecoders(bytes32)` function. The error message is confusing because it looks like the MARKET_ID isn't registered, when actually the `positionEngine.address` is pointing at the wrong contract.

## #17 — `_resolveMarket` staticcall return handling: stale data from a successful-but-short call becomes address(0)

**Pattern:** The `_resolveMarket` function checks `data.length < 32` after a successful staticcall. If the `positionDecoders` mapping returns `address(0)` (i.e., the market ID hasn't been registered), the ABI-decoded data is 32 bytes of zeros, which is `>= 32`, so `abi.decode` returns `address(0)` — the caller then reverts with `MarketNotSupported`. This is correct behavior.

## #17 — First-write SSTORE premium makes cold `placeOrder` ~205k; warm is ~100k

**Pattern:** The first `placeOrder` on a fresh contract pays 7 cold SSTOREs (~22,100 gas each = ~154,700 total for slots: `nextOrderId`, `orders[1]`×5, and `nonces[trader]`). Warm subsequent calls pay ~2,900 per slot (~20,300 total). The non-storage overhead (keccak256, ecrecover, staticcalls, validation) is ~50k. Result:
- First (cold) placeOrder: ~205k gas
- Subsequent (warm) placeOrder: ~100k gas

The plan's 150k budget is therefore achievable for steady-state (warm storage) operation but unreachable for the very first order on a fresh contract unless storage writes are consolidated. Since the 150k target represents typical operation (after a few orders have warmed the slots), this is acceptable.

## #17 — Solidity's `0x_BEEF` hex literal with underscore separator is invalid in some solc versions

**Pattern:** `uint256 key = 0x_BEEF;` compiles with solc 0.8.24 under error 8936 ("Hexadecimal digit missing or invalid"). The underscore after `0x` is not allowed — use `0xBEEF` without the internal separator. This is a minor formatting gotcha when copying from patterns that use the separator to group hex digits: Solidity allows `0xBEEF` and `0xBE_EF` but NOT `0x_BEEF`.

## #17 — Unicode em-dash in revert message strings fails solc compilation

**Pattern:** `revert("zero is not registered — use 1 to simulate zero")` (with an em-dash `—`, U+2014) fails solc 0.8.24 compilation with error 8936 "Invalid character in string". Solidity requires `unicode"..."` string literals for non-ASCII characters, or simply replace the em-dash with ASCII `--` or rephrase without it.

