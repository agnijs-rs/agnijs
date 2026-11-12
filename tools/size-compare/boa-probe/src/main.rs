//! Boa in the size probe: one default context, the command-line script
//! evaluated, its completion value printed.

use boa_engine::{Context, Source};

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    let mut ctx = Context::default();
    match ctx.eval(Source::from_bytes(src.as_bytes())) {
        Ok(v) => println!("{}", v.display()),
        Err(e) => println!("error: {e}"),
    }
}
