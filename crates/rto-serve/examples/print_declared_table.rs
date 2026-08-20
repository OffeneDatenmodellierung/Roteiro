//! Print the declared OpenAI parameter table as `docs/SERVING.md` carries it.
//!
//! `docs/SERVING.md`'s parameter table is generated from
//! `rto_serve::openai_params::OPENAI_CHAT_PARAMS` and asserted back against it
//! by `the_published_table_is_this_table`. When that test fails because a row
//! was added or its wording changed, this is how the document is brought back
//! into line rather than by hand:
//!
//! ```text
//! cargo run -p rto-serve --example print_declared_table
//! ```

fn main() {
    print!("{}", rto_serve::openai_params::published_table());
}
