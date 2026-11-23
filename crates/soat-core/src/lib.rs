//! Domain types shared by every SOAT module.
//!
//! Nothing here knows about HTTP or databases, so these types can be reused by
//! the server, plugins and SDKs alike.

mod domain;
mod id;
mod money;
mod permission;
mod tenant;

pub use domain::{Domain, DomainError};
pub use id::{Id, IdError, IdKind};
pub use money::{Currency, CurrencyError, Money, MoneyError};
pub use permission::{is_permission_name, Permission};
pub use tenant::{TenantId, TenantIdError};
