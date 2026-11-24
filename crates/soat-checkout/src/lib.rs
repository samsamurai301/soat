//! Checkout: from a cart to an order and a payment.
//!
//! The shopper, holding the cart's token, gives an email, a shipping
//! address (a billing address, a VAT number) and picks one of the shipping
//! options for that address, and may enter a discount code.
//! `GET .../checkout` shows the totals with tax
//! at every step and what is still missing. Completing it, in one
//! transaction, places the order, holds the stock for it and closes the
//! cart; then the payment starts with the chosen method.
//!
//! Stock is held for an hour while a card payment completes, and for
//! good once the order is paid; payments staff confirm (bank transfer)
//! hold it until the order is paid or canceled.

use std::sync::Arc;

use serde_json::Value;
use soat_cart::storefront::CartToken;
use soat_cart::{view, CartView};
use soat_core::address::is_country_code;
use soat_core::{Currency, Permission};
use soat_module::{Event, HandlerFuture, Module, Subscriber};
use soat_orders::{NewOrder, NewOrderLine, NewShipping, Order, PostalAddress};
use soat_payments::{Payment, Providers, StartPayment};
use soat_promotions::{AppliedDiscount, DiscountContext, DiscountLine};
use soat_shipping::{ShippingOption, ShippingQuery};
use soat_tax::{QuoteInput, TaxQuote};
use soat_tenancy::{Scope, Shop, TenantTx};
use zapline::db::sqlx::types::Json as SqlJson;
use zapline::db::sqlx::{PgConnection, PgPool};
use zapline::db::{sqlx, Db};
use zapline::embed::Dir;
use zapline::prelude::*;
use zapline::Resources;

pub mod drafts;

/// How long stock is held for an order whose payment is still running.
pub const HOLD_SECONDS: i64 = 3600;

/// The checkout module.
pub struct Checkout;

impl Module for Checkout {
    fn name(&self) -> &'static str {
        "checkout"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &[
            "tenancy",
            "events",
            "catalog",
            "customers",
            "inventory",
            "pricing",
            "cart",
            "tax",
            "shipping",
            "payments",
            "promotions",
            "giftcards",
            "orders",
        ]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(KeepPaidStock)]
    }

    fn mount(&self, app: App) -> App {
        drafts::mount(app).mount((get_checkout, update_checkout, complete_checkout))
    }
}

/// Body of a checkout change; absent fields stay as they are.
#[model]
pub struct CheckoutChange {
    #[validate(email, length(max = 254))]
    pub email: Option<String>,
    pub shipping_address: Option<PostalAddress>,
    /// Defaults to the shipping address.
    pub billing_address: Option<PostalAddress>,
    /// A business buyer's EU VAT number (reverse charge).
    #[validate(length(max = 20))]
    pub vat_number: Option<String>,
    /// One of `shipping_options`.
    pub shipping_rate_id: Option<String>,
    /// A discount code; an empty string removes it. A code that does not
    /// apply is a 422 `invalid_discount` saying why.
    #[validate(length(max = 64))]
    pub discount_code: Option<String>,
    /// A gift card's code; an empty string removes it. A card that cannot
    /// pay is a 422 `invalid_gift_card` saying why.
    #[validate(length(max = 64))]
    pub gift_card_code: Option<String>,
    /// Spend the signed-in customer's store credit.
    pub use_store_credit: Option<bool>,
}

/// A gift card or store credit paying part of the order.
#[model]
pub struct AppliedBalance {
    pub gift_card_id: String,
    /// The code's last characters; empty for store credit.
    pub last_chars: String,
    /// What is on it now.
    pub balance_minor: i64,
    /// What it pays of this order.
    pub applied_minor: i64,
}

