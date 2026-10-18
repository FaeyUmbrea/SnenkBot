use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::schema::DescribeConfig;

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct StoredConfig {
    pub definition: String,
    pub version: u32,
    pub data: Value,
}

pub type MigrationFunction = fn(Value) -> Result<Value, ManualMigrationError>;

#[derive(Clone, Copy, Debug)]
pub struct MigrationStep {
    pub from: u32,
    pub to: u32,
    pub migrate: MigrationFunction,
}

#[derive(Debug)]
pub struct LoadedConfig<T> {
    pub value: T,
    pub stored: StoredConfig,
    pub migrated: bool,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ManualMigrationError {
    message: String,
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("stored definition `{stored}` does not match compiled definition `{compiled}`")]
    DefinitionMismatch {
        stored: String,
        compiled: &'static str,
    },
    #[error("stored version {stored} is newer than compiled version {compiled}")]
    FutureVersion { stored: u32, compiled: u32 },
    #[error("multiple explicit migrations start at version {0}")]
    AmbiguousPath(u32),
    #[error("migration from version {from} has invalid destination version {to}")]
    InvalidDestination { from: u32, to: u32 },
    #[error("automatic migration requires a JSON object, but stored data is {0}")]
    StoredDataNotObject(&'static str),
    #[error("the default value for `{0}` did not serialize as a JSON object")]
    DefaultNotObject(&'static str),
    #[error("the default value for field `{field}` is missing from `{definition}`")]
    MissingDefault {
        definition: &'static str,
        field: &'static str,
    },
    #[error("failed to serialize defaults for `{definition}`: {source}")]
    SerializeDefaults {
        definition: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize `{definition}`: {source}")]
    SerializeConfig {
        definition: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("explicit migration from version {from} to {to} failed: {source}")]
    Explicit {
        from: u32,
        to: u32,
        #[source]
        source: ManualMigrationError,
    },
    #[error("migrated data for `{definition}` is invalid: {source}")]
    Deserialize {
        definition: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

impl ManualMigrationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl StoredConfig {
    pub fn from_current<T>(value: &T) -> Result<Self, MigrationError>
    where
        T: DescribeConfig,
    {
        let schema = T::SCHEMA;
        let data =
            serde_json::to_value(value).map_err(|source| MigrationError::SerializeConfig {
                definition: schema.id,
                source,
            })?;

        Ok(Self {
            definition: schema.id.to_owned(),
            version: schema.version,
            data,
        })
    }
}

pub fn load_config<T>(
    original: &StoredConfig,
    explicit_steps: &[MigrationStep],
) -> Result<LoadedConfig<T>, MigrationError>
where
    T: DescribeConfig + DeserializeOwned,
{
    let schema = T::SCHEMA;
    if original.definition != schema.id {
        return Err(MigrationError::DefinitionMismatch {
            stored: original.definition.clone(),
            compiled: schema.id,
        });
    }
    if original.version > schema.version {
        return Err(MigrationError::FutureVersion {
            stored: original.version,
            compiled: schema.version,
        });
    }

    let mut stored = original.clone();
    while stored.version < schema.version {
        let mut candidates = explicit_steps
            .iter()
            .filter(|step| step.from == stored.version);
        let explicit = candidates.next();
        if candidates.next().is_some() {
            return Err(MigrationError::AmbiguousPath(stored.version));
        }

        if let Some(step) = explicit {
            if step.to <= step.from || step.to > schema.version {
                return Err(MigrationError::InvalidDestination {
                    from: step.from,
                    to: step.to,
                });
            }
            stored.data =
                (step.migrate)(stored.data).map_err(|source| MigrationError::Explicit {
                    from: step.from,
                    to: step.to,
                    source,
                })?;
            stored.version = step.to;
        } else {
            let next_version = stored.version + 1;
            add_missing_defaults::<T>(&mut stored.data, next_version)?;
            stored.version = next_version;
        }
    }

    let value = serde_json::from_value(stored.data.clone()).map_err(|source| {
        MigrationError::Deserialize {
            definition: schema.id,
            source,
        }
    })?;

    Ok(LoadedConfig {
        value,
        migrated: stored != *original,
        stored,
    })
}

fn add_missing_defaults<T>(data: &mut Value, version: u32) -> Result<(), MigrationError>
where
    T: DescribeConfig,
{
    let schema = T::SCHEMA;
    let defaults =
        serde_json::to_value(T::default()).map_err(|source| MigrationError::SerializeDefaults {
            definition: schema.id,
            source,
        })?;
    let defaults = defaults
        .as_object()
        .ok_or(MigrationError::DefaultNotObject(schema.id))?;
    let data_kind = json_kind(data);
    let data = data
        .as_object_mut()
        .ok_or(MigrationError::StoredDataNotObject(data_kind))?;

    for field in schema
        .fields
        .iter()
        .filter(|field| field.introduced_in == version)
        .filter(|field| field.kind != crate::schema::ConfigFieldKind::Secret)
    {
        if data.contains_key(field.id) {
            continue;
        }
        if let Some(default) = defaults.get(field.id) {
            data.insert(field.id.to_owned(), default.clone());
        } else if field.required {
            return Err(MigrationError::MissingDefault {
                definition: schema.id,
                field: field.id,
            });
        }
    }

    Ok(())
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::ConfigSchema;

    #[derive(Debug, Default, Deserialize, PartialEq, Serialize, ConfigSchema)]
    #[config(id = "test.settings", version = 2, title = "Test")]
    struct TestSettings {
        #[config(id = "name", label = "Name", introduced = 1)]
        name: String,
        #[config(id = "enabled", label = "Enabled", introduced = 2)]
        enabled: bool,
    }

    #[test]
    fn additive_migration_inserts_defaults_before_deserializing() {
        let original = StoredConfig {
            definition: "test.settings".into(),
            version: 1,
            data: json!({ "name": "Snenk" }),
        };

        let loaded = load_config::<TestSettings>(&original, &[]).unwrap();

        assert_eq!(
            loaded.value,
            TestSettings {
                name: "Snenk".into(),
                enabled: false,
            }
        );
        assert_eq!(loaded.stored.version, 2);
        assert_eq!(
            loaded.stored.data,
            json!({ "name": "Snenk", "enabled": false })
        );
        assert!(loaded.migrated);
        assert_eq!(original.version, 1);
        assert_eq!(original.data, json!({ "name": "Snenk" }));
    }

    #[test]
    fn additive_migration_accepts_an_omitted_optional_default() {
        #[derive(Debug, Default, Deserialize, PartialEq, Serialize, ConfigSchema)]
        #[config(id = "test.optional", version = 2, title = "Optional")]
        struct OptionalSettings {
            #[config(id = "name", label = "Name", introduced = 1)]
            name: String,
            #[config(id = "reply", label = "Reply", introduced = 2)]
            #[serde(skip_serializing_if = "Option::is_none")]
            reply: Option<String>,
        }

        let original = StoredConfig {
            definition: "test.optional".into(),
            version: 1,
            data: json!({ "name": "Snenk" }),
        };
        let loaded = load_config::<OptionalSettings>(&original, &[]).unwrap();
        assert_eq!(loaded.value.reply, None);
        assert_eq!(loaded.stored.data, original.data);
        assert_eq!(loaded.stored.version, 2);
    }

    #[test]
    fn current_but_incomplete_data_is_not_silently_defaulted() {
        let original = StoredConfig {
            definition: "test.settings".into(),
            version: 2,
            data: json!({ "name": "Snenk" }),
        };

        assert!(matches!(
            load_config::<TestSettings>(&original, &[]),
            Err(MigrationError::Deserialize { .. })
        ));
    }

    #[test]
    fn explicit_migration_replaces_the_automatic_step() {
        fn migrate(mut data: Value) -> Result<Value, ManualMigrationError> {
            let object = data
                .as_object_mut()
                .ok_or_else(|| ManualMigrationError::new("expected an object"))?;
            object.insert("enabled".into(), Value::Bool(true));
            Ok(data)
        }

        let original = StoredConfig {
            definition: "test.settings".into(),
            version: 1,
            data: json!({ "name": "Snenk" }),
        };
        let steps = [MigrationStep {
            from: 1,
            to: 2,
            migrate,
        }];

        let loaded = load_config::<TestSettings>(&original, &steps).unwrap();

        assert!(loaded.value.enabled);
    }

    #[test]
    fn future_data_is_rejected_without_modification() {
        let original = StoredConfig {
            definition: "test.settings".into(),
            version: 3,
            data: json!({ "name": "Snenk", "enabled": true }),
        };

        assert!(matches!(
            load_config::<TestSettings>(&original, &[]),
            Err(MigrationError::FutureVersion {
                stored: 3,
                compiled: 2
            })
        ));
        assert_eq!(original.version, 3);
    }

    #[test]
    fn failed_explicit_migration_preserves_original_payload() {
        fn fail(mut data: Value) -> Result<Value, ManualMigrationError> {
            data["name"] = json!("changed");
            Err(ManualMigrationError::new("cannot migrate"))
        }

        let original = StoredConfig {
            definition: "test.settings".into(),
            version: 1,
            data: json!({ "name": "original", "unknown": { "keep": true } }),
        };
        let before = original.clone();
        let steps = [MigrationStep {
            from: 1,
            to: 2,
            migrate: fail,
        }];

        assert!(matches!(
            load_config::<TestSettings>(&original, &steps),
            Err(MigrationError::Explicit { from: 1, to: 2, .. })
        ));
        assert_eq!(original, before);
    }

    #[test]
    fn migration_retains_unknown_payload_fields() {
        let original = StoredConfig {
            definition: "test.settings".into(),
            version: 1,
            data: json!({ "name": "original", "extension": { "values": [1, 2, 3] } }),
        };

        let loaded = load_config::<TestSettings>(&original, &[]).unwrap();

        assert_eq!(loaded.stored.data["extension"], original.data["extension"]);
        assert_eq!(loaded.stored.data["enabled"], false);
    }

    #[test]
    fn unavailable_definition_is_rejected_without_modification() {
        let original = StoredConfig {
            definition: "unavailable.settings".into(),
            version: 1,
            data: json!({ "unknown": [true, "keep"] }),
        };
        let before = original.clone();

        assert!(matches!(
            load_config::<TestSettings>(&original, &[]),
            Err(MigrationError::DefinitionMismatch { .. })
        ));
        assert_eq!(original, before);
    }

    #[test]
    fn explicit_migration_can_skip_intermediate_versions() {
        #[derive(Debug, Default, Deserialize, Serialize, ConfigSchema)]
        #[config(id = "test.jump", version = 3, title = "Jump")]
        struct Settings {
            #[config(id = "name", label = "Name", introduced = 3)]
            name: String,
        }

        fn rename(mut data: Value) -> Result<Value, ManualMigrationError> {
            let object = data.as_object_mut().unwrap();
            let name = object.remove("old_name").unwrap();
            object.insert("name".into(), name);
            Ok(data)
        }

        let original = StoredConfig {
            definition: "test.jump".into(),
            version: 1,
            data: json!({ "old_name": "kept", "extension": 42 }),
        };
        let loaded = load_config::<Settings>(
            &original,
            &[MigrationStep {
                from: 1,
                to: 3,
                migrate: rename,
            }],
        )
        .unwrap();

        assert_eq!(loaded.value.name, "kept");
        assert_eq!(loaded.stored.version, 3);
        assert_eq!(
            loaded.stored.data,
            json!({ "name": "kept", "extension": 42 })
        );
        assert_eq!(original.version, 1);
    }
}
