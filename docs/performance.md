# Performance

`crates/soat/examples/load.rs` is the load baseline. It drives one SOAT on
a fresh Postgres schema with shoppers who each, again and again:

- open the home page,
- search,
- open a collection and a product,
- add the product to a cart and open the cart,
- open checkout, enter an address and place an order (paid by invoice).

At the same time a staff member lists orders through the API. Requests go
through the whole app in process: guards, sessions, templates, SQL under
row-level security, and the background event handlers that issue invoices
and emails for every order. So the numbers are SOAT's and Postgres's, not
the network's.

```sh
cargo run --release -p soat --example load                 # 20 shoppers for 20 s
SHOPPERS=50 SECONDS=60 cargo run --release -p soat --example load
```

It needs the test database (`SOAT_TEST_DATABASE_URL`, as for `cargo test`).
It prints p50, p95 and p99 for each step, and exits non-zero when a step's
p95 is over its budget or a step answered an unexpected status. Run it
before and after a change that touches a hot path: a page, the cart,
checkout, or the queries behind them.

## Budgets

These are p95 budgets for the default run (20 shoppers, release build, app
and Postgres on one 4-core machine), set at about twice the baseline below:

| Step | Budget (ms) |
|---|---|
| home, product | 50 |
| search, collection | 60 |
| cart add | 60 |
| cart | 50 |
| checkout | 80 |
| checkout address | 120 |
| place order | 250 |
| api orders (50 a page) | 80 |

A change that pushes a step over its budget needs a reason in its pull
request, or a fix. Budgets change only together with a new baseline below.

## Baseline (2026-11-27)

4 cores, release build, Postgres 16 on the same machine.

| Shoppers | Page p50 | Page p95 | Place order p95 | Orders a second |
|---|---|---|---|---|
| 1 | 3 ms | 4–7 ms | 11 ms | 23 |
| 20 | 15–27 ms | 25–44 ms | 105 ms | 82 |
| 50 | 48–63 ms | 62–83 ms | 236 ms | 76 |

One shopper sees a page in about 3 ms. From about 20 shoppers on, the
machine is saturated: Postgres uses about two of the four cores and SOAT
about one. Beyond that, more shoppers wait longer but do not place more
orders. To serve more, give Postgres its own machine first.
