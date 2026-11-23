//! Domain types shared by every SOAT module.
//!
//! Nothing here knows about HTTP or databases, so these types can be reused by
//! the server, plugins and SDKs alike.

mod id;
mod money;
mod tenant;

pub use id::{Id, IdError, IdKind};
pub use money::{Currency, CurrencyError, Money, MoneyError};
pub use tenant::{TenantId, TenantIdError};
