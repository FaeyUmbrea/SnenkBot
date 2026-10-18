//! Selectable values supplied by compiled integrations for schema-backed editors.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::schema::ConfigSchema;

pub type ChoiceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ConfigChoice>, String>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct ConfigChoice {
    /// Stable stored value. Labels may change without retargeting saved workflows.
    pub value: String,
    pub label: String,
    pub detail: Option<String>,
}

pub trait ChoiceProvider: Send + Sync {
    fn choices<'a>(&'a self, source: &'a str, depends_on: Option<&'a str>) -> ChoiceFuture<'a>;
}

#[derive(Default)]
pub struct ChoiceCatalog {
    providers: BTreeMap<&'static str, Arc<dyn ChoiceProvider>>,
}

impl ChoiceCatalog {
    pub fn register(&mut self, source: &'static str, provider: Arc<dyn ChoiceProvider>) {
        assert!(
            self.providers.insert(source, provider).is_none(),
            "duplicate choice source: {source}"
        );
    }

    pub async fn choices(
        &self,
        source: &str,
        depends_on: Option<&str>,
    ) -> Result<Vec<ConfigChoice>, String> {
        let provider = self
            .providers
            .get(source)
            .ok_or_else(|| format!("choice source `{source}` is unavailable"))?;
        provider.choices(source, depends_on).await
    }

    /// Catch a compiled schema that advertises choices without a registered provider.
    pub fn validate_schemas(&self, schemas: &[&ConfigSchema]) -> Result<(), String> {
        for schema in schemas {
            for field in schema.fields {
                if let Some(source) = field.choice_source
                    && !self.providers.contains_key(source.key)
                {
                    return Err(format!(
                        "action `{}` field `{}` has unavailable choice source `{}`",
                        schema.id, field.id, source.key
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoChoices;

    impl ChoiceProvider for EchoChoices {
        fn choices<'a>(&'a self, source: &'a str, depends_on: Option<&'a str>) -> ChoiceFuture<'a> {
            Box::pin(async move {
                Ok(vec![ConfigChoice {
                    value: depends_on.unwrap_or_default().into(),
                    label: source.into(),
                    detail: None,
                }])
            })
        }
    }

    #[tokio::test]
    async fn registered_sources_keep_dependency_values_and_reject_unknown_sources() {
        let mut catalog = ChoiceCatalog::default();
        catalog.register("test.items", Arc::new(EchoChoices));
        assert_eq!(
            catalog
                .choices("test.items", Some("parent-id"))
                .await
                .unwrap(),
            vec![ConfigChoice {
                value: "parent-id".into(),
                label: "test.items".into(),
                detail: None,
            }]
        );
        assert!(catalog.choices("missing", None).await.is_err());
    }
}
