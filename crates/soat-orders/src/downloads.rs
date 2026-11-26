//! Downloads: the files of an order's digital lines, for the buyer once
//! the order is paid. Links are signed and last an hour; asking again
//! gives fresh ones.

use soat_customers::accounts::SignedIn;
use soat_files::FileView;
use soat_tenancy::{Scope, Tenant};
use zapline::db::sqlx::PgConnection;
use zapline::db::Db;
use zapline::prelude::*;
use zapline::storage::Storage;

use crate::{load, Order, OrderToken, ORDERS_READ};

pub(crate) fn mount(app: App) -> App {
    app.mount((staff_downloads, my_downloads, guest_downloads))
}

/// A file the buyer of a digital line may download.
#[model]
pub struct Download {
    pub line_id: String,
    pub product_id: String,
    /// The line's title, e.g. `Field guide - EPUB`.
    pub title: String,
    pub file: FileView,
}

/// The downloads of `order`: 409 `order_not_paid` unless it is open and
/// paid (or partly refunded).
async fn of(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    order: &Order,
) -> Result<Vec<Download>> {
    let paid = matches!(
        order.financial_status.as_str(),
        "paid" | "partially_refunded"
    );
    if order.status != "open" || !paid {
        return Err(
            Error::conflict("downloads open once the order is paid").with_code("order_not_paid")
        );
    }
    let digital: Vec<_> = order
        .lines
        .iter()
        .filter(|l| !l.requires_shipping)
        .filter_map(|l| Some((l, l.product_id.clone()?)))
        .collect();
    let products: Vec<String> = digital.iter().map(|(_, p)| p.clone()).collect();
    let files = soat_catalog::downloads::downloads_of(conn, scope, &products).await?;
    let ids: Vec<String> = files.iter().map(|(_, f)| f.clone()).collect();
    let views = soat_files::views(conn, scope, storage, &ids).await?;
    let mut out = Vec::new();
    for (line, product) in &digital {
        for (p, f) in &files {
            if p != product {
                continue;
            }
            if let Some(v) = views.iter().find(|v| &v.id == f) {
                out.push(Download {
                    line_id: line.id.clone(),
                    product_id: product.clone(),
                    title: line.title.clone(),
                    file: v.clone(),
                });
            }
        }
    }
    Ok(out)
}

/// An order's downloads with fresh links (`orders:read`); 409
/// `order_not_paid` before it is paid.
#[get("/api/orders/{id}/downloads", tag = "orders")]
async fn staff_downloads(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<Json<Vec<Download>>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let order = load(&mut tx, &tenant, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let out = of(&mut tx, &tenant, &storage, &order).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// The downloads of one of the signed-in customer's orders; 409
/// `order_not_paid` before it is paid.
#[get("/api/storefront/account/orders/{id}/downloads", tag = "storefront")]
async fn my_downloads(
    db: Db,
    signed_in: SignedIn,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<Json<Vec<Download>>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let order = crate::customer_order(&mut tx, &signed_in.shop, &customer, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let out = of(&mut tx, &signed_in.shop, &storage, &order).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// The downloads of an order, for the guest holding its token
/// (`x-soat-order-token`); 409 `order_not_paid` before it is paid.
#[get("/api/storefront/orders/{id}/downloads", tag = "storefront")]
async fn guest_downloads(
    db: Db,
    holder: OrderToken,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<Json<Vec<Download>>> {
    let (mut tx, id) = holder.open(&db, &id).await?;
    let order = load(&mut tx, &holder.shop, &id)
        .await?
        .ok_or_else(Error::not_found)?;
    let out = of(&mut tx, &holder.shop, &storage, &order).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// The downloads of `order` for pages that show them (the storefront's
/// order page): empty unless it is paid.
///
/// # Errors
///
/// Database errors.
pub async fn ready_downloads(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    order: &Order,
) -> Result<Vec<Download>> {
    match of(conn, scope, storage, order).await {
        Err(e) if e.status() == StatusCode::CONFLICT => Ok(Vec::new()),
        other => other,
    }
}
