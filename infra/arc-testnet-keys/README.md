# Arc testnet deploy keys

Generated locally via `cast wallet new`, never committed (see `.gitignore`
in this dir — everything here is ignored except this file and that
`.gitignore` itself). Fund these three addresses with Arc testnet gas;
private keys stay in the sibling `*.json` files on this machine only.

| Role | Address | Needs funding? |
|---|---|---|
| `deployer` | `0xD8C2F8183f6b9aAA4A5Ec51303c77a9657fD8621` | Yes — deploys every contract, becomes admin |
| `keeper_price_pusher` | `0x4538FdB12e0F40939E4FCD98226054c1265Ec510` | Yes — signs real Pyth price-push txs continuously |
| `market_maker` | `0x91d1ceeeFaF00458bE4C26e63f79Ab99E56759CB` | Yes — signs real trade orders (also needs test USDC via the deployer's `adminMint`, not just gas) |

`keeper_liquidator` needs no separate funded key at all: it only needs an
address to receive `keeperReward` (paid out by the TEE's own signed tx,
not the keeper's), so the deployer's own address can be reused there —
nothing new to fund.

The TEE's own settlement-signing key is NOT here — it's generated inside
the Confidential Space enclave on first boot (or recovered from GCP KMS on
a later boot), never something generated ahead of time on this machine.
It gets funded and authorized (`AttestationRouter.authorizeTEE`) as a
separate step after the matcher's first real deploy, once its address is
known.
