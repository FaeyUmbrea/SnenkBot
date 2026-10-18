use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ConfigFieldKind {
    Text,
    /// A filesystem directory stored as an ordinary configuration string.
    Directory,
    Secret,
    Integer,
    Toggle,
    /// Arbitrary JSON carried through action inputs and configuration fields.
    Json,
    StringList,
    StringSet,
    StringSetMap,
    Object(&'static [ConfigField]),
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

/// Copy for a native credential stored separately from portable configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct CredentialDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub empty_description: &'static str,
    pub stored_description: &'static str,
    pub unavailable_description: &'static str,
    pub save_label: &'static str,
    pub saved_message: &'static str,
    pub save_error: &'static str,
    pub remove_label: Option<&'static str>,
    pub removed_message: Option<&'static str>,
}

/// Copy and saved-setting prerequisite for an integration's native pairing control.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct AuthorizationDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub prerequisite_field: Option<&'static str>,
    pub prerequisite_message: Option<&'static str>,
}

/// A form owner's named-record collection, administered separately from its fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConfigurationCollectionDescriptor {
    pub id: &'static str,
    pub title: &'static str,
    pub item_label: &'static str,
    pub description: &'static str,
}

/// An integration-owned action offered while editing an existing configuration record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConfigurationActionDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub confirmation: Option<&'static str>,
    pub success_message: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ConfigOutputKind {
    Text,
    Number,
    Toggle,
    /// Structured arrays or objects, retained as JSON across workflow and Lua calls.
    Json,
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

/// Settings directory section supplied by an integration's authoritative config type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationCategory {
    General,
    Broadcast,
    Streaming,
    Devices,
}

pub trait DescribeConfig: Default + Serialize {
    const SCHEMA: &'static ConfigSchema;
    const CREDENTIAL: Option<CredentialDescriptor> = None;
    const AUTHORIZATION: Option<AuthorizationDescriptor> = None;
    const COLLECTION: Option<ConfigurationCollectionDescriptor> = None;
    const ACTIONS: &'static [ConfigurationActionDescriptor] = &[];
    const CATEGORY: Option<ConfigurationCategory> = None;
    const INTEGRATION: Option<&'static str> = None;
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
            .filter_map(|field| {
                object
                    .get(field.id)
                    .and_then(|value| public_default(field, value))
                    .map(|value| (field.id.to_owned(), value.clone()))
            })
            .collect();
        Self {
            schema: T::SCHEMA,
            defaults,
        }
    }
}

/// Omits secret values from defaults, including fields inside nested objects.
fn public_default(field: &ConfigField, value: &serde_json::Value) -> Option<serde_json::Value> {
    if value.is_null() || field.kind == ConfigFieldKind::Secret {
        return None;
    }
    let ConfigFieldKind::Object(fields) = field.kind else {
        return Some(value.clone());
    };
    let object = value.as_object()?;
    Some(serde_json::Value::Object(
        fields
            .iter()
            .filter_map(|child| {
                object
                    .get(child.id)
                    .and_then(|value| public_default(child, value))
                    .map(|value| (child.id.to_owned(), value))
            })
            .collect(),
    ))
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

    #[derive(Default, Serialize, crate::ConfigSchema)]
    #[config(id = "test.nested", version = 1, title = "Nested")]
    struct Nested {
        #[config(id = "name", label = "Name", introduced = 1)]
        name: String,
        #[config(id = "secret", label = "Secret", introduced = 1, secret)]
        secret: String,
    }

    #[derive(Default, Serialize, crate::ConfigSchema)]
    #[config(id = "test.collections", version = 1, title = "Collections")]
    struct Collections {
        #[config(id = "nested", label = "Nested", introduced = 1)]
        nested: Nested,
        #[config(id = "list", label = "List", introduced = 1)]
        list: Vec<String>,
        #[config(id = "set", label = "Set", introduced = 1)]
        set: std::collections::BTreeSet<String>,
        #[config(id = "map", label = "Map", introduced = 1)]
        map: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    }

    /// Generated defaults preserve nested objects and collection values without flattening.
    #[test]
    fn nested_fields_and_collections_keep_their_json_shapes() {
        let fields = Collections::SCHEMA.fields;
        assert!(matches!(fields[0].kind, ConfigFieldKind::Object(children) if children.len() == 2));
        assert_eq!(fields[1].kind, ConfigFieldKind::StringList);
        assert_eq!(fields[2].kind, ConfigFieldKind::StringSet);
        assert_eq!(fields[3].kind, ConfigFieldKind::StringSetMap);
        assert_eq!(
            serde_json::to_value(fields[0].kind).unwrap(),
            serde_json::json!({"object": [
                {"id": "name", "label": "Name", "description": "", "introduced_in": 1,
                    "kind": "text", "required": true, "choice_source": null},
                {"id": "secret", "label": "Secret", "description": "", "introduced_in": 1,
                    "kind": "secret", "required": true, "choice_source": null}
            ]})
        );
    }

    /// Nested credential defaults never cross the public schema boundary.
    #[test]
    fn nested_secret_defaults_are_omitted() {
        let default = serde_json::json!({"name": "visible", "secret": "private"});
        assert_eq!(
            public_default(&Collections::SCHEMA.fields[0], &default),
            Some(serde_json::json!({"name": "visible"}))
        );
    }

    /// Literal validation follows nested fields and rejects malformed collection values.
    #[test]
    fn nested_literals_and_string_set_maps_are_validated() {
        let mut inputs = std::collections::BTreeMap::from([
            (
                "nested".to_owned(),
                crate::engine::Input::Literal(
                    serde_json::json!({"name": "visible", "secret": "private"}),
                ),
            ),
            (
                "list".to_owned(),
                crate::engine::Input::Literal(serde_json::json!(["first"])),
            ),
            (
                "set".to_owned(),
                crate::engine::Input::Literal(serde_json::json!(["second"])),
            ),
            (
                "map".to_owned(),
                crate::engine::Input::Literal(serde_json::json!({"provider": ["read"]})),
            ),
        ]);
        assert!(crate::engine::validate_configured_inputs(Collections::SCHEMA, &inputs).is_ok());
        inputs.insert(
            "map".to_owned(),
            crate::engine::Input::Literal(serde_json::json!({"provider": [7]})),
        );
        assert!(crate::engine::validate_configured_inputs(Collections::SCHEMA, &inputs).is_err());
        inputs.insert(
            "map".to_owned(),
            crate::engine::Input::Literal(serde_json::json!({"provider": ["read"]})),
        );
        inputs.insert(
            "nested".to_owned(),
            crate::engine::Input::Literal(serde_json::json!({"name": 7, "secret": "private"})),
        );
        assert!(crate::engine::validate_configured_inputs(Collections::SCHEMA, &inputs).is_err());
    }

    /// Credential metadata serializes using the generated frontend field names.
    #[test]
    fn credential_copy_uses_frontend_field_names() {
        let serialized = serde_json::to_value(crate::obs::settings::PASSWORD_CREDENTIAL).unwrap();
        assert_eq!(
            serialized["emptyDescription"],
            "Optional password for the OBS connection."
        );
        assert!(serialized.get("empty_description").is_none());
        assert!(serialized["removeLabel"].is_null());
    }
}
