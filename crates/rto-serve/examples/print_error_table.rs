//! Print the error table as `docs/SERVING.md` carries it.
//!
//! Sibling of `print_declared_table`: the document publishes what
//! [`rto_serve::server::published_error_table`] generates, and
//! `server::tests::the_published_error_table_is_this_table` fails when the two
//! drift. Run this and paste the output over the block to fix that.
fn main() {
    print!("{}", rto_serve::server::published_error_table());
}
