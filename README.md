# cdk-ehash

A [`MintPayment`] processor for [Hashpool] mining-share rewards, built on
the [Cashu Development Kit (CDK)].

Miners submit a valid proof-of-work share (identified by its `header_hash`).
Once Hashpool validates the share it calls `pay_ehash_quote()` on the
processor, which marks the corresponding quote as paid so the miner can
mint ecash tokens.

## Overview

The mint flow has four steps:

1. **Quote** — the miner's wallet calls `POST /v1/mint/quote/ehash` with
   `{"amount": N, "unit": "ehash", "extra": {"header_hash": "<64-char hex>"}}`.
   The mint returns a quote ID.

2. **Validate** — the miner submits the share to Hashpool. Hashpool verifies
   proof-of-work and calls `EhashPaymentProcessor::pay_ehash_quote(header_hash, amount)`.

3. **Poll** — the wallet polls `GET /v1/mint/quote/ehash/<id>` until
   `"state": "PAID"`.

4. **Mint** — the wallet calls `POST /v1/mint/ehash` with the quote ID and
   blinded messages to receive ecash tokens.

## Wiring into Hashpool's mint binary

`cdk-ehash` plugs into CDK via `MintBuilder::add_payment_processor`. No
`cdk-mintd` dependency is needed — Hashpool builds its own mint binary:

```rust
use std::sync::Arc;
use cdk::mint::{MintBuilder, MintMeltLimits};
use cdk::nuts::{CurrencyUnit, PaymentMethod};
use cdk_ehash::EhashPaymentProcessor;

let unit = CurrencyUnit::Custom("ehash".to_string());
let processor = Arc::new(EhashPaymentProcessor::new(unit.clone()));

let mut builder = MintBuilder::new(localstore.clone());
builder
    .add_payment_processor(
        unit,
        PaymentMethod::Custom("ehash".to_string()),
        MintMeltLimits::new(1, 1_000_000),
        processor.clone(),
    )
    .await?;

let mint = builder
    .with_name("Hashpool Mint".to_string())
    // ... other configuration ...
    .build_with_seed(localstore, &seed)
    .await?;

// Keep `processor` alive — call this when a share is validated:
// processor.pay_ehash_quote(&header_hash, amount).await?;
```

The `cdk-axum` router auto-generates all custom-method endpoints
(`/v1/mint/quote/ehash`, `/v1/mint/ehash`, etc.) from the registered
processor — no extra routing code required.

## Dependency note

`cdk-ehash` targets CDK's upstream `main` branch. The `hashed_derivation_index`
feature needed for custom currency units is not yet in a stable CDK release.
Until CDK publishes a release with that feature, add the following to your
`Cargo.toml` to patch the published crates with the upstream source:

```toml
[patch.crates-io]
cashu = { git = "https://github.com/cashubtc/cdk" }
cdk = { git = "https://github.com/cashubtc/cdk" }
cdk-common = { git = "https://github.com/cashubtc/cdk" }
cdk-http-client = { git = "https://github.com/cashubtc/cdk" }
cdk-signatory = { git = "https://github.com/cashubtc/cdk" }
cdk-sql-common = { git = "https://github.com/cashubtc/cdk" }
cdk-sqlite = { git = "https://github.com/cashubtc/cdk" }
```

## License

MIT — same as upstream CDK.

[`MintPayment`]: https://docs.rs/cdk-common/latest/cdk_common/payment/trait.MintPayment.html
[Hashpool]: https://github.com/vnprc/hashpool
[Cashu Development Kit (CDK)]: https://github.com/cashubtc/cdk
