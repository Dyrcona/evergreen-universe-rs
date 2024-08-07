//! Base module for A/T Reactors
use crate::common::trigger::{Event, EventState, Processor};
use crate::result::EgResult;

mod circ;

/// Add reactor routines to the Processor.
impl Processor<'_> {
    /// React to one or more events.
    ///
    /// Multiple Events implies a linked event group.
    ///
    /// Reactors in Perl return true/false to indicate success,
    /// but the return value doesn't appear to be used, just the
    /// event state.
    pub fn react(&mut self, events: &mut [&mut Event]) -> EgResult<()> {
        let event_ids: Vec<String> = events.iter().map(|e| e.id().to_string()).collect();

        log::info!("{self} reacting to events [{}]", event_ids.join(","));

        if events.is_empty() {
            return Ok(());
        }

        for event in events.iter_mut() {
            self.set_event_state(event, EventState::Reacting)?;
        }

        let reactor = self.reactor();

        log::debug!(
            "{self} reacting with '{reactor}' on {} event(s)",
            events.len()
        );

        let react_result = match reactor {
            "NOOP_True" => Ok(()),
            "NOOP_False" => Err("NOOP_False".to_string().into()),
            "Circ::AutoRenew" => self.autorenew(events),
            _ => Err(format!("No such reactor: {reactor}").into()),
        };

        if react_result.is_ok() {
            for event in events.iter_mut() {
                self.set_event_state(event, EventState::Reacted)?;
            }
        }

        react_result
    }
}
