/// Prints the program IDL JSON to stdout.
/// Used by the e2e test runner to extract the IDL for validation.
fn main() {
    println!("{}", fixture_program::PROGRAM_IDL_JSON);
}
