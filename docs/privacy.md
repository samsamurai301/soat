# Customers' data (GDPR)

SOAT can hand a customer everything it keeps about them (GDPR articles 15
and 20) and erase them (article 17). It does this across every module,
including ones added later, because each module declares its own personal
data.

## For staff

On a customer's page in the admin:

- **Download their personal data** gives a JSON file to send them.
  - API: `GET /api/customers/{id}/personal-data` (`privacy:export`).
  - It holds their profile, addresses, carts, orders, invoices, reviews,
    wishlist, points, subscriptions, quotes and more, one list per table.
  - Session and sign-in tokens are left out.
- **Erase customer** asks for their email again, because it can't be undone.
  - API: `POST /api/customers/{id}/erase {"email": "..."}` (`privacy:erase`).
  - **Deleted:** their account, addresses, carts, reviews, wishlist, points,
    subscriptions, quotes and campaign mails.
  - **Kept without them:** orders and discount uses stay, without their
    email or link to them.
  - **Kept as they are:** invoices, because tax law requires keeping them
    for ten years (e.g. § 147 AO, § 14b UStG). An order's billing address
    stays for the same reason.
  - **Event log:** names, emails and addresses in it are blanked.
  - The answer reports what happened to each table.

Customers can download their own data and ask to be erased from their
account page:

- `GET /api/storefront/account/personal-data`
- `POST /api/storefront/account/erasure-request`

Their requests show under Admin → Privacy requests (`GET
/api/privacy-requests`, `privacy:read`). The GDPR gives you a month to
answer. Erasing the customer closes their request. You can also decline it
with the reason, for example an unpaid invoice that is still owed (`POST
/api/privacy-requests/{id}/decline`).

## Events

- `customer.data_exported` records each export, so the audit log shows who
  saw the data.
- `customer.erased` records each erasure.
- `privacy_request.created` and `privacy_request.declined` follow the
  requests.

None of them carries personal data.

## For module authors

A module that stores anything about a customer declares it in
`Module::personal_data`:

```rust
fn personal_data(&self) -> &'static [soat_module::PersonalData] {
    &[soat_module::PersonalData {
        table: "wishlist_items",
        matches: "customer_id = $2",          // $2 the customer's id, $3 their email (lowercase)
        export: &["product_id", "created_at"], // what goes in an export
        erase: soat_module::Erase::Delete,     // or Anonymize(&[("email", "''")]) or Keep("why")
    }]
}
```

`crates/soat/tests/privacy.rs` fails for any table that has a
`customer_id`, `email`, `phone`, `recipient`, `recipient_email` or
`author_name` column and no declaration. It also fails for a declaration
whose columns don't exist, or that anonymizes a NOT NULL column to NULL.
