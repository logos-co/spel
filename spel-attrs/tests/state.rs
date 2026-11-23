//! `#[state]` leaves its item exactly as written.

use spel_attrs::state;

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

#[test]
fn marked_types_are_unchanged() {
    assert_eq!(CounterState::Count(3).clone(), CounterState::Count(3));
    assert_ne!(CounterState::Closed, CounterState::Count(0));
    assert_eq!(Observations::default().prices, Vec::<u64>::new());
    assert_eq!(Tick(7).0, 7);
}
