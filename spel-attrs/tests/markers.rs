//! `#[message]`, `#[state]` and `#[event]` leave their item exactly as written.

use spel_attrs::{event, message, state};

#[message]
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Increment { by: u64 },
    Close,
}

#[spel_attrs::message]
enum Internal {
    Ping,
}

/// Docs and other attributes stay.
#[state]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounterState {
    Count(u64),
    Closed,
}

#[derive(Debug, Default, PartialEq, Eq)]
#[state]
pub(crate) struct Observations {
    prices: Vec<u64>,
}

#[spel_attrs::state]
struct Tick(u32);

#[event]
pub trait CounterEvent {
    const NAME: &'static str;

    fn name(&self) -> &'static str {
        Self::NAME
    }
}

pub struct Incremented;

impl CounterEvent for Incremented {
    const NAME: &'static str = "Incremented";
}

/// # Safety
/// Nothing to uphold: it checks the marker accepts an `unsafe trait`.
#[spel_attrs::event]
unsafe trait Raw {}

unsafe impl Raw for Incremented {}

const fn is_raw<T: Raw>(_: &T) -> bool {
    true
}

#[test]
fn marked_types_are_unchanged() {
    assert_eq!(CounterState::Count(3).clone(), CounterState::Count(3));
    assert_ne!(CounterState::Closed, CounterState::Count(0));
    assert_eq!(Observations::default().prices, Vec::<u64>::new());
    assert_eq!(Tick(7).0, 7);
    assert_ne!(Message::Increment { by: 1 }, Message::Close);
    assert!(matches!(Internal::Ping, Internal::Ping));
    assert_eq!(Incremented.name(), "Incremented");
    assert!(is_raw(&Incremented));
}
