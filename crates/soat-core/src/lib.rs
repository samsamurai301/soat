//! Domain types shared by every SOAT module.
//!
//! Nothing here knows about HTTP or databases, so these types can be reused by
//! the server, plugins and SDKs alike.

pub mod address;
pub mod content;
pub mod content_types;
pub mod csv;
pub mod discount;
mod domain;
pub mod feeds;
pub mod fields;
pub mod handle;
pub mod hooks;
pub mod i18n;
mod id;
pub mod import;
pub mod invoice;
pub mod markdown;
pub mod media;
mod money;
pub mod options;
pub mod order;
pub mod payment;
mod permission;
pub mod rules;
pub mod secret;
pub mod shopify;
pub mod shopware;
pub mod tags;
pub mod tax;
pub mod template;
mod tenant;
pub mod woocommerce;

pub use domain::{Domain, DomainError};
pub use id::{Id, IdError, IdKind};
pub use money::{convert, Currency, CurrencyError, Money, MoneyError, Rounding};
pub use permission::{is_permission_name, Permission};
pub use tenant::{TenantId, TenantIdError};