/// A checkout with today's prices, shipping and tax.
#[model]
pub struct CheckoutView {
    pub cart: CartView,
    pub email: String,
    pub shipping_address: Option<PostalAddress>,
    pub billing_address: Option<PostalAddress>,
    pub vat_number: Option<String>,
    /// What can be chosen for the shipping address, cheapest first.
    pub shipping_options: Vec<ShippingOption>,
    /// The chosen option, while it still applies.
    pub shipping: Option<ShippingOption>,
    /// Tax on the lines and shipping, once the address is known.
    pub tax: Option<TaxQuote>,
    /// The automatic discount that applies, worth most of those that do;
    /// it comes off before any code.
    pub automatic_discount: Option<AppliedDiscount>,
    /// The code entered and what it takes off, while it applies.
    pub discount: Option<AppliedDiscount>,
    /// Why the code entered no longer applies (e.g. the cart changed).
    pub discount_issue: Option<String>,
    pub subtotal_minor: i64,
    pub shipping_minor: i64,
    /// Taken off the lines and shipping.
    pub discount_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    /// The gift card entered, while it can pay.
    pub gift_card: Option<AppliedBalance>,
    /// Why the gift card entered can no longer pay.
    pub gift_card_issue: Option<String>,
    /// The signed-in customer's store credit in the cart's currency, and
    /// what of it pays when `use_store_credit` is set.
    pub store_credit: Option<AppliedBalance>,
    pub use_store_credit: bool,
    /// What is left to pay with a payment method, after gift card and
    /// store credit.
    pub amount_due_minor: i64,
    pub prices_include_tax: bool,
    /// What is still needed: `lines` (the cart is empty or has issues),
    /// `email`, `shipping_address`, `shipping`.
    pub missing: Vec<String>,
    pub ready: bool,
}

/// The order placed and its payment.
#[model]
pub struct Completed {
    pub order: Order,
    /// Send as `x-soat-order-token` to see the order; shown once.
    pub order_token: String,
    /// Null when nothing is to be paid. Its `client_secret` and
    /// `next_action` tell the storefront what the buyer does next.
    pub payment: Option<Payment>,
}

/// Body of completing a checkout.
#[model]
pub struct Complete {
    /// One of `GET /api/storefront/payment-methods`; needed unless a gift
    /// card or store credit pays it all.
    pub payment_method_id: Option<String>,
    /// Where a payment provider sends the buyer back.
    #[validate(url)]
    pub return_url: Option<String>,
}

#[derive(sqlx::FromRow, Default)]
struct Details {
    email: Option<String>,
    shipping_address: Option<SqlJson<Value>>,
    billing_address: Option<SqlJson<Value>>,
    vat_number: Option<String>,
    shipping_rate_id: Option<String>,
    discount_code: Option<String>,
    gift_card_id: Option<String>,
    use_store_credit: bool,
}

fn address(v: &Option<SqlJson<Value>>) -> Option<PostalAddress> {
    v.as_ref()
        .and_then(|j| serde_json::from_value(j.0.clone()).ok())
}

fn checked(a: &PostalAddress) -> Result<()> {
    let bad = |m: &str| Err(Error::unprocessable(m.to_owned()).with_code("invalid_address"));
    if a.line1.trim().is_empty() || a.city.trim().is_empty() {
        return bad("an address needs line1 and city");
    }
    if !is_country_code(&a.country) {
        return bad("country is an ISO 3166-1 alpha-2 code, e.g. DE");
    }
    let long = [
        (&a.first_name, 100),
        (&a.last_name, 100),
        (&a.company, 200),
        (&a.line1, 200),
        (&a.line2, 200),
        (&a.postal_code, 20),
        (&a.city, 100),
        (&a.region, 100),
        (&a.phone, 40),
    ];
    if long.iter().any(|(s, max)| s.chars().count() > *max) {
        return bad("an address field is too long");
    }
    Ok(())
}

/// A checkout worked out, with what an order needs.
struct Built {
    view: CheckoutView,
    lines: Vec<NewOrderLine>,
    shipping: Option<NewShipping>,
    currency: Currency,
    /// The error a code entered gives, while it does not apply.
    discount_error: Option<Error>,
}

