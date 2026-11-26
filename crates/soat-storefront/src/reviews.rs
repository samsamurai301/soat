//! Product reviews on the product page (feature `reviews`).

use serde_json::{json, Value};
use soat_reviews::NewReview;
use soat_tenancy::Shop;
use zapline::db::Db;
use zapline::prelude::*;

use crate::{catalog, root_of, View};

/// The product's published reviews for the page: `count`, `average` (e.g.
/// `4.5`), `stars` (rounded, 0 to 5) and `items`.
pub(crate) async fn for_page(v: &mut View, shop: &Shop, product: &str) -> Result<Value> {
    let r = soat_reviews::published(&mut v.tx, shop, product).await?;
    let average = r.average_tenths.map(|t| format!("{}.{}", t / 10, t % 10));
    let stars = r.average_tenths.map_or(0, |t| (t + 5) / 10);
    Ok(json!({
        "count": r.count,
        "average": average,
        "stars": "★".repeat(usize::try_from(stars).unwrap_or(0)),
        "items": r.reviews.iter().map(|x| json!({
            "author_name": x.author_name,
            "rating": x.rating,
            "stars": "★".repeat(usize::try_from(x.rating).unwrap_or(0)),
            "title": x.title,
            "body": x.body,
            "verified": x.verified,
            "reply": x.reply,
            "date": x.created_at.get(..10).unwrap_or_default(),
        })).collect::<Vec<_>>(),
    }))
}

/// The review form's fields.
#[model]
pub struct ReviewForm {
    pub rating: i16,
    pub title: Option<String>,
    pub body: Option<String>,
}

/// The product page's review form: the signed-in customer's review goes
/// to the shop for approval. Others are sent to sign in.
#[post("/products/{handle}/review", tag = "storefront")]
pub(crate) async fn review_form(
    db: Db,
    shop: Shop,
    storage: zapline::storage::Storage,
    headers: HeaderMap,
    Path(handle): Path<String>,
    Form(f): Form<ReviewForm>,
) -> Result<Response> {
    let root = root_of(&headers);
    let mut v = View::open(&db, &shop, &headers).await?;
    let Some(customer) = v.customer.clone() else {
        return Ok(Redirect::to(&format!("{root}/account/login")).into_response());
    };
    let Some(p) = catalog::product_by_handle(&mut v.tx, &shop, &storage, &handle).await? else {
        return v.missing(&shop, &format!("/products/{handle}")).await;
    };
    let review = NewReview {
        rating: f.rating,
        title: f.title.map(|t| t.chars().take(120).collect()),
        body: f.body.map(|b| b.chars().take(5000).collect()),
    };
    soat_reviews::write(&mut v.tx, &shop, &customer, &p.id, &review).await?;
    v.tx.commit().await?;
    Ok(Redirect::to(&format!("{root}/products/{}?reviewed=1#reviews", p.handle)).into_response())
}
