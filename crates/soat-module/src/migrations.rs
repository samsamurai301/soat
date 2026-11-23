//! Merges each module's migration directory into the one directory Zapline
//! runs.
//!
//! Zapline's `Database::migrations` takes a single directory. Until Zapline
//! accepts several (planned as a Zapline addition), the files are copied into
//! one in-memory directory at startup. This runs once per process.

use std::collections::HashMap;

use zapline::embed::{content_hash, Dir, EmbeddedFile};

use crate::ModuleError;

/// Joins `(module, directory)` pairs into one directory, rejecting two
/// modules that use the same migration version.
pub fn merge_migrations(dirs: &[(&'static str, Dir)]) -> Result<Dir, ModuleError> {
    let mut owners: HashMap<String, &'static str> = HashMap::new();
    let mut files = Vec::new();
    for (module, dir) in dirs {
        if !dir.exists() {
            continue;
        }
        let unreadable = |e: std::io::Error| ModuleError::Unreadable {
            module,
            error: e.to_string(),
        };
        for name in dir.files().map_err(unreadable)? {
            // Zapline, like sqlx, reads only the top level.
            if name.contains('/') || !name.ends_with(".sql") {
                continue;
            }
            let Some((version, _)) = name.split_once('_') else {
                continue;
            };
            match owners.get(version) {
                Some(first) if first != module => {
                    return Err(ModuleError::DuplicateMigration {
                        version: version.to_owned(),
                        first,
                        second: module,
                    })
                }
                _ => {
                    owners.insert(version.to_owned(), module);
                }
            }
            let bytes = dir.read(&name).map_err(unreadable)?.into_owned();
            files.push(EmbeddedFile {
                hash: leak(content_hash(&bytes)),
                path: leak(name),
                bytes: Vec::leak(bytes),
            });
        }
    }
    let files: &'static [EmbeddedFile] = Vec::leak(files);
    // A root that is not a directory keeps every read in memory.
    Ok(Dir::__from_embedded("<soat modules>", files))
}

fn leak(s: String) -> &'static str {
    String::leak(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_and_rejects_shared_versions() {
        let tenancy = zapline::embed!("tests/fixtures/a");
        let catalog = zapline::embed!("tests/fixtures/b");
        let merged = merge_migrations(&[("a", tenancy.clone()), ("b", catalog)]).unwrap();
        assert_eq!(
            merged.files().unwrap(),
            [
                "1_one.down.sql",
                "1_one.up.sql",
                "2_two.down.sql",
                "2_two.up.sql"
            ]
        );
        assert_eq!(
            merged.read_to_string("2_two.up.sql").unwrap().trim(),
            "SELECT 2;"
        );

        let clash = merge_migrations(&[("a", tenancy.clone()), ("again", tenancy)]);
        assert_eq!(
            clash.err(),
            Some(ModuleError::DuplicateMigration {
                version: "1".into(),
                first: "a",
                second: "again"
            })
        );
    }
}
