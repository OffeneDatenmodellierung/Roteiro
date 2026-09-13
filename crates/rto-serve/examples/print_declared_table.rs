//! Print a declared parameter table as `docs/SERVING.md` carries it.
//!
//! `docs/SERVING.md` carries two generated tables — the chat one from
//! `rto_serve::openai_params::OPENAI_CHAT_PARAMS` and the Responses one from
//! `rto_serve::responses::RESPONSES_PARAMS` — each asserted back against the
//! document by its own test. When one of those tests fails because a row was
//! added or its wording changed, this is how the document is brought back into
//! line rather than by hand:
//!
//! ```text
//! cargo run -p rto-serve --example print_declared_table
//! cargo run -p rto-serve --example print_declared_table -- responses
//! ```

fn main() {
    let which = std::env::args().nth(1).unwrap_or_default();
    if which == "responses" {
        print!("{}", rto_serve::responses::published_table());
    } else {
        print!("{}", rto_serve::openai_params::published_table());
    }
}
