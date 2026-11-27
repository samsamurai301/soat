//! Shipping labels: printed for a shipment with one of the shop's carrier
//! accounts ([`soat_shipping::carriers`]). The label's tracking goes onto
//! the shipment, and its PDF is kept as a file of the shop.

use serde_json::json;
use soat_core::id_kind;
use soat_files::FileView;
use soat_shipping::carriers::{self, Carriers, LabelParts};
use soat_tenancy::{Scope, Tenant, TenantTx};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;
use zapline::storage::Storage;

use crate::fulfillments::{changed, locked};
use crate::{load, ORDERS_READ, ORDERS_WRITE};

id_kind!(LabelId, LabelKind, "lbl");

/// A printed label.
#[model]
pub struct ShippingLabel {
    /// e.g. `lbl_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub fulfillment_id: String,
    pub carrier_account_id: String,
    /// The carrier that takes it, e.g. `dhl`.
    pub carrier: String,
    pub tracking_number: String,
    pub tracking_url: String,
    /// `active` or `canceled`.
    pub status: String,
    /// The PDF, with a link valid for an hour.
    pub file: Option<FileView>,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// Body of a label request.
#[model]
pub struct NewLabel {
    pub carrier_account_id: String,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((create_label, list_labels, cancel_label))
}

type Row = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
);

const COLUMNS: &str = "id, fulfillment_id, carrier_account_id, carrier, tracking_number, \
     tracking_url, status, file_id, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";

async fn views(
    tx: &mut TenantTx,
    scope: &impl Scope,
    storage: &Storage,
    rows: Vec<Row>,
) -> Result<Vec<ShippingLabel>> {
    let ids: Vec<String> = rows.iter().filter_map(|r| r.7.clone()).collect();
    let files = soat_files::views(tx, scope, storage, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|r| ShippingLabel {
            file: r
                .7
                .as_ref()
                .and_then(|f| files.iter().find(|v| &v.id == f).cloned()),
            id: r.0,
            fulfillment_id: r.1,
            carrier_account_id: r.2,
            carrier: r.3,
            tracking_number: r.4,
            tracking_url: r.5,
            status: r.6,
            created_at: r.8,
        })
        .collect())
}

