//! Product reviews.
//!
//! A signed-in customer rates a product from 1 to 5 with a title and a
//! text, once per product (writing again replaces their review and sends
//! it back to moderation). A review is marked verified when the customer
//! bought the product in a paid order. Staff publish or reject reviews and
//! may reply; the storefront shows the published ones with the product's
//! average rating.

use serde_json::json;
use soat_core::{id_kind, Permission};
use soat_customers::accounts::SignedIn;
use soat_module::Module;
use soat_tenancy::{Scope, Shop, Tenant, TenantListing, TenantTx};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::embed::Dir;
use zapline::prelude::*;

id_kind!(ReviewId, ReviewKind, "rev");

/// See product reviews, including unpublished ones.
pub const REVIEWS_READ: Permission = Permission::new(
    "reviews:read",
    "See product reviews, including unpublished ones",
)
.for_staff();
/// Publish, reject, reply to and delete product reviews.
pub const REVIEWS_MANAGE: Permission = Permission::new(
    "reviews:manage",
    "Publish, reject, reply to and delete product reviews",
)
.for_staff();

/// Most reviews one storefront page shows.
const PAGE: i64 = 50;

/// The reviews module.
pub struct Reviews;

impl Module for Reviews {
    fn name(&self) -> &'static str {
        "reviews"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        &["tenancy", "events", "catalog", "customers"]
    }

    fn migrations(&self) -> Option<Dir> {
        Some(zapline::embed!("migrations"))
    }

    fn permissions(&self) -> &'static [Permission] {
        &[REVIEWS_READ, REVIEWS_MANAGE]
    }

    fn personal_data(&self) -> &'static [soat_module::PersonalData] {
        &[soat_module::PersonalData {
            table: "product_reviews",
            matches: "customer_id = $2",
            export: &[
                "product_id",
                "author_name",
                "rating",
                "title",
                "body",
                "status",
                "created_at",
            ],
            erase: soat_module::Erase::Delete,
        }]
    }

    fn mount(&self, app: App) -> App {
        app.mount((
            list_reviews,
            get_review,
            update_review,
            delete_review,
            write_review,
            product_reviews,
        ))
    }
}

