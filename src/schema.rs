use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ConfigFieldKind {
    Text,
    Secret,
    Integer,
    Toggle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConfigField {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub introduced_in: u32,
    pub kind: ConfigFieldKind,
    pub required: bool,
    pub choice_source: Option<ConfigChoiceSource>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConfigChoiceSource {
    /// Namespaced key supplied by a compiled integration, such as `obs.scenes`.
    pub key: &'static str,
    /// Input field whose current value narrows the available choices.
    pub depends_on: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ConfigOutputKind {
    Text,
    Number,
    Toggle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConfigOutput {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: ConfigOutputKind,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConfigSchema {
    pub id: &'static str,
    pub version: u32,
    pub title: &'static str,
    pub fields: &'static [ConfigField],
    pub outputs: &'static [ConfigOutput],
}

pub trait DescribeConfig: Default + Serialize {
    const SCHEMA: &'static ConfigSchema;
}

/// Module-owned metadata and runtime values for a workflow trigger payload.
pub trait DescribeValues {
    const OUTPUTS: &'static [ConfigOutput];

    fn values(&self) -> crate::engine::Values;
}

/// Editor defaults come from the same Rust input type used by the capability.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ActionDefinition {
    pub schema: &'static ConfigSchema,
    #[cfg_attr(feature = "desktop-contracts", specta(type = std::collections::BTreeMap<String, specta_typescript::Unknown>))]
    pub defaults: crate::engine::Values,
}

impl ActionDefinition {
    pub fn of<T: DescribeConfig>() -> Self {
        let serialized = serde_json::to_value(T::default())
            .expect("compiled action defaults must be serializable");
        let object = serialized
            .as_object()
            .expect("compiled action inputs must be an object");
        let defaults = T::SCHEMA
            .fields
            .iter()
            .filter(|field| field.kind != ConfigFieldKind::Secret)
            .filter_map(|field| {
                object
                    .get(field.id)
                    .filter(|value| !value.is_null())
                    .map(|value| (field.id.to_owned(), value.clone()))
            })
            .collect();
        Self {
            schema: T::SCHEMA,
            defaults,
        }
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize, crate::ConfigSchema)]
    #[config(id = "test.defaults", version = 1, title = "Defaults")]
    struct Inputs {
        #[config(id = "enabled", label = "Enabled", introduced = 1)]
        enabled: bool,
        #[config(id = "count", label = "Count", introduced = 1)]
        count: u32,
        #[config(id = "name", label = "Name", introduced = 1)]
        name: String,
        #[config(id = "optional", label = "Optional", introduced = 1)]
        optional: Option<String>,
        #[config(id = "secret", label = "Secret", introduced = 1, secret)]
        secret: String,
    }

    impl Default for Inputs {
        fn default() -> Self {
            Self {
                enabled: true,
                count: 42,
                name: "Hello".into(),
                optional: None,
                secret: "private".into(),
            }
        }
    }

    #[test]
    fn defaults_follow_the_input_type_and_omit_absent_and_secret_values() {
        let definition = ActionDefinition::of::<Inputs>();
        assert_eq!(
            definition.defaults,
            crate::engine::Values::from([
                ("enabled".into(), serde_json::json!(true)),
                ("count".into(), serde_json::json!(42)),
                ("name".into(), serde_json::json!("Hello")),
            ])
        );
        assert!(
            !serde_json::to_string(&definition)
                .unwrap()
                .contains("private")
        );
        assert_eq!(definition.schema.id, Inputs::SCHEMA.id);
    }
}