/// Print a label for a shipment (`orders:write`) with one of the shop's
/// carrier accounts: it goes to the order's shipping address (422
/// `no_shipping_address` without one), weighs what the shipment holds, and
/// its carrier and tracking go onto the shipment. A canceled or delivered
/// shipment is a 409. Publishes `label.created` and `fulfillment.updated`.
#[post("/api/fulfillments/{id}/labels", tag = "orders", status = 201)]
async fn create_label(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    carriers: State<Carriers>,
    Path(id): Path<String>,
    Json(b): Json<NewLabel>,
) -> Result<(StatusCode, Json<ShippingLabel>)> {
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let (id, order_id, status) = locked(&mut tx, &tenant, &id).await?;
    if status != "shipped" {
        return Err(
            Error::conflict(format!("a {status} shipment gets no label"))
                .with_code("invalid_fulfillment_status"),
        );
    }
    let order = load(&mut tx, &tenant, &order_id)
        .await?
        .ok_or_else(|| Error::internal("order vanished"))?;
    let to = order
        .shipping_address
        .as_ref()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .ok_or_else(|| {
            Error::unprocessable("the order has no shipping address")
                .with_code("no_shipping_address")
        })?;
    let f = order
        .fulfillments
        .iter()
        .find(|f| f.id == id)
        .ok_or_else(|| Error::internal("shipment vanished"))?;
    let mut weight = 0i64;
    for l in &f.lines {
        if let Some(item) = soat_catalog::stock_item(&mut tx, &tenant, &l.item_id)
            .await
            .ok()
            .flatten()
        {
            weight += item.weight_grams * i64::from(l.quantity);
        }
    }
    let reference = order.number.to_string();
    let (account, label) = carriers::create_label(
        &mut tx,
        &tenant,
        &carriers,
        &b.carrier_account_id,
        LabelParts {
            to: &to,
            email: &order.email,
            weight_grams: weight,
            reference: &reference,
        },
    )
    .await?;
    let file = soat_files::store(
        &mut tx,
        &tenant,
        &storage,
        &format!("label-{}.pdf", order.number),
        "",
        label.pdf.clone(),
    )
    .await?;
    let row: Row = sqlx::query_as(&format!(
        "INSERT INTO shipping_labels (id, tenant_id, fulfillment_id, carrier_account_id, \
         carrier, tracking_number, tracking_url, carrier_ref, file_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {COLUMNS}"
    ))
    .bind(LabelId::new().as_str())
    .bind(tenant.id())
    .bind(&id)
    .bind(&account.id)
    .bind(&label.carrier)
    .bind(&label.tracking_number)
    .bind(&label.tracking_url)
    .bind(&label.carrier_ref)
    .bind(&file.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE fulfillments SET carrier = $3, tracking_number = $4, tracking_url = $5 \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.id())
    .bind(&id)
    .bind(&label.carrier)
    .bind(&label.tracking_number)
    .bind(&label.tracking_url)
    .execute(&mut *tx)
    .await?;
    let made = views(&mut tx, &tenant, &storage, vec![row])
        .await?
        .remove(0);
    tenant
        .publish(
            &mut tx,
            "label.created",
            &json!({ "order_id": order_id, "label": made }),
        )
        .await?;
    changed(&mut tx, &tenant, &id, &order_id, "fulfillment.updated").await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

/// A shipment's labels, newest first, with fresh links (`orders:read`).
#[get("/api/fulfillments/{id}/labels", tag = "orders")]
async fn list_labels(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    Path(id): Path<String>,
) -> Result<Json<Vec<ShippingLabel>>> {
    let mut tx = tenant.begin_for(&db, &ORDERS_READ).await?;
    let (id, _, _) = locked(&mut tx, &tenant, &id).await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM shipping_labels WHERE tenant_id = $1 AND fulfillment_id = $2 \
         ORDER BY created_at DESC, id DESC"
    ))
    .bind(tenant.id())
    .bind(&id)
    .fetch_all(&mut *tx)
    .await?;
    let out = views(&mut tx, &tenant, &storage, rows).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Void a label not yet used (`orders:write`): the carrier cancels it.
/// One already canceled is a 409. Publishes `label.canceled`.
#[post("/api/labels/{id}/cancel", tag = "orders")]
async fn cancel_label(
    db: Db,
    tenant: Tenant,
    storage: Storage,
    carriers: State<Carriers>,
    Path(id): Path<String>,
) -> Result<Json<ShippingLabel>> {
    let id = LabelId::parse(&id).map_err(|_| Error::not_found())?;
    let mut tx = tenant.begin_for(&db, &ORDERS_WRITE).await?;
    let found: Option<(String, String, String)> = sqlx::query_as(
        "SELECT carrier_account_id, carrier_ref, status FROM shipping_labels \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let (account, carrier_ref, status) = found.ok_or_else(Error::not_found)?;
    if status == "canceled" {
        return Err(Error::conflict("the label is canceled already").with_code("label_canceled"));
    }
    carriers::cancel_label(&mut tx, &tenant, &carriers, &account, &carrier_ref).await?;
    let row: Row = sqlx::query_as(&format!(
        "UPDATE shipping_labels SET status = 'canceled' WHERE tenant_id = $1 AND id = $2 \
         RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(id.as_str())
    .fetch_one(&mut *tx)
    .await?;
    let label = views(&mut tx, &tenant, &storage, vec![row])
        .await?
        .remove(0);
    tenant.publish(&mut tx, "label.canceled", &label).await?;
    tx.commit().await?;
    Ok(Json(label))
}