/// A product review.
#[model]
#[derive(sqlx::FromRow)]
pub struct Review {
    /// e.g. `rev_01jdx3k9q8m2v4t6w7y0z1a2b3`.
    pub id: String,
    pub product_id: String,
    /// Null once the customer is deleted.
    pub customer_id: Option<String>,
    /// As shown: the customer's first name and last initial.
    pub author_name: String,
    /// 1 to 5.
    pub rating: i16,
    pub title: String,
    pub body: String,
    /// The author bought the product in a paid order.
    pub verified: bool,
    /// `pending`, `published` or `rejected`.
    pub status: String,
    /// The shop's public answer; empty for none.
    pub reply: String,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "id, product_id, customer_id, author_name, rating, title, body, verified, \
     status, reply, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at, \
     to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated_at";

/// A review as buyers see it.
#[model]
pub struct PublicReview {
    pub id: String,
    pub author_name: String,
    pub rating: i16,
    pub title: String,
    pub body: String,
    pub verified: bool,
    pub reply: String,
    pub created_at: String,
}

impl From<Review> for PublicReview {
    fn from(r: Review) -> Self {
        Self {
            id: r.id,
            author_name: r.author_name,
            rating: r.rating,
            title: r.title,
            body: r.body,
            verified: r.verified,
            reply: r.reply,
            created_at: r.created_at,
        }
    }
}

/// A product's published reviews and their summary.
#[model]
pub struct ProductReviews {
    /// Published reviews.
    pub count: i64,
    /// Their average rating, to one decimal in tenths (e.g. 43 is 4.3);
    /// null without reviews.
    pub average_tenths: Option<i64>,
    /// Newest first, up to 50.
    pub reviews: Vec<PublicReview>,
}

/// Body of writing a review.
#[model]
pub struct NewReview {
    #[validate(range(min = 1, max = 5))]
    pub rating: i16,
    #[validate(length(max = 120))]
    pub title: Option<String>,
    #[validate(length(max = 5000))]
    pub body: Option<String>,
}

/// Body of a staff change of a review.
#[model]
pub struct ReviewChange {
    /// `pending`, `published` or `rejected`.
    pub status: Option<String>,
    /// The shop's public answer; empty removes it.
    #[validate(length(max = 5000))]
    pub reply: Option<String>,
}

async fn load(conn: &mut PgConnection, scope: &impl Scope, id: &str) -> Result<Review> {
    let id = ReviewId::parse(id).map_err(|_| Error::not_found())?;
    sqlx::query_as::<_, Review>(&format!(
        "SELECT {COLUMNS} FROM product_reviews WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(scope.tenant_id())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?
    .ok_or_else(Error::not_found)
}

/// The shop's reviews (`reviews:read`), newest first; filter by `status`,
/// `product_id`, `rating`, search in title and text.
#[get("/api/reviews", tag = "reviews")]
async fn list_reviews(db: Db, tenant: Tenant, params: ListParams) -> Result<Json<Paged<Review>>> {
    let mut tx = tenant.begin_for(&db, &REVIEWS_READ).await?;
    let page = TenantListing::new("product_reviews", COLUMNS)
        .sortable(["created_at", "rating"])
        .default_sort("-created_at")
        .search(["title", "body", "author_name"])
        .filter_text("status")
        .filter_text("product_id")
        .filter_text("customer_id")
        .fetch(&mut tx, &params)
        .await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// A review (`reviews:read`).
#[get("/api/reviews/{id}", tag = "reviews")]
async fn get_review(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<Json<Review>> {
    let mut tx = tenant.begin_for(&db, &REVIEWS_READ).await?;
    let r = load(&mut tx, &tenant, &id).await?;
    tx.commit().await?;
    Ok(Json(r))
}

/// Publish or reject a review, or reply to it (`reviews:manage`).
/// Publishes `review.updated`.
#[patch("/api/reviews/{id}", tag = "reviews")]
async fn update_review(
    db: Db,
    tenant: Tenant,
    Path(id): Path<String>,
    Valid(Json(c)): Valid<Json<ReviewChange>>,
) -> Result<Json<Review>> {
    if let Some(s) = c.status.as_deref() {
        if !matches!(s, "pending" | "published" | "rejected") {
            return Err(
                Error::unprocessable(format!("{s} is not pending, published or rejected"))
                    .with_code("invalid_status"),
            );
        }
    }
    let mut tx = tenant.begin_for(&db, &REVIEWS_MANAGE).await?;
    let old = load(&mut tx, &tenant, &id).await?;
    let r = sqlx::query_as::<_, Review>(&format!(
        "UPDATE product_reviews SET status = COALESCE($3, status), reply = COALESCE($4, reply), \
         updated_at = now() WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
    ))
    .bind(tenant.id())
    .bind(&old.id)
    .bind(c.status.as_deref())
    .bind(c.reply.as_deref().map(str::trim))
    .fetch_one(&mut *tx)
    .await?;
    tenant.publish(&mut tx, "review.updated", &r).await?;
    tx.commit().await?;
    Ok(Json(r))
}

/// Delete a review (`reviews:manage`). Publishes `review.deleted`.
#[delete("/api/reviews/{id}", tag = "reviews", status = 204)]
async fn delete_review(db: Db, tenant: Tenant, Path(id): Path<String>) -> Result<StatusCode> {
    let mut tx = tenant.begin_for(&db, &REVIEWS_MANAGE).await?;
    let r = load(&mut tx, &tenant, &id).await?;
    sqlx::query("DELETE FROM product_reviews WHERE tenant_id = $1 AND id = $2")
        .bind(tenant.id())
        .bind(&r.id)
        .execute(&mut *tx)
        .await?;
    tenant
        .publish(&mut tx, "review.deleted", &json!({ "id": r.id }))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The signed-in customer reviews a product; writing again replaces
/// their review. It waits for the shop to publish it. Publishes
/// `review.created` (or `review.updated`).
#[put("/api/storefront/products/{id}/review", tag = "storefront")]
async fn write_review(
    db: Db,
    signed_in: SignedIn,
    Path(product): Path<String>,
    Valid(Json(n)): Valid<Json<NewReview>>,
) -> Result<Json<Review>> {
    let (mut tx, customer) = signed_in.begin(&db).await?;
    let r = write(&mut tx, &signed_in.shop, &customer, &product, &n).await?;
    tx.commit().await?;
    Ok(Json(r))
}

/// Customer `customer`'s review of `product`, written or replaced and
/// back in moderation. Publishes `review.created` or `review.updated`.
/// Run it in the shop's transaction.
///
/// # Errors
///
/// 404 for a product the shop does not have; 422 for a rating outside 1
/// to 5; database errors.
pub async fn write(
    tx: &mut TenantTx,
    shop: &Shop,
    customer: &str,
    product: &str,
    n: &NewReview,
) -> Result<Review> {
    if !(1..=5).contains(&n.rating) {
        return Err(Error::unprocessable("a rating is 1 to 5").with_code("invalid_rating"));
    }
    if soat_catalog::product_tags(tx, shop, product)
        .await?
        .is_none()
    {
        return Err(Error::not_found());
    }
    let name: (String, String) = sqlx::query_as(
        "SELECT first_name, last_name FROM customers WHERE tenant_id = $1 AND id = $2",
    )
    .bind(shop.id())
    .bind(customer)
    .fetch_one(&mut **tx)
    .await?;
    let author = author_name(&name.0, &name.1);
    let (verified,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM orders o JOIN order_lines l ON l.order_id = o.id \
         WHERE o.tenant_id = $1 AND o.customer_id = $2 AND l.product_id = $3 \
         AND o.financial_status IN ('paid', 'partially_refunded'))",
    )
    .bind(shop.id())
    .bind(customer)
    .bind(product)
    .fetch_one(&mut **tx)
    .await?;
    let row = sqlx::query_as::<_, ReviewRow>(&format!(
        "INSERT INTO product_reviews (id, tenant_id, product_id, customer_id, author_name, \
         rating, title, body, verified) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (tenant_id, product_id, customer_id) DO UPDATE SET \
         author_name = EXCLUDED.author_name, rating = EXCLUDED.rating, \
         title = EXCLUDED.title, body = EXCLUDED.body, verified = EXCLUDED.verified, \
         status = 'pending', updated_at = now() \
         RETURNING {COLUMNS}, (xmax = 0) AS created"
    ))
    .bind(ReviewId::new().as_str())
    .bind(shop.id())
    .bind(product)
    .bind(customer)
    .bind(&author)
    .bind(n.rating)
    .bind(n.title.as_deref().unwrap_or("").trim())
    .bind(n.body.as_deref().unwrap_or("").trim())
    .bind(verified)
    .fetch_one(&mut **tx)
    .await?;
    let kind = if row.created {
        "review.created"
    } else {
        "review.updated"
    };
    shop.publish(tx, None, kind, &row.review).await?;
    Ok(row.review)
}

#[derive(sqlx::FromRow)]
struct ReviewRow {
    #[sqlx(flatten)]
    review: Review,
    created: bool,
}

/// A product's published reviews, newest first, with their count and
/// average rating.
#[get("/api/storefront/products/{id}/reviews", tag = "storefront")]
async fn product_reviews(
    db: Db,
    shop: Shop,
    Path(product): Path<String>,
) -> Result<Json<ProductReviews>> {
    let mut tx = shop.begin(&db).await?;
    let out = published(&mut tx, &shop, &product).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Product `product`'s published reviews and their summary, for pages.
///
/// # Errors
///
/// Database errors.
pub async fn published(
    conn: &mut PgConnection,
    scope: &impl Scope,
    product: &str,
) -> Result<ProductReviews> {
    let (count, average): (i64, Option<i64>) = sqlx::query_as(
        "SELECT COUNT(*), ROUND(AVG(rating) * 10)::BIGINT FROM product_reviews \
         WHERE tenant_id = $1 AND product_id = $2 AND status = 'published'",
    )
    .bind(scope.tenant_id())
    .bind(product)
    .fetch_one(&mut *conn)
    .await?;
    let reviews = sqlx::query_as::<_, Review>(&format!(
        "SELECT {COLUMNS} FROM product_reviews WHERE tenant_id = $1 AND product_id = $2 \
         AND status = 'published' ORDER BY created_at DESC, id DESC LIMIT {PAGE}"
    ))
    .bind(scope.tenant_id())
    .bind(product)
    .fetch_all(&mut *conn)
    .await?;
    Ok(ProductReviews {
        count,
        average_tenths: average,
        reviews: reviews.into_iter().map(PublicReview::from).collect(),
    })
}

/// "Ada L." from a first and last name; "A customer" without a first name.
fn author_name(first: &str, last: &str) -> String {
    let first = first.trim();
    let initial = last
        .trim()
        .chars()
        .next()
        .map(|c| format!(" {}.", c.to_uppercase()));
    match (first.is_empty(), initial) {
        (false, Some(i)) => format!("{first}{i}"),
        (false, None) => first.to_owned(),
        (true, _) => "A customer".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::author_name;

    #[test]
    fn names() {
        assert_eq!(author_name("Ada", "lovelace"), "Ada L.");
        assert_eq!(author_name(" Ada ", ""), "Ada");
        assert_eq!(author_name("", ""), "A customer");
        assert_eq!(author_name("", "Lovelace"), "A customer");
    }
}