async fn build(conn: &mut PgConnection, scope: &impl Scope, cart_id: &str) -> Result<Built> {
    let cart = view(conn, scope, cart_id).await?;
    let d = sqlx::query_as::<_, Details>(
        "SELECT email, shipping_address, billing_address, vat_number, shipping_rate_id, \
         discount_code, gift_card_id, use_store_credit FROM checkouts \
         WHERE tenant_id = $1 AND cart_id = $2",
    )
    .bind(scope.tenant_id())
    .bind(cart_id)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or_default();
    let currency = Currency::parse(&cart.currency).map_err(|e| Error::internal(e.to_string()))?;
    let email = d
        .email
        .clone()
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| cart.email.clone());
    let shipping_address = address(&d.shipping_address);
    let billing_address = address(&d.billing_address).or_else(|| shipping_address.clone());

    // Lines as they would be ordered.
    let mut lines = Vec::with_capacity(cart.lines.len());
    let mut weight = 0i64;
    for l in &cart.lines {
        let (Some(price), None) = (l.unit_price_minor, &l.issue) else {
            continue;
        };
        let Some(item) = soat_catalog::stock_item(conn, scope, &l.item_id).await? else {
            continue;
        };
        weight = weight.saturating_add(item.weight_grams.saturating_mul(l.quantity));
        lines.push(NewOrderLine {
            item_id: item.id,
            product_id: Some(item.product_id),
            sku: item.sku,
            title: item.title,
            quantity: l.quantity,
            unit_price_minor: price,
            tax_class: item.tax_class,
            tax_rate_bps: 0,
            tax_minor: 0,
            discount_minor: 0,
        });
    }

    let mut missing = Vec::new();
    if !cart.ready {
        missing.push("lines".to_owned());
    }
    if email.is_empty() {
        missing.push("email".to_owned());
    }
    let (mut options, mut chosen, mut tax) = (Vec::new(), None, None);
    match &shipping_address {
        None => missing.push("shipping_address".to_owned()),
        Some(addr) => {
            let q = ShippingQuery {
                country: addr.country.clone(),
                currency: cart.currency.clone(),
                subtotal_minor: cart.subtotal_minor,
                weight_grams: weight,
            };
            options = soat_shipping::options(conn, scope, &q).await?;
            chosen = d
                .shipping_rate_id
                .as_ref()
                .and_then(|id| options.iter().find(|o| &o.rate_id == id).cloned());
            if chosen.is_none() {
                missing.push("shipping".to_owned());
            }
        }
    }

    // The automatic discount worth most, then the code on what is left.
    let mut offs: Vec<DiscountLine> = lines
        .iter()
        .map(|l| DiscountLine {
            product_id: l.product_id.clone(),
            unit_price_minor: l.unit_price_minor,
            quantity: l.quantity,
            off_minor: 0,
        })
        .collect();
    let shipping_price = chosen.as_ref().map_or(0, |s| s.price_minor);
    let auto = {
        let ctx = DiscountContext {
            currency: &cart.currency,
            lines: &offs,
            shipping_minor: shipping_price,
            email: &email,
            customer_id: cart.customer_id.as_deref(),
        };
        soat_promotions::automatic(conn, scope, &ctx).await?
    };
    if let Some(a) = &auto {
        for (l, off) in offs.iter_mut().zip(&a.lines_minor) {
            l.off_minor += off;
        }
    }
    let auto_shipping = auto.as_ref().map_or(0, |a| a.shipping_minor);
    let (mut discount, mut discount_error) = (None, None);
    if let Some(code) = d.discount_code.as_deref().filter(|c| !c.is_empty()) {
        let ctx = DiscountContext {
            currency: &cart.currency,
            lines: &offs,
            shipping_minor: shipping_price - auto_shipping,
            email: &email,
            customer_id: cart.customer_id.as_deref(),
        };
        match soat_promotions::check(conn, scope, code, &ctx).await {
            Ok(applied) => discount = Some(applied),
            Err(e) if e.status() == StatusCode::UNPROCESSABLE_ENTITY => discount_error = Some(e),
            Err(e) => return Err(e),
        }
    }
    if let Some(a) = &discount {
        for (l, off) in offs.iter_mut().zip(&a.lines_minor) {
            l.off_minor += off;
        }
    }
    for (line, off) in lines.iter_mut().zip(&offs) {
        line.discount_minor = off.off_minor;
    }
    let shipping_discount = auto_shipping + discount.as_ref().map_or(0, |a| a.shipping_minor);

    if let Some(addr) = &shipping_address {
        let mut amounts: Vec<(String, i64)> = lines
            .iter()
            .map(|l| {
                (
                    l.tax_class.clone(),
                    l.unit_price_minor * l.quantity - l.discount_minor,
                )
            })
            .collect();
        if let Some(s) = &chosen {
            amounts.push((s.tax_class.clone(), s.price_minor - shipping_discount));
        }
        let quote = soat_tax::quote(
            conn,
            scope,
            &QuoteInput {
                country: addr.country.clone(),
                vat_number: d.vat_number.clone(),
                lines: amounts,
            },
        )
        .await?;
        for (line, taxed) in lines.iter_mut().zip(&quote.lines) {
            line.tax_rate_bps = taxed.rate_bps;
            line.tax_minor = taxed.tax_minor;
        }
        tax = Some(quote);
    }
    let prices_include_tax = match &tax {
        Some(q) => q.prices_include_tax,
        None => soat_tax::settings(conn, scope).await?.prices_include_tax,
    };
    let shipping = chosen.as_ref().map(|s| NewShipping {
        rate_id: s.rate_id.clone(),
        name: s.name.clone(),
        price_minor: s.price_minor,
        discount_minor: shipping_discount,
        tax_minor: tax
            .as_ref()
            .and_then(|q| q.lines.get(lines.len()))
            .map_or(0, |l| l.tax_minor),
        tax_rate_bps: tax
            .as_ref()
            .and_then(|q| q.lines.get(lines.len()))
            .map_or(0, |l| l.rate_bps),
    });
    let draft = NewOrder {
        cart_id: Some(cart.id.clone()),
        customer_id: cart.customer_id.clone(),
        email: email.clone(),
        currency,
        lines: lines.clone(),
        shipping: shipping.clone(),
        shipping_address: shipping_address.clone(),
        billing_address: billing_address.clone(),
        prices_include_tax,
        tax_country: None,
        reverse_charge: false,
        vat_number: None,
        note: String::new(),
        discount_code: None,
    };
    let totals = soat_orders::totals(&draft)?;
    let ready = missing.is_empty();

    // Gift card, then store credit, pay what they can of the total.
    let mut due = totals.total;
    let (mut gift_card, mut gift_card_issue) = (None, None);
    if let Some(card) = &d.gift_card_id {
        match soat_giftcards::usable(conn, scope, card, &cart.currency).await? {
            Some((card, None)) => {
                let applied = card.balance_minor.min(due);
                due -= applied;
                gift_card = Some(AppliedBalance {
                    gift_card_id: card.id,
                    last_chars: card.last_chars,
                    balance_minor: card.balance_minor,
                    applied_minor: applied,
                });
            }
            Some((_, Some(why))) => gift_card_issue = Some(why),
            None => gift_card_issue = Some("no such gift card".into()),
        }
    }
    let mut store_credit = None;
    if let Some(customer) = &cart.customer_id {
        if let Some(card) =
            soat_giftcards::store_credit(conn, scope, customer, &cart.currency).await?
        {
            let applied = if d.use_store_credit {
                card.balance_minor.min(due)
            } else {
                0
            };
            due -= applied;
            store_credit = Some(AppliedBalance {
                gift_card_id: card.id,
                last_chars: card.last_chars,
                balance_minor: card.balance_minor,
                applied_minor: applied,
            });
        }
    }
    Ok(Built {
        view: CheckoutView {
            cart,
            email,
            shipping_address,
            billing_address,
            vat_number: d.vat_number,
            shipping_options: options,
            shipping: chosen,
            tax,
            discount_issue: discount_error.as_ref().map(|e| e.message().to_owned()),
            discount,
            automatic_discount: auto,
            subtotal_minor: totals.subtotal,
            shipping_minor: totals.shipping,
            discount_minor: totals.discount,
            tax_minor: totals.tax,
            total_minor: totals.total,
            gift_card,
            gift_card_issue,
            store_credit,
            use_store_credit: d.use_store_credit,
            amount_due_minor: due,
            prices_include_tax,
            missing,
            ready,
        },
        lines,
        shipping,
        currency,
        discount_error,
    })
}

