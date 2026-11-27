//! The payroll program: a seat records its treasury, then pays employees from it. A payment is a
//! custody transfer out of the treasury's native balance, authorized by the seat's seed, so it
//! reaches an actor no field names (the treasury's balance) and credits the employee.

use lee_core::native_token::custody_transfer;
use lee_core::program::{PdaSeed, ReceiveInput, Response};
use payroll_core::{Message, Seat};

/// The handler the guest runs.
///
/// # Panics
/// If the seat doesn't authorize, opens twice, pays before opening, or the total paid would
/// overflow. A panic refuses the message.
pub fn handle_message(input: &ReceiveInput, message: Message) -> Response {
    assert!(input.is_authorized, "the seat must authorize");
    match message {
        Message::Open { treasury } => {
            assert!(input.pre_state.is_empty(), "the seat is already open");
            Response::set_state(encode(&Seat { treasury, paid: 0 }))
        },
        Message::Pay { employee, amount } => {
            assert!(!input.pre_state.is_empty(), "open the seat first");
            let mut seat: Seat = borsh::from_slice(&input.pre_state).expect("seat state must decode");
            seat.paid = seat.paid.checked_add(amount).expect("total paid overflows");
            let seed = PdaSeed::new(input.receiver.account_id.into_value());
            let transfer = custody_transfer(seat.treasury, seed, employee, amount);
            Response::set_state(encode(&seat)).send(transfer)
        },
    }
}

fn encode(seat: &Seat) -> Vec<u8> {
    borsh::to_vec(seat).expect("borsh serialization is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lee_core::account::{AccountId, Actor, ActorState};

    const SEAT: AccountId = AccountId::new([1; 32]);
    const TREASURY: AccountId = AccountId::new([2; 32]);
    const EMPLOYEE: AccountId = AccountId::new([3; 32]);

    fn run(is_authorized: bool, seat: Option<Seat>, message: Message) -> Response {
        let input = ReceiveInput {
            receiver: Actor::new(SEAT, AccountId::new([9; 32])),
            from: None,
            is_authorized,
            pre_state: ActorState::from(seat.as_ref().map(encode).unwrap_or_default()),
            message: borsh::to_vec(&message).unwrap(),
        };
        handle_message(&input, message)
    }

    fn open(paid: u128) -> Seat {
        Seat { treasury: TREASURY, paid }
    }

    #[test]
    fn open_records_the_treasury() {
        let response = run(true, None, Message::Open { treasury: TREASURY });
        assert_eq!(response, Response::set_state(encode(&open(0))));
    }

    #[test]
    fn pay_transfers_out_of_the_treasury_under_the_seats_seed() {
        let response = run(true, Some(open(5)), Message::Pay { employee: EMPLOYEE, amount: 7 });
        let transfer = custody_transfer(TREASURY, PdaSeed::new([1; 32]), EMPLOYEE, 7);
        assert_eq!(response, Response::set_state(encode(&open(12))).send(transfer));
    }

    #[test]
    #[should_panic(expected = "must authorize")]
    fn an_unauthorized_seat_is_refused() {
        let _refused = run(false, Some(open(0)), Message::Pay { employee: EMPLOYEE, amount: 1 });
    }

    #[test]
    #[should_panic(expected = "already open")]
    fn a_seat_opens_once() {
        let _refused = run(true, Some(open(0)), Message::Open { treasury: TREASURY });
    }

    #[test]
    #[should_panic(expected = "open the seat first")]
    fn paying_needs_an_open_seat() {
        let _refused = run(true, None, Message::Pay { employee: EMPLOYEE, amount: 1 });
    }
}
