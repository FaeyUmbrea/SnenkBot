//! Resolve resource pickers through compiled action metadata, never webview-supplied providers.

use serde::Deserialize;
use snenk_bot::schema::{ConfigChoiceSource, ConfigSchema};
use specta::Type;

use crate::editor::{EditorError, EditorErrorCode};

#[derive(Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ActionChoices {
    pub action_id: String,
    pub field_id: String,
    /// Current literal value of the dependency declared by this field's schema.
    pub depends_on: Option<String>,
}

pub(crate) fn resolve_source(
    schemas: &[&ConfigSchema],
    request: &ActionChoices,
) -> Result<ConfigChoiceSource, EditorError> {
    let source = schemas
        .iter()
        .find(|schema| schema.id == request.action_id)
        .and_then(|schema| {
            schema
                .fields
                .iter()
                .find(|field| field.id == request.field_id)
        })
        .and_then(|field| field.choice_source)
        .ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidEdit,
                "This field does not have an available resource picker.",
            )
        })?;
    if source.depends_on.is_none() && request.depends_on.is_some() {
        return Err(EditorError::new(
            EditorErrorCode::InvalidEdit,
            "This resource picker does not accept a parent value.",
        ));
    }
    if source.depends_on.is_some()
        && request
            .depends_on
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(EditorError::new(
            EditorErrorCode::InvalidEdit,
            "Choose a parent value before loading these resources.",
        ));
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snenk_bot::schema::{ConfigField, ConfigFieldKind};

    const SCHEMA: ConfigSchema = ConfigSchema {
        id: "test.action",
        version: 1,
        title: "Test",
        outputs: &[],
        fields: &[
            ConfigField {
                id: "scene",
                label: "Scene",
                description: "",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: true,
                choice_source: Some(ConfigChoiceSource {
                    key: "test.scenes",
                    depends_on: None,
                }),
            },
            ConfigField {
                id: "item",
                label: "Item",
                description: "",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: true,
                choice_source: Some(ConfigChoiceSource {
                    key: "test.items",
                    depends_on: Some("scene"),
                }),
            },
            ConfigField {
                id: "text",
                label: "Text",
                description: "",
                introduced_in: 1,
                kind: ConfigFieldKind::Text,
                required: true,
                choice_source: None,
            },
        ],
    };

    #[test]
    fn dependent_pickers_require_a_value_before_contacting_an_integration() {
        for depends_on in [None, Some("".into()), Some("  ".into())] {
            let request = ActionChoices {
                action_id: "test.action".into(),
                field_id: "item".into(),
                depends_on,
            };
            let error = resolve_source(&[&SCHEMA], &request).unwrap_err();
            assert_eq!(error.code, EditorErrorCode::InvalidEdit);
            assert_eq!(
                error.message,
                "Choose a parent value before loading these resources."
            );
        }
    }

    #[test]
    fn only_compiled_fields_can_select_providers() {
        let mut request = ActionChoices {
            action_id: "test.action".into(),
            field_id: "scene".into(),
            depends_on: None,
        };
        assert_eq!(
            resolve_source(&[&SCHEMA], &request).unwrap().key,
            "test.scenes"
        );
        request.depends_on = Some("parent".into());
        assert!(resolve_source(&[&SCHEMA], &request).is_err());
        request.field_id = "item".into();
        assert_eq!(
            resolve_source(&[&SCHEMA], &request).unwrap().depends_on,
            Some("scene")
        );
        assert_eq!(request.depends_on.as_deref(), Some("parent"));
        for field in ["text", "missing", "test.items"] {
            request.field_id = field.into();
            assert!(resolve_source(&[&SCHEMA], &request).is_err());
        }
        request.action_id = "missing".into();
        request.field_id = "scene".into();
        assert!(resolve_source(&[&SCHEMA], &request).is_err());
        assert!(
            serde_json::from_value::<ActionChoices>(serde_json::json!({
                "action_id": "test.action", "field_id": "item", "depends_on": null,
                "source": "unregistered"
            }))
            .is_err()
        );
    }
}