/// The cart's checkout: totals with shipping and tax, and what is still
/// missing.
#[get("/api/storefront/carts/{id}/checkout", tag = "storefront")]
async fn get_checkout(
    db: Db,
    holder: CartToken,
    Path(id): Path<String>,
) -> Result<Json<CheckoutView>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let built = build(&mut tx, &holder.shop, &id).await?;
    tx.commit().await?;
    Ok(Json(built.view))
}

/// Saves `c` on the checkout of open cart `id` (locked with
/// [`CartToken::open`]) and returns it worked out again.
///
/// # Errors
///
/// 422 for a bad address or VAT number, a shipping choice that does not
/// apply, or a discount code that does not apply.
pub async fn update(
    tx: &mut TenantTx,
    shop: &Shop,
    id: &str,
    c: &CheckoutChange,
) -> Result<CheckoutView> {
    for a in [&c.shipping_address, &c.billing_address]
        .into_iter()
        .flatten()
    {
        checked(a)?;
    }
    if let Some(v) = c.vat_number.as_deref().filter(|v| !v.trim().is_empty()) {
        if soat_core::tax::vat_number_country(v).is_none() {
            return Err(Error::unprocessable(format!("{v} is not an EU VAT number"))
                .with_code("invalid_vat_number"));
        }
    }
    // A gift card code becomes the card's id; "" removes it.
    let gift_card = match c.gift_card_code.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(String::new()),
        Some(code) => {
            let cart = soat_cart::view(tx, shop, id).await?;
            Some(
                soat_giftcards::find(tx, shop, code, &cart.currency)
                    .await?
                    .id,
            )
        }
    };
    let json = |a: &Option<PostalAddress>| {
        a.as_ref()
            .map(|a| SqlJson(serde_json::to_value(a).unwrap_or(Value::Null)))
    };
    sqlx::query(
        "INSERT INTO checkouts (cart_id, tenant_id, email, shipping_address, billing_address, \
         vat_number, shipping_rate_id, discount_code, gift_card_id, use_store_credit) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULLIF($9, ''), COALESCE($10, FALSE)) \
         ON CONFLICT (cart_id) DO UPDATE SET \
         email = COALESCE(EXCLUDED.email, checkouts.email), \
         shipping_address = COALESCE(EXCLUDED.shipping_address, checkouts.shipping_address), \
         billing_address = COALESCE(EXCLUDED.billing_address, checkouts.billing_address), \
         vat_number = COALESCE(EXCLUDED.vat_number, checkouts.vat_number), \
         shipping_rate_id = COALESCE(EXCLUDED.shipping_rate_id, checkouts.shipping_rate_id), \
         discount_code = COALESCE(EXCLUDED.discount_code, checkouts.discount_code), \
         gift_card_id = CASE WHEN $9::text IS NULL THEN checkouts.gift_card_id \
                        ELSE NULLIF($9, '') END, \
         use_store_credit = COALESCE($10, checkouts.use_store_credit), \
         updated_at = now()",
    )
    .bind(id)
    .bind(shop.id())
    .bind(c.email.as_deref().map(|e| e.trim().to_lowercase()))
    .bind(json(&c.shipping_address))
    .bind(json(&c.billing_address))
    .bind(c.vat_number.as_deref().map(str::trim))
    .bind(&c.shipping_rate_id)
    .bind(c.discount_code.as_deref().map(str::trim))
    .bind(gift_card)
    .bind(c.use_store_credit)
    .execute(&mut **tx)
    .await?;
    let mut built = build(tx, shop, id).await?;
    if c.discount_code
        .as_deref()
        .is_some_and(|c| !c.trim().is_empty())
    {
        if let Some(e) = built.discount_error.take() {
            return Err(e);
        }
    }
    if let Some(rate) = &c.shipping_rate_id {
        if built.view.shipping.as_ref().map(|s| &s.rate_id) != Some(rate) {
            return Err(Error::unprocessable(
                "that shipping option does not apply to this cart and address",
            )
            .with_code("invalid_shipping"));
        }
    }
    Ok(built.view)
}

