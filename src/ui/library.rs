use slint::{Model, ModelRc, VecModel};

use super::{AppWindow, AutomationRow};

pub fn connect_library(window: &AppWindow) {
    window.on_filter_automations(|rows, query| {
        let query = query.trim().to_lowercase();
        let filtered: Vec<_> = rows
            .iter()
            .filter(|row: &AutomationRow| {
                row.title.to_lowercase().contains(&query)
                    || row.trigger_summary.to_lowercase().contains(&query)
            })
            .collect();
        ModelRc::new(VecModel::from(filtered))
    });
}
