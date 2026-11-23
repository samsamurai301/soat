//! The catalog as buyers see it: active products and collections, by
//! handle, with no sign-in.

use soat_files::FileView;
use soat_tenancy::{Scope, Shop, TenantListing};
use zapline::db::sqlx::PgConnection;
use zapline::db::{sqlx, Db, ListParams, Paged};
use zapline::prelude::*;
use zapline::storage::Storage;

use crate::variants::{load_options, load_variants, ProductOption};

pub(crate) fn mount(app: App) -> App {
    app.mount((shop_products, shop_product, shop_collection))
}

/// A picture of a product.
#[model]
pub struct ShopImage {
    pub url: String,
    pub alt: String,
}

impl From<FileView> for ShopImage {
    fn from(f: FileView) -> Self {
        Self {
            url: f.url,
            alt: f.alt,
        }
    }
}

/// A product in a list: enough for a card.
#[model]
#[derive(sqlx::FromRow)]
pub struct ProductCard {
    pub id: String,
    pub handle: String,
    pub title: String,
    pub price_minor: i64,
    pub currency: String,
    pub tags: Vec<String>,
    /// The first picture, if any.
    #[sqlx(skip)]
    pub image: Option<ShopImage>,
}

/// A variant buyers can choose.
#[model]
pub struct ShopVariant {
    pub id: String,
    /// e.g. `M / Red`.
    pub title: String,
    pub options: Vec<String>,
    pub price_minor: i64,
    pub currency: String,
}

/// A product page's worth.
#[model]
pub struct ShopProduct {
    pub id: String,
    pub handle: String,
    pub title: String,
    /// Markdown.
    pub description: String,
    /// Empty: use the title.
    pub seo_title: String,
    /// Empty: use the start of the description.
    pub seo_description: String,
    pub price_minor: i64,
    pub currency: String,
    pub tags: Vec<String>,
    pub images: Vec<ShopImage>,
    pub options: Vec<ProductOption>,
    /// Empty when the product itself is what is sold.
    pub variants: Vec<ShopVariant>,
}

/// A collection and its first page of products.
#[model]
pub struct ShopCollection {
    pub id: String,
    pub handle: String,
    pub title: String,
    /// Markdown.
    pub description: String,
    /// Empty: use the title.
    pub seo_title: String,
    /// Empty: use the start of the description.
    pub seo_description: String,
}

#[derive(sqlx::FromRow)]
struct ProductRow {
    id: String,
    handle: String,
    title: String,
    description: String,
    seo_title: String,
    seo_description: String,
    price_minor: i64,
    currency: String,
    tags: Vec<String>,
}

const CARD_COLUMNS: &str = "id, handle, title, price_minor, currency, tags";

/// The first picture of each of `ids`.
pub async fn first_images(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    cards: &mut [ProductCard],
) -> Result<()> {
    let ids: Vec<String> = cards.iter().map(|c| c.id.clone()).collect();
    let firsts: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (product_id) product_id, file_id FROM product_media \
         WHERE tenant_id = $1 AND product_id = ANY($2) ORDER BY product_id, position",
    )
    .bind(scope.tenant_id())
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let files: Vec<String> = firsts.iter().map(|(_, f)| f.clone()).collect();
    let views = soat_files::views(conn, scope, storage, &files).await?;
    for card in cards.iter_mut() {
        let file = firsts.iter().find(|(p, _)| *p == card.id).map(|(_, f)| f);
        card.image = file
            .and_then(|f| views.iter().find(|v| &v.id == f))
            .map(|v| ShopImage {
                url: v.url.clone(),
                alt: v.alt.clone(),
            });
    }
    Ok(())
}

