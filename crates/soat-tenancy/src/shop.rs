//! The shop's profile: its name, the country it sells from and the currency
//! it prices in.

use soat_core::address::is_country_code;
use soat_core::{Currency, Permission};
use zapline::db::{sqlx, Db};
use zapline::prelude::*;

use crate::Tenant;

/// See the shop's profile.
pub const SHOP_READ: Permission =
    Permission::new("shop:read", "See the shop's profile").for_staff();
/// Change the shop's name, country and currency.
pub const SHOP_MANAGE: Permission = Permission::new(
    "shop:manage",
    "Change the shop's name, country and currency",
);

/// The shop's profile.
#[model]
#[derive(sqlx::FromRow)]
pub struct Shop {
    pub id: String,
    pub name: String,
    /// ISO 3166-1 alpha-2, such as `DE`; empty until set.
    pub country: Option<String>,
    /// ISO 4217, such as `EUR`; empty until set.
    pub currency: Option<String>,
}

/// A change to the profile; fields left out stay as they are.
#[model]
pub struct ShopChange {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    pub country: Option<String>,
    pub currency: Option<String>,
}

pub(crate) fn mount(app: App) -> App {
    app.mount((get_shop, update_shop))
}

async fn load(conn: &mut sqlx::PgConnection, id: &str) -> Result<Shop> {
    sqlx::query_as::<_, Shop>("SELECT id, name, country, currency FROM tenants WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(Error::not_found)
}

/// The shop's profile (`shop:read`).
#[get("/api/shop", tag = "shop")]
async fn get_shop(db: Db, tenant: Tenant) -> Result<Json<Shop>> {
    let mut tx = tenant.begin_for(&db, &SHOP_READ).await?;
    let shop = load(&mut tx, tenant.id()).await?;
    tx.commit().await?;
    Ok(Json(shop))
}

/// Change the shop's name, country or currency (`shop:manage`). Publishes
/// `shop.updated`. A country is two capital letters, a currency one SOAT
/// knows (422 otherwise).
#[patch("/api/shop", tag = "shop")]
async fn update_shop(
    db: Db,
    tenant: Tenant,
    Valid(Json(c)): Valid<Json<ShopChange>>,
) -> Result<Json<Shop>> {
    if let Some(country) = &c.country {
        if !is_country_code(country) {
            return Err(
                Error::unprocessable("country is a code such as DE").with_code("invalid_country")
            );
        }
    }
    let currency = match &c.currency {
        Some(code) => Some(
            Currency::parse(code)
                .map_err(|e| Error::unprocessable(e.to_string()).with_code("invalid_currency"))?
                .to_string(),
        ),
        None => None,
    };
    let name = c.name.as_deref().map(str::trim);
    if name == Some("") {
        return Err(Error::unprocessable("name cannot be blank"));
    }
    let mut tx = tenant.begin_for(&db, &SHOP_MANAGE).await?;
    sqlx::query(
        "UPDATE tenants SET name = COALESCE($2, name), country = COALESCE($3, country), \
         currency = COALESCE($4, currency) WHERE id = $1",
    )
    .bind(tenant.id())
    .bind(name)
    .bind(&c.country)
    .bind(&currency)
    .execute(&mut *tx)
    .await?;
    let shop = load(&mut tx, tenant.id()).await?;
    tenant.publish(&mut tx, "shop.updated", &shop).await?;
    tx.commit().await?;
    Ok(Json(shop))
}
