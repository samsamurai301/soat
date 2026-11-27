//! DNS TXT lookups for domain proofs (`soat_tenancy::domains`), through
//! the system's resolver configuration.

use hickory_resolver::TokioResolver;
use soat_tenancy::domains::{TxtFuture, TxtLookup};

/// Looks up TXT records with the system's resolvers.
pub(crate) struct SystemDns(TokioResolver);

impl SystemDns {
    pub(crate) fn new() -> Result<Self, String> {
        let resolver = TokioResolver::builder_tokio()
            .map_err(|e| {
                format!("the system's DNS configuration (or set SOAT_VERIFY_DOMAINS=0): {e}")
            })?
            .build();
        Ok(Self(resolver))
    }
}

impl TxtLookup for SystemDns {
    fn txt<'a>(&'a self, name: &'a str) -> TxtFuture<'a> {
        Box::pin(async move {
            match self.0.txt_lookup(name).await {
                Ok(found) => Ok(found
                    .iter()
                    .map(|txt| {
                        txt.txt_data()
                            .iter()
                            .map(|part| String::from_utf8_lossy(part).into_owned())
                            .collect::<String>()
                    })
                    .collect()),
                Err(e) if e.is_no_records_found() => Ok(Vec::new()),
                Err(e) => Err(e.to_string()),
            }
        })
    }
}