/// Active products, a page of cards. `?q=` searches titles, `?tag=`
/// filters, `?collection=<handle>` keeps one collection's products,
/// `?sort=` by `title` or `price_minor` (default newest first).
pub async fn product_cards(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    params: &ListParams,
) -> Result<Paged<ProductCard>> {
    let mut listing = TenantListing::new("products", CARD_COLUMNS)
        .scope_text("status", "active")
        .sortable(["id", "title", "price_minor"])
        .default_sort("-id")
        .search(["title"])
        .filter_array("tag", "tags")
        .page_size(24, 100);
    if let Some(handle) = params.get("collection") {
        listing = listing.scope_subquery(
            "id IN (SELECT cp.product_id FROM collection_products cp \
             JOIN collections c ON c.id = cp.collection_id WHERE c.handle = ANY(",
            vec![handle.to_owned()],
            "))",
        );
    }
    let mut page: Paged<ProductCard> = listing.fetch(conn, params).await?;
    first_images(conn, scope, storage, &mut page.items).await?;
    Ok(page)
}

/// An active product by handle.
pub async fn product_by_handle(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    handle: &str,
) -> Result<Option<ShopProduct>> {
    let row: Option<ProductRow> = sqlx::query_as(
        "SELECT id, handle, title, description, seo_title, seo_description, price_minor, \
         currency, tags FROM products WHERE tenant_id = $1 AND handle = $2 AND status = 'active'",
    )
    .bind(scope.tenant_id())
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id = row.id;
    let media: Vec<(String,)> = sqlx::query_as(
        "SELECT file_id FROM product_media WHERE tenant_id = $1 AND product_id = $2 \
         ORDER BY position",
    )
    .bind(scope.tenant_id())
    .bind(&id)
    .fetch_all(&mut *conn)
    .await?;
    let files: Vec<String> = media.into_iter().map(|(f,)| f).collect();
    let images = soat_files::views(conn, scope, storage, &files)
        .await?
        .into_iter()
        .map(ShopImage::from)
        .collect();
    let options = load_options(conn, scope, &id)
        .await?
        .into_iter()
        .map(|(name, values)| ProductOption { name, values })
        .collect();
    let variants = load_variants(conn, scope, &id)
        .await?
        .into_iter()
        .map(|v| ShopVariant {
            id: v.id,
            title: v.title,
            options: v.options,
            price_minor: v.price_minor,
            currency: v.currency,
        })
        .collect();
    Ok(Some(ShopProduct {
        id,
        handle: row.handle,
        title: row.title,
        description: row.description,
        seo_title: row.seo_title,
        seo_description: row.seo_description,
        price_minor: row.price_minor,
        currency: row.currency,
        tags: row.tags,
        images,
        options,
        variants,
    }))
}

/// A collection by handle.
pub async fn collection_by_handle(
    conn: &mut PgConnection,
    scope: &impl Scope,
    handle: &str,
) -> Result<Option<ShopCollection>> {
    type Row = (String, String, String, String, String, String);
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, handle, title, description, seo_title, seo_description FROM collections \
         WHERE tenant_id = $1 AND handle = $2",
    )
    .bind(scope.tenant_id())
    .bind(handle)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(
        |(id, handle, title, description, seo_title, seo_description)| ShopCollection {
            id,
            handle,
            title,
            description,
            seo_title,
            seo_description,
        },
    ))
}

/// The currency most of the shop's active products are priced in.
pub async fn main_currency(conn: &mut PgConnection, scope: &impl Scope) -> Result<Option<String>> {
    let row: (Option<String>,) = sqlx::query_as(
        "SELECT mode() WITHIN GROUP (ORDER BY currency) FROM products \
         WHERE tenant_id = $1 AND status = 'active'",
    )
    .bind(scope.tenant_id())
    .fetch_one(conn)
    .await?;
    Ok(row.0)
}

/// Most addresses one sitemap lists.
pub const SITEMAP_LIMIT: i64 = 50_000;

