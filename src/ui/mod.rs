use super::*;

static PRESENTATION_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_presentation_id() -> u64 {
    PRESENTATION_ID.fetch_add(1, Ordering::AcqRel) + 1
}

pub(crate) fn current_presentation_id() -> u64 {
    PRESENTATION_ID.load(Ordering::Acquire)
}

mod application_callbacks;
mod background_operations;
mod callbacks;
mod crash_callbacks;
mod diagnostic_callbacks;
mod event_details;
mod finance;
mod notification_callbacks;
mod presentation;
mod runtime_bridge;
mod secondary_callbacks;
mod settings;
mod settings_callbacks;
mod task_callbacks;
mod tasks;
mod update_callbacks;
mod window_state;
mod workers;

#[allow(unused_imports)]
pub(crate) use {
    application_callbacks::*, background_operations::*, callbacks::*, crash_callbacks::*,
    diagnostic_callbacks::*, event_details::*, finance::*, notification_callbacks::*,
    presentation::*, runtime_bridge::*, secondary_callbacks::*, settings::*, settings_callbacks::*,
    task_callbacks::*, tasks::*, update_callbacks::*, window_state::*, workers::*,
};

#[cfg(test)]
mod tests;