/// The checkout of cart `id`: totals with shipping and tax, and what is
/// still missing.
pub async fn checkout(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
) -> Result<CheckoutView> {
    Ok(build(conn, scope, id).await?.view)
}

/// Give the checkout's email, addresses, VAT number, shipping choice or
/// discount code. A shipping choice that does not apply to the address, or
/// a code that does not apply to the cart, is a 422.
#[patch("/api/storefront/carts/{id}/checkout", tag = "storefront")]
async fn update_checkout(
    db: Db,
    holder: CartToken,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<CheckoutChange>>,
) -> Result<Json<CheckoutView>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let view = update(&mut tx, &holder.shop, &id, &c).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Places the order of cart `id` held by `holder` and starts its payment.
///
/// # Errors
///
/// See `POST /api/storefront/carts/{id}/checkout/complete`.
pub async fn complete(
    db: &Db,
    providers: &Providers,
    holder: &CartToken,
    id: &str,
    c: Complete,
) -> Result<Completed> {
    let (mut tx, id) = holder.open(db, id).await?;
    let shop = &holder.shop;
    let mut built = build(&mut tx, shop, &id).await?;
    if let Some(e) = built.discount_error.take() {
        return Err(e);
    }
    if !built.view.ready {
        return Err(Error::unprocessable(format!(
            "the checkout still needs: {}",
            built.view.missing.join(", ")
        ))
        .with_code("checkout_incomplete")
        .with_details(&built.view.missing));
    }
    if let Some(why) = built.view.gift_card_issue.take() {
        return Err(Error::unprocessable(why).with_code("invalid_gift_card"));
    }
    let due = built.view.amount_due_minor;
    let method = match c.payment_method_id.as_deref().filter(|_| due > 0) {
        Some(m) => Some(
            soat_payments::active_method(&mut tx, shop, m)
                .await?
                .ok_or_else(|| {
                    Error::unprocessable("choose one of the shop's payment methods")
                        .with_code("unknown_payment_method")
                })?,
        ),
        None if due > 0 => {
            return Err(
                Error::unprocessable("choose one of the shop's payment methods")
                    .with_code("unknown_payment_method"),
            )
        }
        None => None,
    };
    let confirmed_by_staff = method.as_ref().is_some_and(|m| {
        providers
            .get(&m.provider)
            .is_some_and(|p| p.confirmed_by_staff())
    });
    let view = &built.view;
    let tax = view.tax.as_ref();
    let order = NewOrder {
        cart_id: Some(id.clone()),
        customer_id: view.cart.customer_id.clone(),
        email: view.email.clone(),
        currency: built.currency,
        lines: built.lines.clone(),
        shipping: built.shipping.clone(),
        shipping_address: view.shipping_address.clone(),
        billing_address: view.billing_address.clone(),
        prices_include_tax: view.prices_include_tax,
        tax_country: tax.map(|t| t.tax_country.clone()),
        reverse_charge: tax.is_some_and(|t| t.reverse_charge),
        vat_number: view.vat_number.clone(),
        note: String::new(),
        discount_code: view.discount.as_ref().and_then(|a| a.code.clone()),
    };
    let (mut placed, token) = soat_orders::place(&mut tx, shop, &order).await?;
    for applied in view.automatic_discount.iter().chain(&view.discount) {
        soat_promotions::redeem(
            &mut tx,
            shop,
            applied,
            &placed.id,
            placed.customer_id.as_deref(),
            &placed.email,
        )
        .await?;
    }
    hold(&mut tx, shop, &built.lines, &placed.id, confirmed_by_staff).await?;
    let balances = view.gift_card.iter().chain(&view.store_credit);
    let mut paid_by_balance = false;
    for b in balances.filter(|b| b.applied_minor > 0) {
        soat_giftcards::redeem(
            &mut tx,
            shop,
            &b.gift_card_id,
            b.applied_minor,
            &placed.id,
            built.currency.as_str(),
        )
        .await?;
        paid_by_balance = true;
    }
    if paid_by_balance {
        soat_orders::sync_payments(&mut tx, shop, &placed.id).await?;
        placed = soat_orders::load(&mut tx, shop, &placed.id)
            .await?
            .ok_or_else(|| Error::internal("order vanished"))?;
    }
    soat_cart::complete(&mut tx, shop, &id, &placed.id).await?;
    tx.commit().await?;

    let payment = if let Some(method) = method.filter(|_| due > 0) {
        let req = StartPayment {
            method_id: method.id,
            reference: placed.id.clone(),
            amount_minor: due,
            currency: built.currency,
            return_url: c.return_url,
        };
        Some(soat_payments::start(db, providers, shop, &req).await?)
    } else {
        None
    };
    Ok(Completed {
        order: placed,
        order_token: token,
        payment,
    })
}