/// The handles of the shop's active products and of its collections, for a
/// sitemap; together at most [`SITEMAP_LIMIT`].
pub async fn sitemap_handles(
    conn: &mut PgConnection,
    scope: &impl Scope,
) -> Result<(Vec<String>, Vec<String>)> {
    let products: Vec<(String,)> = sqlx::query_as(
        "SELECT handle FROM products WHERE tenant_id = $1 AND status = 'active' \
         ORDER BY handle LIMIT $2",
    )
    .bind(scope.tenant_id())
    .bind(SITEMAP_LIMIT)
    .fetch_all(&mut *conn)
    .await?;
    let left = SITEMAP_LIMIT - products.len() as i64;
    let collections: Vec<(String,)> = sqlx::query_as(
        "SELECT handle FROM collections WHERE tenant_id = $1 ORDER BY handle LIMIT $2",
    )
    .bind(scope.tenant_id())
    .bind(left)
    .fetch_all(&mut *conn)
    .await?;
    Ok((
        products.into_iter().map(|(h,)| h).collect(),
        collections.into_iter().map(|(h,)| h).collect(),
    ))
}

/// The shop's active products (no sign-in). `?q=` searches titles,
/// `?tag=` filters, `?collection=<handle>` keeps one collection, `?sort=`
/// by `title` or `price_minor`; 24 a page, at most 100.
#[get("/api/storefront/products", tag = "storefront")]
async fn shop_products(
    db: Db,
    shop: Shop,
    storage: Storage,
    params: ListParams,
) -> Result<Json<Paged<ProductCard>>> {
    let mut tx = shop.begin(&db).await?;
    let page = product_cards(&mut tx, &shop, &storage, &params).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// An active product by handle, with pictures, options and variants (no
/// sign-in). Drafts and archived products are a 404.
#[get("/api/storefront/products/{handle}", tag = "storefront")]
async fn shop_product(
    db: Db,
    shop: Shop,
    storage: Storage,
    Path(handle): Path<String>,
) -> Result<Json<ShopProduct>> {
    let mut tx = shop.begin(&db).await?;
    let p = product_by_handle(&mut tx, &shop, &storage, &handle).await?;
    tx.commit().await?;
    p.map(Json).ok_or_else(Error::not_found)
}

/// A collection by handle (no sign-in). List its products with
/// `GET /api/storefront/products?collection=<handle>`.
#[get("/api/storefront/collections/{handle}", tag = "storefront")]
async fn shop_collection(
    db: Db,
    shop: Shop,
    Path(handle): Path<String>,
) -> Result<Json<ShopCollection>> {
    let mut tx = shop.begin(&db).await?;
    let c = collection_by_handle(&mut tx, &shop, &handle).await?;
    tx.commit().await?;
    c.map(Json).ok_or_else(Error::not_found)
}

/// Active products among `ids`, in that order, as cards.
pub async fn cards_by_ids(
    conn: &mut PgConnection,
    scope: &impl Scope,
    storage: &Storage,
    ids: &[String],
) -> Result<Vec<ProductCard>> {
    let rows: Vec<ProductCard> = sqlx::query_as(&format!(
        "SELECT {CARD_COLUMNS} FROM products WHERE tenant_id = $1 AND id = ANY($2) \
         AND status = 'active'"
    ))
    .bind(scope.tenant_id())
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut rows: Vec<Option<ProductCard>> = rows.into_iter().map(Some).collect();
    let mut cards: Vec<ProductCard> = ids
        .iter()
        .filter_map(|id| {
            rows.iter_mut()
                .find(|r| r.as_ref().is_some_and(|r| &r.id == id))
                .and_then(Option::take)
        })
        .collect();
    first_images(conn, scope, storage, &mut cards).await?;
    Ok(cards)
}

/// The handle of collection `id`, if the shop has it.
pub async fn collection_handle(
    conn: &mut PgConnection,
    scope: &impl Scope,
    id: &str,
) -> Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT handle FROM collections WHERE tenant_id = $1 AND id = $2")
            .bind(scope.tenant_id())
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|(h,)| h))
}
