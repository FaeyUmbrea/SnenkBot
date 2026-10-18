//! Direct test delivery through explicitly registered output capabilities.

use serde::{Deserialize, Serialize};
use snenk_bot::{app::AppServices, engine::Values, event_testing::EventTestSender};
use specta::Type;

use crate::events::{EventCommandError, EventCommandErrorCode};

/// One named recipient with the sender channels it is permitted to receive.
#[derive(Clone, Debug, Serialize, Type)]
pub struct EventTestConsumer {
    pub id: String,
    pub name: String,
    pub channels: Vec<String>,
}

/// Available registered test senders and current named overlay consumers.
#[derive(Clone, Debug, Serialize, Type)]
pub struct EventTestOptions {
    pub senders: Vec<EventTestSender>,
    pub consumers: Vec<EventTestConsumer>,
}

/// Literal test inputs and one explicit recipient; the native layer owns recipient injection.
#[derive(Clone, Debug, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct SendTestEvent {
    pub action_id: String,
    pub version: u32,
    pub consumer_id: String,
    #[specta(type = std::collections::BTreeMap<String, specta_typescript::Unknown>)]
    pub values: Values,
}

/// Loads the safe consumer picker and sender schemas without tokens or workflow state.
pub fn options(services: &AppServices) -> Result<EventTestOptions, EventCommandError> {
    let consumers = services
        .api()
        .list_consumers()
        .map_err(|_| EventCommandError::unavailable())?
        .into_iter()
        .map(|consumer| EventTestConsumer {
            id: consumer.id,
            name: consumer.name,
            channels: consumer.permissions.channels.into_iter().collect(),
        })
        .collect();
    Ok(EventTestOptions {
        senders: services.event_tests().senders(),
        consumers,
    })
}

/// Checks current grants and delegates payload validation and delivery to the ordinary capability.
pub async fn send(
    services: &AppServices,
    mut request: SendTestEvent,
) -> Result<(), EventCommandError> {
    select_recipient(&options(services)?, &mut request)?;
    services
        .event_tests()
        .send(&request.action_id, request.version, request.values)
        .await
        .map_err(|message| invalid(&message))
}

/// Injects only the selected consumer after checking the current sender and grants.
fn select_recipient(
    options: &EventTestOptions,
    request: &mut SendTestEvent,
) -> Result<(), EventCommandError> {
    let sender = options
        .senders
        .iter()
        .find(|sender| sender.definition.schema.id == request.action_id)
        .ok_or_else(|| invalid("This event sender is unavailable."))?;
    let consumer = options
        .consumers
        .iter()
        .find(|consumer| consumer.id == request.consumer_id)
        .ok_or_else(|| invalid("This consumer is no longer available."))?;
    if !consumer.channels.contains(&sender.channel) {
        return Err(invalid("This consumer cannot receive the selected event."));
    }
    request.values.insert(
        sender.target_field.clone(),
        serde_json::json!([consumer.id]),
    );
    Ok(())
}

/// Produces the shared safe event error shape for a rejected direct send.
fn invalid(message: &str) -> EventCommandError {
    EventCommandError::new(EventCommandErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snenk_bot::schema::{ActionDefinition, ConfigSchema};

    /// Native target selection cannot be replaced by targets supplied through editable inputs.
    #[test]
    fn selected_consumer_overrides_supplied_targets_and_requires_current_grants() {
        static SCHEMA: ConfigSchema = ConfigSchema {
            id: "future.send",
            version: 1,
            title: "Future event",
            fields: &[],
            outputs: &[],
        };
        let mut catalog = EventTestOptions {
            senders: vec![EventTestSender {
                definition: ActionDefinition {
                    schema: &SCHEMA,
                    defaults: Values::new(),
                },
                target_field: "targets".into(),
                channel: "future.channel".into(),
            }],
            consumers: vec![EventTestConsumer {
                id: "alice".into(),
                name: "Main overlay".into(),
                channels: vec!["future.channel".into()],
            }],
        };
        let mut request = SendTestEvent {
            action_id: "future.send".into(),
            version: 1,
            consumer_id: "alice".into(),
            values: Values::from([("targets".into(), json!(["bob"]))]),
        };
        select_recipient(&catalog, &mut request).unwrap();
        assert_eq!(request.values["targets"], json!(["alice"]));
        catalog.consumers[0].channels.clear();
        assert!(select_recipient(&catalog, &mut request).is_err());
        request.consumer_id = "removed".into();
        assert!(select_recipient(&catalog, &mut request).is_err());
        request.action_id = "unsafe.action".into();
        assert!(select_recipient(&catalog, &mut request).is_err());
    }
}