/// Place the order: checks that nothing is missing (422
/// `checkout_incomplete` names it), holds the stock (409
/// `insufficient_stock`), closes the cart and starts the payment with the
/// chosen method. A discount code that no longer applies is a 422
/// `invalid_discount`. Publishes `order.placed`, `cart.completed` and
/// `discount.redeemed`.
#[post(
    "/api/storefront/carts/{id}/checkout/complete",
    tag = "storefront",
    status = 201
)]
async fn complete_checkout(
    db: Db,
    holder: CartToken,
    providers: State<Providers>,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<Complete>>,
) -> Result<(StatusCode, Json<Completed>)> {
    let done = complete(&db, &providers, &holder, &id, c).await?;
    Ok((StatusCode::CREATED, Json(done)))
}

async fn hold(
    tx: &mut TenantTx,
    scope: &impl Scope,
    lines: &[NewOrderLine],
    order_id: &str,
    until_paid: bool,
) -> Result<()> {
    let ttl = if until_paid { None } else { Some(HOLD_SECONDS) };
    for l in lines {
        soat_inventory::reservations::reserve(tx, scope, &l.item_id, l.quantity, order_id, ttl)
            .await?;
    }
    Ok(())
}

/// The shop an event belongs to; the system acts.
struct EventScope(String);

impl Scope for EventScope {
    fn tenant_id(&self) -> &str {
        &self.0
    }

    fn actor_id(&self) -> Option<&str> {
        None
    }
}

/// Holds a paid order's stock until it ships or is canceled.
struct KeepPaidStock;

impl Subscriber for KeepPaidStock {
    fn name(&self) -> &'static str {
        "checkout"
    }

    fn wants(&self, kind: &str) -> bool {
        kind == "order.paid"
    }

    fn handle<'a>(&'a self, event: &'a Event, res: &'a Resources) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(order) = event.payload["id"].as_str() else {
                return Ok(());
            };
            let pool: PgPool = res.require()?;
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT set_config('soat.tenant', $1, true)")
                .bind(&event.tenant_id)
                .execute(&mut *tx)
                .await?;
            let scope = EventScope(event.tenant_id.clone());
            soat_inventory::reservations::keep(&mut tx, &scope, order).await?;
            tx.commit().await?;
            Ok(())
        })
    }
}
