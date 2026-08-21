//! Measure an advertised tool surface: the wire split, and the prompt bytes it
//! renders to.
//!
//! Issue #590 found that the mass of what `serve` sends on every turn was
//! discoverable only by hand — nothing pinned the rendering, and the
//! description-versus-schema split had been estimated rather than measured. This
//! is the instrument, so a claim about the surface can be re-derived instead of
//! quoted:
//!
//! ```text
//! cargo run -p rto-serve --example advertised_surface -- tools.json
//! ```
//!
//! `tools.json` is an OpenAI `tools` array — a client's, or one dumped from
//! Roteiro's own graph registry. At the 3.13 ms/token prefill measured in #578,
//! the last number it prints is seconds per turn.

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: advertised_surface <tools.json>   (an OpenAI `tools` array)");
        return ExitCode::FAILURE;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("reading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let specs: Vec<serde_json::Value> = match serde_json::from_str(&text) {
        Ok(specs) => specs,
        Err(e) => {
            eprintln!("parsing {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut tools = Vec::new();
    let (mut names, mut descriptions, mut schemas) = (0usize, 0usize, 0usize);
    for spec in &specs {
        let function = spec.get("function").unwrap_or(spec);
        let name = function
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let description = function
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let parameters = function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        names += name.len();
        descriptions += description.len();
        schemas += serde_json::to_string(&parameters).map_or(0, |s| s.len());
        tools.push(rto_serve::ToolDef {
            name,
            description,
            parameters,
        });
    }

    // The wire bytes — the same sum `crate::types` bounds a client's array by,
    // and the one an MCP `tools/list` hands over.
    let wire = names + descriptions + schemas;
    let pct = |part: usize| (part * 100).checked_div(wire).unwrap_or(0);
    println!("tools:            {}", tools.len());
    println!("wire bytes:       {wire}");
    println!("  names:          {names} ({}%)", pct(names));
    println!("  descriptions:   {descriptions} ({}%)", pct(descriptions));
    println!("  schemas:        {schemas} ({}%)", pct(schemas));

    // And the prompt those definitions actually become, which is what a model
    // prefills. Larger than the wire sum by the preamble, smaller by the
    // rendering.
    let prompt = rto_serve::advertised_system_prompt(&tools.iter().collect::<Vec<_>>());
    println!("rendered prompt:  {} bytes", prompt.len());
    ExitCode::SUCCESS
}
