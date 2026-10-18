//! Explicitly registered output senders for transient overlay testing.

use std::{collections::BTreeMap, sync::Arc};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{
    engine::{Capability, CapabilityError, Input, Values},
    schema::ActionDefinition,
};

/// An integration-owned test sender using its ordinary compiled action inputs.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "desktop-contracts", derive(specta::Type))]
pub struct EventTestSender {
    pub definition: ActionDefinition,
    pub target_field: String,
    pub channel: String,
}

/// Registered output-only capabilities available without creating or running a workflow.
#[derive(Clone, Default)]
pub struct EventTestCatalog {
    entries: BTreeMap<(String, u32), (EventTestSender, Arc<dyn Capability>)>,
}

impl EventTestCatalog {
    /// Adds an explicitly eligible sender with the same definition and execution handler.
    pub fn register(&mut self, sender: EventTestSender, capability: Arc<dyn Capability>) {
        assert!(
            sender
                .definition
                .schema
                .fields
                .iter()
                .any(|field| field.id == sender.target_field)
        );
        assert!(
            self.entries
                .insert(
                    (
                        sender.definition.schema.id.into(),
                        sender.definition.schema.version
                    ),
                    (sender, capability)
                )
                .is_none()
        );
    }

    /// Returns the latest registered version of each sender without execution handles.
    pub fn senders(&self) -> Vec<EventTestSender> {
        self.entries
            .values()
            .map(|(sender, _)| (sender.definition.schema.id, sender.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect()
    }

    /// Validates literal inputs and executes the registered output capability within its deadline.
    pub async fn send(&self, id: &str, version: u32, values: Values) -> Result<(), String> {
        let (_, capability) = self
            .entries
            .get(&(id.to_owned(), version))
            .ok_or("This event sender or version is unavailable. Reopen event testing.")?;
        let inputs = values
            .iter()
            .map(|(key, value)| (key.clone(), Input::Literal(value.clone())))
            .collect();
        capability.validate_inputs(&inputs)?;
        capability.ready()?;
        let cancel = CancellationToken::new();
        let result = tokio::time::timeout(
            capability.default_deadline(),
            capability.execute(values, cancel.clone()),
        )
        .await;
        cancel.cancel();
        match result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(
                CapabilityError::ConnectorUnavailable(message)
                | CapabilityError::Failed(message)
                | CapabilityError::Uncertain(message),
            )) => Err(message),
            Err(_) => Err(
                "Sending the test event timed out. Check the overlay before trying again.".into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ConfigField, ConfigFieldKind, ConfigSchema};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    const SCHEMA: ConfigSchema = ConfigSchema {
        id: "future.sender",
        version: 1,
        title: "Future sender",
        outputs: &[],
        fields: &[ConfigField {
            id: "targets",
            label: "Targets",
            description: "",
            introduced_in: 1,
            kind: ConfigFieldKind::StringSet,
            required: true,
            choice_source: None,
        }],
    };

    /// Mock output capability counting executions after literal validation.
    struct Sender(AtomicUsize);
    impl Capability for Sender {
        fn default_deadline(&self) -> Duration {
            Duration::from_secs(1)
        }
        fn validate_inputs(&self, inputs: &BTreeMap<String, Input>) -> Result<(), String> {
            crate::engine::validate_configured_inputs(&SCHEMA, inputs)
        }
        fn execute<'a>(
            &'a self,
            _inputs: Values,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Values, CapabilityError>> + Send + 'a>,
        > {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Values::new())
            })
        }
    }

    #[tokio::test]
    async fn only_registered_matching_valid_senders_execute() {
        let capability = Arc::new(Sender(AtomicUsize::new(0)));
        let mut catalog = EventTestCatalog::default();
        catalog.register(
            EventTestSender {
                definition: ActionDefinition {
                    schema: &SCHEMA,
                    defaults: Values::new(),
                },
                target_field: "targets".into(),
                channel: "future.channel".into(),
            },
            capability.clone(),
        );
        let values = BTreeMap::from([("targets".into(), serde_json::json!(["overlay"]))]);
        assert!(
            catalog
                .send("live.side_effect", 1, values.clone())
                .await
                .is_err()
        );
        assert!(
            catalog
                .send("future.sender", 2, values.clone())
                .await
                .is_err()
        );
        assert!(
            catalog
                .send("future.sender", 1, Values::new())
                .await
                .is_err()
        );
        assert_eq!(capability.0.load(Ordering::SeqCst), 0);
        catalog.send("future.sender", 1, values).await.unwrap();
        assert_eq!(capability.0.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.senders()[0].target_field, "targets");
        const NEXT_SCHEMA: ConfigSchema = ConfigSchema {
            version: 2,
            ..SCHEMA
        };
        catalog.register(
            EventTestSender {
                definition: ActionDefinition {
                    schema: &NEXT_SCHEMA,
                    defaults: Values::new(),
                },
                target_field: "targets".into(),
                channel: "future.channel".into(),
            },
            capability.clone(),
        );
        assert_eq!(catalog.senders().len(), 1);
        assert_eq!(catalog.senders()[0].definition.schema.version, 2);
        catalog
            .send(
                "future.sender",
                2,
                BTreeMap::from([("targets".into(), serde_json::json!(["overlay"]))]),
            )
            .await
            .unwrap();
        assert_eq!(capability.0.load(Ordering::SeqCst), 2);
    }
}
